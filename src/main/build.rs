use std::path::Path;

#[cfg(unix)]
use std::os::fd::AsRawFd;

use crate::resolve_path;
use mimi::ast::Item;
use mimi::codegen;
use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::{lexer, loader, verifier};

/// Extract the OS component from a target triple (e.g. "x86_64-pc-windows-gnu" -> "windows")
fn target_os(triple: &str) -> &str {
    triple.split('-').nth(2).unwrap_or("linux")
}

/// Determine output file extension based on target triple and shared flag.
fn output_extension(target: Option<&str>, shared: bool) -> &'static str {
    let Some(triple) = target else {
        return if shared { ".so" } else { "" };
    };
    match (target_os(triple), shared) {
        ("windows", true) => ".dll",
        ("windows", false) => ".exe",
        ("darwin", true) => ".dylib",
        ("darwin", false) => "",
        (_, true) => ".so",
        (_, false) => "",
    }
}

/// Map a target triple to a cross-compiler/linker command.
/// Returns `None` when the target matches the host (native compilation).
fn target_linker(target: Option<&str>) -> Option<String> {
    let triple = target?;
    let parts: Vec<&str> = triple.split('-').collect();
    if parts.len() < 3 {
        return None;
    }
    let arch = parts[0];
    let os = parts[2];
    let env = parts.get(3).copied().unwrap_or("");
    let prefix = match (arch, os, env) {
        ("x86_64", "windows", "gnu") => "x86_64-w64-mingw32",
        ("i686", "windows", "gnu") => "i686-w64-mingw32",
        ("aarch64", "windows", "gnu") => "aarch64-w64-mingw32",
        ("x86_64", "windows", "msvc") => "x86_64-w64-mingw32",
        ("aarch64", "linux", _) => "aarch64-linux-gnu",
        ("arm", "linux", "gnueabihf") => "arm-linux-gnueabihf",
        ("riscv64", "linux", _) => "riscv64-linux-gnu",
        ("x86_64", "darwin", _) => "x86_64-apple-darwin20",
        ("aarch64", "darwin", _) => "aarch64-apple-darwin20",
        _ => return None,
    };
    Some(format!("{}-gcc", prefix))
}

/// Compute extra linker flags for a given target triple.
fn target_linker_flags(target: Option<&str>) -> Vec<&'static str> {
    let Some(triple) = target else { return vec![] };
    let mut flags = Vec::new();
    if target_os(triple) == "windows" {
        flags.push("-lws2_32");
    }
    flags
}

/// ASan verification channel. When `MIMI_ASAN` is present, the runtime staticlib
/// is built with `-Z sanitizer=address` (using the nightly toolchain) and the
/// final `cc` link adds `-fsanitize=address`, so AddressSanitizer instruments the
/// Mimi runtime heap and catches UAF / OOB / double-free in native-compiled Mimi
/// programs. The host `mimi` binary is unaffected; only the spawned runtime build
/// and the produced executable opt in. Never set in normal builds.
fn asan_enabled() -> bool {
    std::env::var_os("MIMI_ASAN").is_some()
}

fn asan_rustc_flags() -> Vec<&'static str> {
    if asan_enabled() {
        vec!["-Z", "sanitizer=address"]
    } else {
        vec![]
    }
}

fn asan_link_flags() -> Vec<&'static str> {
    if asan_enabled() {
        vec!["-fsanitize=address"]
    } else {
        vec![]
    }
}

#[cfg(unix)]
fn cached_native_runtime(runtime_rs: &Path) -> Result<std::path::PathBuf, String> {
    let runtime_dir = runtime_rs
        .parent()
        .ok_or_else(|| "runtime source has no parent directory".to_string())?;
    let mut files = std::fs::read_dir(runtime_dir)
        .map_err(|e| format!("read runtime directory: {e}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect::<Vec<_>>();
    files.push(runtime_rs.to_path_buf());
    files.sort();

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mimi-native-runtime-v1\0");
    if asan_enabled() {
        // Invalidate the cache for ASan builds so a non-ASan runtime is never
        // reused for an ASan-instrumented link.
        hasher.update(b"asan\0");
    }
    for path in files {
        hasher.update(path.to_string_lossy().as_bytes());
        let contents =
            std::fs::read(&path).map_err(|e| format!("read runtime file {path:?}: {e}"))?;
        hasher.update(&contents);
    }
    let key = hasher.finalize().to_hex();
    let cache_dir = std::env::temp_dir().join("mimi_runtime_build_cache");
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("create runtime cache: {e}"))?;
    let cache_path = cache_dir.join(format!("libmimi_runtime_{key}.a"));
    let lock_path = cache_dir.join("build.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open runtime cache lock: {e}"))?;
    // SAFETY: lock_file is an open regular file and flock only changes its
    // advisory lock state; no Rust references cross the FFI boundary.
    unsafe {
        if libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) != 0 {
            return Err(format!(
                "lock runtime cache: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    if cache_path.exists() {
        return Ok(cache_path);
    }

    let tmp_path = cache_dir.join(format!("libmimi_runtime_{key}.tmp-{}", std::process::id()));
    let mut rt_cmd = std::process::Command::new("rustc");
    rt_cmd
        .args([
            "--edition",
            "2021",
            "--crate-type",
            "staticlib",
            "--cfg",
            "standalone",
        ])
        .args(["--crate-name", "mimi_runtime", "-A", "dead_code"])
        .args(asan_rustc_flags())
        .arg("-o")
        .arg(&tmp_path)
        .arg(runtime_rs);
    if asan_enabled() {
        // `-Z sanitizer=address` requires the nightly compiler; the host `mimi`
        // may have been built with stable, so pin the spawned rustc to nightly.
        rt_cmd.env("RUSTUP_TOOLCHAIN", "nightly");
    }
    let status = rt_cmd
        .status()
        .map_err(|e| format!("runtime compile (rustc): {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err("Rust runtime compilation failed".into());
    }
    std::fs::rename(&tmp_path, &cache_path).map_err(|e| format!("publish runtime cache: {e}"))?;
    Ok(cache_path)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build(
    path: Option<&Path>,
    output: Option<&Path>,
    emit_ir: bool,
    strict: bool,
    no_std: bool,
    verify_contracts: bool,
    verify_ffi: bool,
    shared: bool,
    target: Option<&str>,
) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = mimi::path_safety::read_source_capped(&path)?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let (file, parse_errors) = loader::parser_for_path(tokens, &path)?.parse_file_with_recovery();
    if !parse_errors.is_empty() {
        let use_color = colors_enabled();
        let src_ref = Some(source.as_str());
        let filename = &path.display().to_string();
        for e in &parse_errors {
            let formatted = format_diagnostic(&e.to_diagnostic(), src_ref, filename);
            if use_color {
                eprint!("{}", formatted);
            } else {
                eprint!("{}", strip_ansi(&formatted));
            }
        }
        return Err(format!("{} parse error(s) found", parse_errors.len()));
    }

    // Load all imports and merge into single file
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main_with_file(&path, file)?;
        loader.merge_all()?
    } else {
        file
    };

    // Auto-merge standard library prelude unless --no-std
    if !no_std {
        loader::merge_prelude_into(&mut merged_file);
    }

    // Reorder so the entry `main` is the LAST item. After
    // `merge_prelude_into` (which inserts prelude items at the front)
    // and `merge_all` (which keeps `main` from the entry file first),
    // `main` ends up before its callees from `use std::…`. Pushing
    // `main` to the back guarantees every pub helper is compiled
    // (and its LLVM symbol emitted) before `main` references it.
    if let Some(main_idx) = merged_file
        .items
        .iter()
        .position(|i| matches!(i, Item::Func(f) if f.name == "main"))
    {
        let main_item = merged_file.items.remove(main_idx);
        merged_file.items.push(main_item);
    }

    let checked_program = if strict {
        mimi::core::check_program_strict(&merged_file)
    } else {
        mimi::core::check_program(&merged_file)
    };
    let checked_program = match checked_program {
        Ok(program) => program,
        Err(diagnostics) => {
            eprintln!(
                "{} has {} type error(s):",
                path.display(),
                diagnostics.len()
            );
            let use_color = colors_enabled();
            let src = mimi::path_safety::read_source_capped(&path).ok();
            let src_ref = src.as_deref();
            for d in &diagnostics {
                let formatted = format_diagnostic(d, src_ref, &path.display().to_string());
                if use_color {
                    eprint!("{}", formatted);
                } else {
                    eprint!("{}", strip_ansi(&formatted));
                }
            }
            return Err("type checking failed".into());
        }
    };

    if verify_ffi {
        match verifier::verify_ffi_checked(&checked_program) {
            Ok(ffi_results) => {
                for res in &ffi_results {
                    if res.status == verifier::VerifStatus::Disproven {
                        eprintln!("⚠  FFI violation: {} — {}", res.func_name, res.message);
                        if let Some(diag) = &res.diagnostic {
                            let formatted =
                                format_diagnostic(diag, None, &path.display().to_string());
                            eprint!("{}", formatted);
                        }
                    } else if res.status.is_inconclusive() {
                        eprintln!("ℹ  {} — {}", res.func_name, res.message);
                    }
                }
                // v0.31.25: --verify-ffi fails closed on Disproven or
                // solver/infrastructure limitations (不放行 Unknown).
                // Body-level NotInTrustedSubset and NoObligations are exempt.
                let ffi_failed = ffi_results.iter().any(|r| {
                    r.status == verifier::VerifStatus::Disproven
                        || (r.status.is_inconclusive()
                            && r.status != verifier::VerifStatus::NoObligations
                            && !matches!(
                                r.trusted_subset_domain,
                                Some(verifier::TrustedSubsetDomain::Body)
                            ))
                });
                if ffi_failed {
                    return Err("FFI contract verification failed".into());
                }
            }
            Err(e) => {
                // P-H11: --verify-ffi must fail closed when the verifier itself
                // errors (timeout, Z3 unavailable after typecheck, etc.).
                return Err(format!("FFI verification error: {}", e));
            }
        }
    }

    let context = inkwell::context::Context::create();
    let module_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("main");
    let mut codegen = codegen::CodeGenerator::new(&context, module_name);
    codegen.strict = strict;
    codegen.no_std = no_std;
    codegen.verify_contracts = verify_contracts;
    codegen.shared = shared;
    codegen.target_triple = target.map(|s| s.to_string());

    if let Err(diagnostics) = codegen.compile_checked(&checked_program) {
        let use_color = colors_enabled();
        let filename = path.display().to_string();
        for diagnostic in &diagnostics {
            let formatted = format_diagnostic(diagnostic, Some(source.as_str()), &filename);
            if use_color {
                eprint!("{}", formatted);
            } else {
                eprint!("{}", strip_ansi(&formatted));
            }
        }
        return Err("native backend capability check failed".into());
    }

    if emit_ir {
        println!("{}", codegen.emit_ir());
        return Ok(());
    }

    let output_path_buf = output.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("main");
        let mut out = std::path::PathBuf::from(name);
        let ext = output_extension(target, shared);
        if ext.is_empty() {
            out.set_extension("");
        } else {
            out.set_extension(ext.trim_start_matches('.'));
        }
        out
    });
    let output_path = output.unwrap_or(&output_path_buf);

    // P-H8: stage object files in a temp directory so intermediate
    // artifacts never collide with user-named outputs in the project tree.
    let tmp_dir = std::env::temp_dir().join(format!(
        "mimi-build-{}-{}",
        std::process::id(),
        output_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("out")
    ));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("failed to create temp build dir: {}", e))?;
    let obj_path = tmp_dir.join(
        output_path
            .file_name()
            .map(|n| {
                let mut p = std::path::PathBuf::from(n);
                p.set_extension("o");
                p
            })
            .unwrap_or_else(|| std::path::PathBuf::from("out.o")),
    );

    codegen
        .compile_to_object(&obj_path)
        .map_err(|e| e.to_diagnostic().to_string())?;

    // Determine the C compiler/linker to use (cross-compiler or native)
    let cc_cmd = target_linker(target).unwrap_or_else(|| "cc".to_string());

    // Compile and link Rust runtime
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime_rs = manifest_dir.join("src/runtime/standalone.rs");
    // Native executable builds share an immutable, content-addressed runtime
    // archive. Cross/shared builds keep their per-build archive because target
    // and relocation flags change the artifact ABI.
    let use_native_cache = cfg!(unix) && target.is_none() && !shared && !no_std;
    let runtime_lib = if use_native_cache {
        #[cfg(unix)]
        {
            cached_native_runtime(&runtime_rs)?
        }
        #[cfg(not(unix))]
        {
            return Err("native runtime cache requires Unix".into());
        }
    } else {
        let runtime_lib = tmp_dir.join("libmimi_runtime.a");
        let mut rt_cmd = std::process::Command::new("rustc");
        rt_cmd.arg("--edition").arg("2021");
        rt_cmd.arg("--crate-type").arg("staticlib");
        rt_cmd.arg("--cfg").arg("standalone");
        rt_cmd.arg("--crate-name").arg("mimi_runtime");
        // Runtime symbols are called from LLVM IR (invisible to rustc reachability).
        rt_cmd.arg("-A").arg("dead_code");
        rt_cmd.args(asan_rustc_flags());
        if let Some(triple) = target {
            rt_cmd.arg("--target").arg(triple);
        }
        if shared {
            rt_cmd.arg("-C").arg("relocation-model=pic");
        }
        if asan_enabled() {
            // `-Z sanitizer=address` requires the nightly compiler.
            rt_cmd.env("RUSTUP_TOOLCHAIN", "nightly");
        }
        rt_cmd.arg("-o").arg(&runtime_lib);
        rt_cmd.arg(&runtime_rs);
        let rt_status = rt_cmd
            .status()
            .map_err(|e| format!("runtime compile (rustc): {}", e))?;
        if !rt_status.success() {
            let _ = std::fs::remove_file(&obj_path);
            return Err("Rust runtime compilation failed".into());
        }
        runtime_lib
    };

    // Link with cc to create executable or shared library
    let mut cmd = std::process::Command::new(&cc_cmd);
    cmd.args(asan_link_flags());
    // Prefer lld when available — 5× faster than GNU ld on the 28 MB runtime archive.
    if target.is_none() {
        let has_lld = std::process::Command::new("ld.lld")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if has_lld {
            cmd.arg("-fuse-ld=lld");
        }
    }
    if shared {
        cmd.arg("-shared").arg("-fPIC");
        if no_std {
            cmd.arg("-nostdlib");
        }
    } else if no_std {
        cmd.arg("-nostdlib").arg("-static");
    } else if target_os(target.unwrap_or("")) != "windows" {
        cmd.arg("-no-pie");
    }
    // Add target-specific linker flags (e.g. -lws2_32 for Windows)
    for flag in target_linker_flags(target) {
        cmd.arg(flag);
    }
    let status = cmd
        .arg(obj_path.to_str().ok_or("object path is not valid UTF-8")?)
        .arg(
            runtime_lib
                .to_str()
                .ok_or("runtime library path is not valid UTF-8")?,
        )
        // Link stdlib dependencies *after* the object files so that
        // `--as-needed` (the modern ld default) does not drop them
        // when no unresolved symbols have been seen yet.
        .args(
            if !no_std {
                ["-lpthread", "-ldl", "-lm"]
            } else {
                ["-lpthread", "", ""]
            }
            .iter()
            .filter(|s| !s.is_empty()),
        )
        .arg("-o")
        .arg(
            output_path
                .to_str()
                .ok_or("output path is not valid UTF-8")?,
        )
        .status()
        .map_err(|e| format!("failed to run linker: {}", e))?;

    // Cleanup intermediate files
    let _ = std::fs::remove_file(&obj_path);
    if !use_native_cache {
        let _ = std::fs::remove_file(&runtime_lib);
    }

    if status.success() {
        let kind = if shared {
            "shared library"
        } else {
            "executable"
        };
        println!(
            "✓ Compiled {} → {} ({})",
            path.display(),
            output_path.display(),
            kind
        );
    } else {
        return Err(format!("linker failed with exit code {:?}", status.code()));
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(())
}
