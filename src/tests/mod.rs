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
pub(crate) mod build_shared;
pub(crate) mod ffi_safety;
pub(crate) mod ffi_passport_types;
pub(crate) mod ffi_verification;
pub(crate) mod ffi_interp_e2e;
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

// === JSON test modules ===
pub(crate) mod json_tests;

// === CODEGEN test modules ===
pub(crate) mod codegen_e2e;
pub(crate) mod codegen_ir;
pub(crate) mod codegen_advanced;

// === Fuzz test modules ===
pub(crate) mod fuzz;

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

/// Compile `mimi_runtime.c` into a shared library for interpreter FFI tests.
/// Returns the path to the compiled `.so`.
/// The caller MUST hold `FfiEnvLock` before calling this and setting `MIMI_FFI_LIB`.
pub(crate) fn build_interp_ffi_so() -> Result<std::path::PathBuf, String> {
    use std::process::Command;
    let runtime_c = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime/mimi_runtime.c");
    let tmp_dir = std::env::temp_dir().join(format!("mimi_ffi_so_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;
    let so_path = tmp_dir.join("mimi_runtime_test.so");
    let status = Command::new("cc")
        .arg("-shared")
        .arg("-fPIC")
        .arg("-o")
        .arg(&so_path)
        .arg(&runtime_c)
        .status()
        .map_err(|e| format!("cc not found: {}", e))?;
    if !status.success() {
        return Err(format!("failed to compile test .so, exit code: {:?}", status.code()));
    }
    Ok(so_path)
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

/// Like `run_source_result` but with fork isolation disabled.
/// Needed for FFI tests that return pointers (raw_string, string, Json)
/// or use callbacks, which are incompatible with fork isolation
/// (child-process heap is not accessible from the parent).
pub(crate) fn run_source_result_no_fork(src: &str) -> Result<interp::Value, String> {
    let tokens = lexer::Lexer::new(src).tokenize().map_err(|e| e)?;
    let file = parser::Parser::new(tokens).parse_file().map_err(|e| e.message)?;
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_ffi = false;
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
static E2E_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Configuration flags for end-to-end codegen test execution.
pub(crate) struct E2EConfig {
    pub verify_contracts: bool,
    pub use_valgrind: bool,
    pub use_asan: bool,
    pub use_ubsan: bool,
    pub valgrind_args: Vec<String>,
}

impl Default for E2EConfig {
    fn default() -> Self {
        Self {
            verify_contracts: false,
            use_valgrind: false,
            use_asan: false,
            use_ubsan: false,
            valgrind_args: vec!["--tool=memcheck".into(), "--error-exitcode=1".into(), "--leak-check=full".into()],
        }
    }
}

fn compile_and_run_with_config(src: &str, config: &E2EConfig) -> Result<String, String> {
    if config.use_valgrind && config.use_asan {
        return Err("cannot use valgrind and ASAN simultaneously".into());
    }
    use std::process::Command;

    let counter = E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let tokens = crate::lexer::Lexer::new(src).tokenize().map_err(|e| format!("lexer: {}", e))?;
    let file = crate::parser::Parser::new(tokens).parse_file().map_err(|e| format!("parser: {}", e))?;

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "e2e_test");
    if config.verify_contracts {
        codegen.verify_contracts = true;
    }
    codegen.compile_file(&file).map_err(|e| e.to_string())?;

    let tmp_dir = std::env::temp_dir().join(format!("mimi_e2e_{}_{}", std::process::id(), counter));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;
    let obj_path = tmp_dir.join("test.o");
    let bin_path = if cfg!(target_os = "windows") { tmp_dir.join("test.exe") } else { tmp_dir.join("test") };

    codegen.compile_to_object(&obj_path).map_err(|e| e.to_string())?;

    // Compile the C runtime
    let runtime_c = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime/mimi_runtime.c");
    let runtime_o = tmp_dir.join("mimi_runtime.o");
    let mut cc_compile = Command::new("cc");
    cc_compile.arg("-c").arg(&runtime_c).arg("-o").arg(&runtime_o);
    if config.use_asan {
        cc_compile.arg("-fsanitize=address");
    }
    if config.use_ubsan {
        cc_compile.arg("-fsanitize=undefined").arg("-fno-sanitize-recover=all");
    }
    let rt_status = cc_compile.status()
        .map_err(|e| format!("runtime compile: {}", e))?;
    if !rt_status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("runtime compile failed with exit code {:?}", rt_status.code()));
    }

    let mut cc_link = Command::new("cc");
    cc_link.arg("-no-pie").arg(&obj_path).arg(&runtime_o).arg("-o").arg(&bin_path);
    if config.use_asan {
        cc_link.arg("-fsanitize=address");
    }
    if config.use_ubsan {
        cc_link.arg("-fsanitize=undefined");
    }
    let status = cc_link.status()
        .map_err(|e| format!("linker: {}", e))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!("linker failed with exit code {:?}", status.code()));
    }

    let output = if config.use_valgrind {
        let mut cmd = Command::new("valgrind");
        for arg in &config.valgrind_args {
            cmd.arg(arg);
        }
        cmd.arg(&bin_path);
        cmd.output().map_err(|e| format!("valgrind run: {}", e))?
    } else {
        Command::new(&bin_path).output().map_err(|e| format!("run: {}", e))?
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("exit code {:?}, stderr: {}", output.status.code(), stderr));
    }

    Ok(stdout)
}

/// Standard E2E codegen test: compile and run, return stdout.
pub(crate) fn compile_and_run(src: &str) -> Result<String, String> {
    compile_and_run_with_config(src, &E2EConfig::default())
}

/// E2E codegen test with contracts verification enabled.
pub(crate) fn compile_and_verify_contracts(src: &str) -> Result<String, String> {
    compile_and_run_with_config(src, &E2EConfig { verify_contracts: true, ..Default::default() })
}

/// E2E test running the binary under valgrind memcheck.
pub(crate) fn compile_and_run_valgrind(src: &str) -> Result<String, String> {
    compile_and_run_with_config(src, &E2EConfig { use_valgrind: true, ..Default::default() })
}

/// E2E test compiled with AddressSanitizer and run directly.
pub(crate) fn compile_and_run_asan(src: &str) -> Result<String, String> {
    compile_and_run_with_config(src, &E2EConfig { use_asan: true, ..Default::default() })
}

/// E2E test compiled with UndefinedBehaviorSanitizer and run directly.
pub(crate) fn compile_and_run_ubsan(src: &str) -> Result<String, String> {
    compile_and_run_with_config(src, &E2EConfig { use_ubsan: true, ..Default::default() })
}

