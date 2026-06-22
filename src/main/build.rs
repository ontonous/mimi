use std::fs;
use std::path::Path;

use crate::codegen;
use crate::diagnostic::format::{colors_enabled, format_diagnostic, strip_ansi};
use crate::{lexer, loader, parser, resolve_path, verifier};

pub(crate) fn build(path: Option<&Path>, output: Option<&Path>, emit_ir: bool, strict: bool, no_std: bool, verify_contracts: bool, verify_ffi: bool, shared: bool) -> Result<(), String> {
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
    crate::contracts::map_rule_contracts(&mut merged_file);

    let check_result = if strict { crate::core::check_strict(&merged_file) } else { crate::core::check(&merged_file) };
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

    codegen.compile_file(&merged_file).map_err(|e| e.to_diagnostic().to_string())?;

    if emit_ir {
        println!("{}", codegen.emit_ir());
        return Ok(());
    }

    let output_path_buf = output.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        let mut out = path.clone();
        if shared {
            out.set_extension("so");
        } else {
            out.set_extension("");
        }
        out
    });
    let output_path = output.unwrap_or(&output_path_buf);

    codegen.compile_to_object(&output_path.with_extension("o")).map_err(|e| e.to_diagnostic().to_string())?;

    // Compile and link C runtime
    let obj_path = output_path.with_extension("o");
    // Compile the C runtime to a temp object
    let runtime_c = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime/mimi_runtime.c");
    let runtime_o = output_path.parent().unwrap_or(std::path::Path::new(".")).join("mimi_runtime.o");
    let mut rt_cmd = std::process::Command::new("cc");
    if no_std {
        rt_cmd.arg("-DMIMI_NO_STD");
    }
    if shared {
        rt_cmd.arg("-fPIC");
    }
    let rt_status = rt_cmd
        .arg("-c").arg(&runtime_c).arg("-o").arg(&runtime_o)
        .status()
        .map_err(|e| format!("runtime compile: {}", e))?;
    if !rt_status.success() {
        let _ = std::fs::remove_file(&obj_path);
        return Err("C runtime compilation failed".into());
    }

    // Link with cc to create executable or shared library
    let mut cmd = std::process::Command::new("cc");
    if shared {
        cmd.arg("-shared").arg("-fPIC");
        if no_std {
            cmd.arg("-nostdlib");
        }
    } else if no_std {
        cmd.arg("-nostdlib").arg("-static");
    } else {
        cmd.arg("-no-pie");
    }
    let status = cmd
        .arg(obj_path.to_str().ok_or("object path is not valid UTF-8")?)
        .arg(runtime_o.to_str().ok_or("runtime object path is not valid UTF-8")?)
        .arg("-o")
        .arg(output_path.to_str().ok_or("output path is not valid UTF-8")?)
        .status()
        .map_err(|e| format!("failed to run linker: {}", e))?;

    // Cleanup object files
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&runtime_o);

    if status.success() {
        let kind = if shared { "shared library" } else { "executable" };
        println!("✓ Compiled {} → {} ({})", path.display(), output_path.display(), kind);
    } else {
        return Err(format!("linker failed with exit code {:?}", status.code()));
    }
    Ok(())
}
