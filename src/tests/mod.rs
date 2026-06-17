pub(crate) mod basic_control_flow;
pub(crate) mod basic_let;
pub(crate) mod basic_functions;
pub(crate) mod basic_operators;
pub(crate) mod basic_literals;
pub(crate) mod basic_lists;
pub(crate) mod basic_tuples;
pub(crate) mod basic_other;
pub(crate) mod closures;

pub(crate) mod strings;
pub(crate) mod builtin_funcs;
pub(crate) mod typecheck;
pub(crate) mod error_handling;
pub(crate) mod visibility;
pub(crate) mod contracts;
pub(crate) mod comptime;
pub(crate) mod ownership;
pub(crate) mod actors;
pub(crate) mod capabilities;
pub(crate) mod generics;
pub(crate) mod extern_blocks;
pub(crate) mod comprehension;

pub(crate) mod v1_2_generics;
pub(crate) mod v1_2_traits;
pub(crate) mod v1_2_parasteps;
pub(crate) mod v1_2_mms;
pub(crate) mod v1_2_effects;
pub(crate) mod v1_2_contract_extract;
pub(crate) mod v1_2_verification;
pub(crate) mod v1_2_static;
pub(crate) mod v1_2_boundary;
pub(crate) mod v1_2_error_paths;
pub(crate) mod v1_2_modules;
pub(crate) mod v1_2_commitment;
pub(crate) mod v1_2_allocators;
pub(crate) mod v1_2_codegen;
pub(crate) mod v1_2_operators;
pub(crate) mod v1_2_generics_misc;
pub(crate) mod v1_2_traits_misc;
pub(crate) mod v1_2_type_def_misc;
pub(crate) mod v1_2_builtin_hof;
pub(crate) mod v1_2_infra;
pub(crate) mod v1_2_misc_remaining;

pub(crate) mod loader;
pub(crate) mod manifest;
pub(crate) mod lsp;
pub(crate) mod extern_calls;
pub(crate) mod ffi_safety;
pub(crate) mod ffi_passport_types;
pub(crate) mod ffi_verification;
pub(crate) mod type_system_verification;
pub(crate) mod actor_concurrent;
pub(crate) mod derive_methods;
pub(crate) mod builtin_extended;
pub(crate) mod cap_runtime;
pub(crate) mod codegen_control;
pub(crate) mod lsp_extended;
pub(crate) mod cli_commands;
pub(crate) mod mms_integration;
pub(crate) mod package_management;
pub(crate) mod property;

// === CODEGEN test modules ===
pub(crate) mod codegen_e2e;
pub(crate) mod codegen_ir;
pub(crate) mod codegen_advanced;

use crate::{core, interp, lexer, parser};

/// File-based lock for tests that mutate the process-wide `MIMI_FFI_LIB` environment
/// variable. This works across multiple test binaries running in parallel.
pub(crate) struct FfiEnvLock {
    _file: std::fs::File,
}

impl FfiEnvLock {
    pub fn lock() -> Self {
        let lock_path = std::env::temp_dir().join("mimi_ffi_test.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .expect("failed to create FFI test lock file");
        
        // Use file locking to ensure exclusive access
        #[cfg(unix)]
        unsafe {
            use std::os::unix::io::AsRawFd;
            libc::flock(file.as_raw_fd(), libc::LOCK_EX);
        }
        
        Self { _file: file }
    }
}

impl Drop for FfiEnvLock {
    fn drop(&mut self) {
        // Lock is automatically released when file is closed
    }
}

pub(crate) fn parse(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    parser::Parser::new(tokens).parse_file().unwrap()
}

pub(crate) fn run_source(src: &str) -> interp::Value {
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    interp.run().unwrap()
}

pub(crate) fn run_source_result(src: &str) -> Result<interp::Value, String> {
    let tokens = lexer::Lexer::new(src).tokenize().map_err(|e| e)?;
    let file = parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    interp.run().map_err(|e| e.message)
}

pub(crate) fn check_source(src: &str) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
    let file = parse(src);
    core::check(&file)
}

pub(crate) fn check_source_strict(src: &str) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
    let file = parse(src);
    core::check_strict(&file)
}

/// End-to-end codegen test: compile Mimi source -> LLVM -> native binary -> execute -> return stdout
/// Requires `cc` and `ld` on PATH. Skips test if linker is unavailable.
pub(crate) fn compile_and_run(src: &str) -> Result<String, String> {
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static E2E_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = E2E_COUNTER.fetch_add(1, Ordering::Relaxed);

    let tokens = crate::lexer::Lexer::new(src).tokenize().map_err(|e| format!("lexer: {}", e))?;
    let file = crate::parser::Parser::new(tokens).parse_file().map_err(|e| format!("parser: {}", e))?;

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "e2e_test");
    codegen.compile_file(&file)?;

    let tmp_dir = std::env::temp_dir().join(format!("mimi_e2e_{}_{}", std::process::id(), counter));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;
    let obj_path = tmp_dir.join("test.o");
    let bin_path = if cfg!(target_os = "windows") { tmp_dir.join("test.exe") } else { tmp_dir.join("test") };

    codegen.compile_to_object(&obj_path)?;

    let status = Command::new("cc")
        .arg("-no-pie").arg(&obj_path).arg("-o").arg(&bin_path)
        .status()
        .map_err(|e| format!("linker: {}", e))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("linker failed with exit code {:?}", status.code()));
    }

    let output = Command::new(&bin_path)
        .output()
        .map_err(|e| format!("run: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("exit code {:?}, stderr: {}", output.status.code(), stderr));
    }

    Ok(stdout)
}
