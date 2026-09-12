use std::path::Path;

#[cfg(unix)]
use std::os::fd::AsRawFd;

use crate::resolve_path;
use mimi::ast::Item;
use mimi::codegen;
use mimi::diagnostic::format::{
    colors_enabled, format_diagnostic, format_diagnostic_with_registry, strip_ansi,
};
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

/// Owns the per-build staging directory for the whole native build attempt.
///
/// The linker and runtime compiler can fail after the directory has been
/// created, so cleanup must happen on every return path rather than only
/// after a successful link.  The directory name is process-scoped and holds
/// only transient object/runtime artifacts.
struct TempBuildDirGuard {
    path: std::path::PathBuf,
}

impl TempBuildDirGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempBuildDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Owns a content-addressed runtime cache's temporary archive until it has
/// been atomically renamed into place.
struct TempFileGuard {
    path: std::path::PathBuf,
}

impl TempFileGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn publish_runtime_cache(tmp_path: &Path, cache_path: &Path) -> Result<std::path::PathBuf, String> {
    let _tmp_guard = TempFileGuard::new(tmp_path.to_path_buf());
    std::fs::rename(tmp_path, cache_path).map_err(|e| format!("publish runtime cache: {e}"))?;
    Ok(cache_path.to_path_buf())
}

fn cleanup_runtime_cache_temps(cache_dir: &Path, key: &str) {
    let prefix = format!("libmimi_runtime_{key}.tmp-");
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn runtime_cache_key(runtime_rs: &Path) -> Result<String, String> {
    runtime_cache_key_with_asan(runtime_rs, asan_enabled())
}

fn runtime_compiler_identity(asan: bool) -> Result<String, String> {
    let mut command = std::process::Command::new("rustc");
    command.args(["--version", "--verbose"]);
    if asan {
        // Keep the cache identity in lockstep with the compiler used for the
        // actual ASan runtime archive below.
        command.env("RUSTUP_TOOLCHAIN", "nightly");
    }
    let output = command
        .output()
        .map_err(|e| format!("runtime compiler identity (rustc): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "runtime compiler identity failed with exit code {:?}",
            output.status.code()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|identity| identity.trim_end().to_owned())
        .map_err(|e| format!("runtime compiler identity is not UTF-8: {e}"))
}

fn runtime_cache_key_with_asan(runtime_rs: &Path, asan: bool) -> Result<String, String> {
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
    if asan {
        // Invalidate the cache for ASan builds so a non-ASan runtime is never
        // reused for an ASan-instrumented link.
        hasher.update(b"asan\0");
    }
    hasher.update(b"rustc\0");
    hasher.update(runtime_compiler_identity(asan)?.as_bytes());
    hasher.update(b"\0");
    for path in files {
        hasher.update(path.to_string_lossy().as_bytes());
        let contents =
            std::fs::read(&path).map_err(|e| format!("read runtime file {path:?}: {e}"))?;
        hasher.update(&contents);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn runtime_cache_hit(cache_path: &Path) -> Result<Option<std::path::PathBuf>, String> {
    match std::fs::symlink_metadata(cache_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(cache_path.to_path_buf())),
        Ok(_) => Err(format!(
            "runtime cache path is not a regular file: {cache_path:?}"
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect runtime cache: {error}")),
    }
}

#[cfg(unix)]
fn acquire_runtime_cache_lock(cache_dir: &Path) -> Result<std::fs::File, String> {
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
    Ok(lock_file)
}

#[cfg(unix)]
fn cached_native_runtime(runtime_rs: &Path) -> Result<std::path::PathBuf, String> {
    let key = runtime_cache_key(runtime_rs)?;
    let cache_dir = std::env::temp_dir().join("mimi_runtime_build_cache");
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("create runtime cache: {e}"))?;
    let cache_path = cache_dir.join(format!("libmimi_runtime_{key}.a"));
    let _lock_file = acquire_runtime_cache_lock(&cache_dir)?;
    cleanup_runtime_cache_temps(&cache_dir, &key);
    if let Some(cache_path) = runtime_cache_hit(&cache_path)? {
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
    publish_runtime_cache(&tmp_path, &cache_path)
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
    mir: bool,
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
                let formatted = format_diagnostic_with_registry(
                    d,
                    &merged_file.sources,
                    src_ref,
                    &path.display().to_string(),
                );
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
                            let formatted = format_diagnostic_with_registry(
                                diag,
                                &merged_file.sources,
                                Some(source.as_str()),
                                &path.display().to_string(),
                            );
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

    let (compile_result, canonical_default) = if mir {
        let canonical =
            crate::canonical_dispatch::build_canonical_program(&checked_program, &merged_file)?;
        (codegen.compile_mir_native(&canonical), false)
    } else {
        match crate::canonical_dispatch::select_default_route(&checked_program, &merged_file) {
            crate::canonical_dispatch::DefaultMirRoute::Canonical(canonical) => {
                (codegen.compile_mir_native(&canonical), true)
            }
            crate::canonical_dispatch::DefaultMirRoute::Legacy(_reason) => {
                crate::canonical_dispatch::report_legacy_route(_reason);
                (codegen.compile_checked(&checked_program), false)
            }
            crate::canonical_dispatch::DefaultMirRoute::Rejected(reason) => {
                return Err(format!("default Canonical MIR route rejected: {reason}"));
            }
        }
    };

    if let Err(diagnostics) = compile_result {
        let use_color = colors_enabled();
        let filename = path.display().to_string();
        for diagnostic in &diagnostics {
            let formatted = format_diagnostic_with_registry(
                diagnostic,
                &merged_file.sources,
                Some(source.as_str()),
                &filename,
            );
            if use_color {
                eprint!("{}", formatted);
            } else {
                eprint!("{}", strip_ansi(&formatted));
            }
        }
        return Err(if mir || canonical_default {
            "canonical MIR native backend capability check failed".into()
        } else {
            "native backend capability check failed".into()
        });
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
    let _tmp_dir_guard = TempBuildDirGuard::new(tmp_dir.clone());
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        cleanup_runtime_cache_temps, publish_runtime_cache, runtime_cache_hit, runtime_cache_key,
        runtime_cache_key_with_asan,
    };
    use std::fs;

    #[test]
    fn runtime_cache_publish_failure_removes_temporary_archive() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-publish-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache publish test directory");
        let tmp_path = dir.join("runtime.tmp");
        let cache_path = dir.join("missing-parent").join("runtime.a");
        fs::write(&tmp_path, b"temporary runtime archive")
            .expect("write temporary runtime archive");

        let result = publish_runtime_cache(&tmp_path, &cache_path);
        assert!(result.is_err(), "missing cache parent must reject publish");
        assert!(
            !tmp_path.exists(),
            "failed runtime cache publish left temporary archive {tmp_path:?}"
        );
        assert!(!cache_path.exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn runtime_cache_publish_success_preserves_archive_after_guard_drop() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-publish-success-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache publish success directory");
        let tmp_path = dir.join("runtime.tmp");
        let cache_path = dir.join("runtime.a");
        fs::write(&tmp_path, b"runtime archive").expect("write temporary runtime archive");

        let published = publish_runtime_cache(&tmp_path, &cache_path)
            .expect("runtime cache publish should succeed");
        assert_eq!(published, cache_path);
        assert!(!tmp_path.exists());
        assert_eq!(
            fs::read(&cache_path).expect("read published runtime archive"),
            b"runtime archive"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn runtime_cache_cleanup_removes_only_matching_stale_temporaries() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-cleanup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache cleanup directory");
        let stale = dir.join("libmimi_runtime_deadbeef.tmp-old");
        let other = dir.join("libmimi_runtime_cafebabe.tmp-keep");
        let archive = dir.join("libmimi_runtime_deadbeef.a");
        fs::write(&stale, b"stale").expect("write stale runtime temporary");
        fs::write(&other, b"other").expect("write other runtime temporary");
        fs::write(&archive, b"archive").expect("write runtime archive");

        cleanup_runtime_cache_temps(&dir, "deadbeef");
        assert!(!stale.exists());
        assert!(other.exists());
        assert!(archive.exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn runtime_cache_key_changes_with_source_content_and_file_set() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-key-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache key directory");
        let runtime_rs = dir.join("standalone.rs");
        let helper_rs = dir.join("helper.rs");
        fs::write(&runtime_rs, b"fn runtime() {}\n").expect("write runtime source");
        fs::write(&helper_rs, b"fn helper() {}\n").expect("write helper source");

        let first = runtime_cache_key(&runtime_rs).expect("compute initial runtime cache key");
        assert_eq!(
            first,
            runtime_cache_key(&runtime_rs).expect("recompute initial runtime cache key")
        );
        fs::write(&helper_rs, b"fn helper_changed() {}\n").expect("change helper source");
        let changed_content =
            runtime_cache_key(&runtime_rs).expect("compute changed-content runtime cache key");
        assert_ne!(first, changed_content);
        fs::write(dir.join("extra.rs"), b"fn extra() {}\n").expect("add runtime source");
        let changed_file_set =
            runtime_cache_key(&runtime_rs).expect("compute changed-file-set runtime cache key");
        assert_ne!(changed_content, changed_file_set);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn runtime_cache_key_separates_asan_and_normal_modes() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-asan-key-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache ASan key directory");
        let runtime_rs = dir.join("standalone.rs");
        fs::write(&runtime_rs, b"fn runtime() {}\n").expect("write runtime source");

        let normal = runtime_cache_key_with_asan(&runtime_rs, false)
            .expect("compute normal runtime cache key");
        let asan =
            runtime_cache_key_with_asan(&runtime_rs, true).expect("compute ASan runtime cache key");
        assert_ne!(normal, asan);
        assert_eq!(
            normal,
            runtime_cache_key_with_asan(&runtime_rs, false)
                .expect("recompute normal runtime cache key")
        );
        assert_eq!(
            asan,
            runtime_cache_key_with_asan(&runtime_rs, true)
                .expect("recompute ASan runtime cache key")
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn runtime_cache_hit_requires_regular_archive_file() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-hit-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache hit directory");
        let missing = dir.join("missing.a");
        assert_eq!(
            runtime_cache_hit(&missing).expect("missing cache is not an error"),
            None
        );

        let archive = dir.join("runtime.a");
        fs::write(&archive, b"runtime archive").expect("write runtime archive");
        assert_eq!(
            runtime_cache_hit(&archive).expect("regular archive is a cache hit"),
            Some(archive.clone())
        );

        let collision = dir.join("collision.a");
        fs::create_dir(&collision).expect("create non-file cache collision");
        let error =
            runtime_cache_hit(&collision).expect_err("non-file cache collision must fail closed");
        assert!(
            error.starts_with("runtime cache path is not a regular file:"),
            "{error}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_cache_hit_rejects_symlink_archive() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-symlink-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache symlink directory");
        let target = dir.join("target.a");
        let link = dir.join("link.a");
        fs::write(&target, b"runtime archive").expect("write runtime archive target");
        std::os::unix::fs::symlink(&target, &link).expect("create runtime cache symlink");
        let error = runtime_cache_hit(&link).expect_err("symlink cache must fail closed");
        assert!(
            error.starts_with("runtime cache path is not a regular file:"),
            "{error}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_cache_lock_open_failure_is_structured() {
        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("build.lock")).expect("create runtime cache lock directory");

        let error = super::acquire_runtime_cache_lock(&dir)
            .expect_err("directory lock path must fail to open as a file");
        assert!(error.starts_with("open runtime cache lock:"), "{error}");

        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_cache_lock_releases_after_guard_drop() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Barrier,
        };
        use std::thread;
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!(
            "mimi-runtime-cache-lock-release-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create runtime cache lock release directory");
        let first_lock =
            super::acquire_runtime_cache_lock(&dir).expect("first runtime cache lock acquisition");
        let ready = Arc::new(Barrier::new(2));
        let acquired = Arc::new(AtomicBool::new(false));
        let thread_ready = Arc::clone(&ready);
        let thread_acquired = Arc::clone(&acquired);
        let thread_dir = dir.clone();
        let waiter = thread::spawn(move || {
            thread_ready.wait();
            let second_lock = super::acquire_runtime_cache_lock(&thread_dir)
                .expect("second runtime cache lock acquisition");
            thread_acquired.store(true, Ordering::Release);
            drop(second_lock);
        });
        ready.wait();
        thread::sleep(Duration::from_millis(50));
        assert!(
            !acquired.load(Ordering::Acquire),
            "second cache lock acquired while first guard was still held"
        );
        drop(first_lock);
        waiter.join().expect("cache lock waiter must finish");
        assert!(acquired.load(Ordering::Acquire));

        fs::remove_dir_all(&dir).ok();
    }
}
