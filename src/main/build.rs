use std::fs;
use std::path::Path;

use mimi::codegen;
use mimi::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use mimi::{lexer, loader, parser, verifier};
use crate::resolve_path;

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

pub(crate) fn build(path: Option<&Path>, output: Option<&Path>, emit_ir: bool, strict: bool, no_std: bool, verify_contracts: bool, verify_ffi: bool, shared: bool, target: Option<&str>) -> Result<(), String> {
    let path = resolve_path(path)?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens).parse_file()?;

    // Load all imports and merge into single file
    let mut merged_file = if !file.imports.is_empty() {
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
        let mut loader = loader::ModuleLoader::new(base_dir);
        loader.load_main(&path)?;
        loader.merge_all()?
    } else {
        file
    };

    // Map inline rule statements to structured contracts
    mimi::contracts::map_rule_contracts(&mut merged_file);

    let check_result = if strict { mimi::core::check_strict(&merged_file) } else { mimi::core::check(&merged_file) };
    if let Err(diagnostics) = check_result {
        eprintln!("{} has {} type error(s):", path.display(), diagnostics.len());
        let use_color = colors_enabled();
        let src = fs::read_to_string(&path).ok();
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

    if verify_ffi {
        match verifier::Verifier::with_timeout(5000) {
            Ok(mut v) => {
                let ffi_results = v.verify_ffi_call_sites(&merged_file);
                for res in &ffi_results {
                    match res.status {
                        verifier::VerifStatus::Failed => {
                            eprintln!("⚠  FFI violation: {} — {}", res.func_name, res.message);
                            if let Some(diag) = &res.diagnostic {
                                let formatted = format_diagnostic(diag, None, &path.display().to_string());
                                eprint!("{}", formatted);
                            }
                        }
                        verifier::VerifStatus::Unknown => {
                            eprintln!("ℹ  {} — {}", res.func_name, res.message);
                        }
                        verifier::VerifStatus::Verified => {}
                    }
                }
                if ffi_results.iter().any(|r| r.status == verifier::VerifStatus::Failed) {
                    return Err("FFI contract verification failed".into());
                }
            }
            Err(e) => {
                eprintln!("⚠  Skipping FFI verification: {}", e);
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

    codegen.compile_file(&merged_file).map_err(|e| e.to_diagnostic().to_string())?;

    if emit_ir {
        println!("{}", codegen.emit_ir());
        return Ok(());
    }

    let output_path_buf = output.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        let mut out = path.clone();
        let ext = output_extension(target, shared);
        if ext.is_empty() {
            out.set_extension("");
        } else {
            out.set_extension(ext.trim_start_matches('.'));
        }
        out
    });
    let output_path = output.unwrap_or(&output_path_buf);

    codegen.compile_to_object(&output_path.with_extension("o")).map_err(|e| e.to_diagnostic().to_string())?;

    // Determine the C compiler/linker to use (cross-compiler or native)
    let cc_cmd = target_linker(target).unwrap_or_else(|| "cc".to_string());

    // Compile and link Rust runtime
    let obj_path = output_path.with_extension("o");
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime_rs = manifest_dir.join("src/runtime/standalone.rs");
    let runtime_lib = output_path.parent().unwrap_or(std::path::Path::new(".")).join("libmimi_runtime.a");

    // Compile the standalone runtime with rustc
    let mut rt_cmd = std::process::Command::new("rustc");
    rt_cmd.arg("--edition").arg("2021");
    rt_cmd.arg("--crate-type").arg("staticlib");
    rt_cmd.arg("--cfg").arg("standalone");
    rt_cmd.arg("--crate-name").arg("mimi_runtime");
    if let Some(triple) = target {
        rt_cmd.arg("--target").arg(triple);
    }
    if shared {
        rt_cmd.arg("-C").arg("relocation-model=pic");
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

    // Link with cc to create executable or shared library
    let mut cmd = std::process::Command::new(&cc_cmd);
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
    // Link stdlib dependencies when not building no_std
    if !no_std {
        cmd.arg("-lpthread").arg("-ldl").arg("-lm");
    }
    let status = cmd
        .arg(obj_path.to_str().ok_or("object path is not valid UTF-8")?)
        .arg(runtime_lib.to_str().ok_or("runtime library path is not valid UTF-8")?)
        .arg("-o")
        .arg(output_path.to_str().ok_or("output path is not valid UTF-8")?)
        .status()
        .map_err(|e| format!("failed to run linker: {}", e))?;

    // Cleanup intermediate files
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&runtime_lib);

    if status.success() {
        let kind = if shared { "shared library" } else { "executable" };
        println!("✓ Compiled {} → {} ({})", path.display(), output_path.display(), kind);
    } else {
        return Err(format!("linker failed with exit code {:?}", status.code()));
    }
    Ok(())
}
