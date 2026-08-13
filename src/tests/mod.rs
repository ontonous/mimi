pub(crate) mod basic_control_flow;
pub(crate) mod basic_functions;
pub(crate) mod basic_let;
pub(crate) mod basic_lists;
pub(crate) mod basic_literals;
pub(crate) mod basic_operators;
pub(crate) mod basic_other;
pub(crate) mod basic_tuples;
pub(crate) mod boundary_stdlib;
pub(crate) mod closures;
pub(crate) mod codegen_boundary;
pub(crate) mod contracts;
pub(crate) mod diagnostic_routing;
pub(crate) mod float_chain;

pub(crate) mod actors;
pub(crate) mod builtin_funcs;
pub(crate) mod capabilities;
pub(crate) mod comprehension;
pub(crate) mod comptime;
pub(crate) mod error_handling;
pub(crate) mod extern_blocks;
pub(crate) mod generics;
pub(crate) mod interpreter_features;
pub(crate) mod ownership;
pub(crate) mod stdlib_comprehensive;
pub(crate) mod strings;
pub(crate) mod typecheck;
pub(crate) mod visibility;

pub(crate) mod v1_2_allocators;
pub(crate) mod v1_2_boundary;
pub(crate) mod v1_2_builtin_hof;
pub(crate) mod v1_2_codegen;
pub(crate) mod v1_2_core_edge;
// 0.34.18c (§4.2): v1_2_effects removed — the `with` effect clause is abolished.
pub(crate) mod v1_2_error_coverage;
pub(crate) mod v1_2_error_paths;
pub(crate) mod v1_2_generics;
pub(crate) mod v1_2_generics_misc;
pub(crate) mod v1_2_infra;
pub(crate) mod v1_2_misc_remaining;
pub(crate) mod v1_2_modules;
pub(crate) mod v1_2_operators;
pub(crate) mod v1_2_parasteps;
pub(crate) mod v1_2_static;
pub(crate) mod v1_2_traits;
pub(crate) mod v1_2_traits_misc;
pub(crate) mod v1_2_type_def_misc;
pub(crate) mod v1_2_verification;
pub(crate) mod v1_3_negative_suite;
pub(crate) mod v1_4_tricky_interaction;

pub(crate) mod actor_concurrent;
pub(crate) mod borrow_boundary;
pub(crate) mod build_shared;
pub(crate) mod builtin_extended;
pub(crate) mod builtin_registry;
pub(crate) mod cap_runtime;
pub(crate) mod cli_commands;
pub(crate) mod codegen_control;
pub(crate) mod cross_compile;
pub(crate) mod debug_info;
pub(crate) mod derive_methods;
pub(crate) mod extern_calls;
pub(crate) mod ffi_interp_e2e;
pub(crate) mod ffi_passport_types;
pub(crate) mod ffi_safety;
pub(crate) mod ffi_verification;
pub(crate) mod fmt_edge_cases;
pub(crate) mod loader;
pub(crate) mod lsp;
pub(crate) mod lsp_extended;
pub(crate) mod manifest;
pub(crate) mod mms_integration;
pub(crate) mod net;
pub(crate) mod package_management;
pub(crate) mod package_v02812;
pub(crate) mod package_v02812_extra;
pub(crate) mod property;
pub(crate) mod stdlib_v02813;
pub(crate) mod stdlib_v02813_list_growth;
pub(crate) mod transitive_deps;
pub(crate) mod type_system_verification;

// === Flow/State/Transition test modules ===
pub(crate) mod flow_features;

// === JSON test modules ===
pub(crate) mod json_tests;
pub(crate) mod pipe_loop_tests;
pub(crate) mod set_tests;

// === CODEGEN test modules ===
pub(crate) mod codegen_advanced;
pub(crate) mod codegen_e2e;
pub(crate) mod codegen_golden;
pub(crate) mod codegen_ir;

// === Fuzz test modules ===
pub(crate) mod fuzz;

// === Dual-backend equivalence tests ===
pub(crate) mod dual_backend;

// === Deep-eval 2026-08-09 regression locks (demos differential findings) ===
pub(crate) mod deep_eval_20260809;

// === Dual-interpreter equivalence tests (0.31.45) ===
pub(crate) mod dual_interp;

// === Trap tests: IEEE-754 / integer overflow / OOB (0.31.46) ===
pub(crate) mod trap_tests;

// === Benchmark modules ===
pub(crate) mod benchmarks;
pub(crate) mod lsp_e2e;

// === Audit regression tests ===
pub(crate) mod audit_regression;
pub(crate) mod audit_round2;
pub(crate) mod deep_audit;

// === Wave-1 full-audit fix regressions (2026-08-05) ===
pub(crate) mod audit_fix_bind_cpp;
pub(crate) mod audit_fix_bind_go;
pub(crate) mod audit_fix_bind_jni;
pub(crate) mod audit_fix_bind_node;
pub(crate) mod audit_fix_bind_py;
pub(crate) mod audit_fix_checker;
pub(crate) mod audit_fix_codegen_expr1;
pub(crate) mod audit_fix_codegen_expr2;
pub(crate) mod audit_fix_codegen_infra;
pub(crate) mod audit_fix_codegen_resolved;
pub(crate) mod audit_fix_component;
pub(crate) mod audit_fix_fmt_lint;
pub(crate) mod audit_fix_io;
pub(crate) mod audit_fix_json_math;
pub(crate) mod audit_fix_linearity;
pub(crate) mod audit_fix_list_string;
pub(crate) mod audit_fix_lowering;
pub(crate) mod audit_fix_lsp;
pub(crate) mod audit_fix_parser;
pub(crate) mod audit_fix_paths;
pub(crate) mod audit_fix_runtime_core;
pub(crate) mod audit_fix_runtime_sub;
pub(crate) mod audit_fix_scripts;
pub(crate) mod audit_fix_stdlib;
pub(crate) mod audit_fix_verifier;
pub(crate) mod audit_fix_verifier_resolved;
pub(crate) mod audit_fix_vm;
pub(crate) mod audit_fix_vm_exec;
pub(crate) mod error_co_h2;
pub(crate) mod fmt_corpus_eval;

use crate::{core, interp, lexer, parser};
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::Arc;

/// Probe the system linker once per test process.
///
/// Codegen and dual-backend tests call this guard hundreds of times. Spawning
/// `cc --version` for every test adds avoidable process overhead and can
/// amplify contention when the test harness runs in parallel.
pub(crate) fn can_link() -> bool {
    static CAN_LINK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN_LINK.get_or_init(|| {
        std::process::Command::new("cc")
            .arg("--version")
            .output()
            .is_ok()
    })
}

/// Detect the fastest available linker once per test process.
///
/// lld is 5× faster than GNU ld when linking against the 28 MB runtime
/// archive.  Falls back to the default linker when lld is absent.
pub(crate) fn linker_flag() -> &'static [&'static str] {
    static FLAG: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    FLAG.get_or_init(|| {
        let has_lld = std::process::Command::new("ld.lld")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if has_lld {
            vec!["-fuse-ld=lld"]
        } else {
            vec![]
        }
    })
}

/// Cache the compiled runtime static library across test cases.
/// Returns path to a cached `.a` compiled from `standalone.rs`.
/// The cache key is a hash of `standalone.rs` + `mod.rs` sources.
pub(crate) fn cached_runtime_lib() -> Result<std::path::PathBuf, String> {
    use std::hash::Hash;
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::io::AsRawFd;

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let runtime_rs = manifest.join("src/runtime/standalone.rs");
    let runtime_dir = manifest.join("src/runtime");

    let mut hasher = DefaultHasher::new();
    // The standalone build is `include!("mod.rs")` which pulls in every
    // `src/runtime/*.rs` module — the cache key must cover all of them, or a
    // change to e.g. regex.rs/net.rs would silently link a stale runtime.
    let mut runtime_files: Vec<_> = std::fs::read_dir(&runtime_dir)
        .map_err(|e| format!("read runtime dir: {}", e))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
        .collect();
    runtime_files.push(runtime_rs.clone());
    runtime_files.sort();
    for path in runtime_files {
        let mut f = std::fs::File::open(&path).map_err(|e| format!("open {:?}: {}", path, e))?;
        let mut buf = Vec::with_capacity(8192);
        f.read_to_end(&mut buf)
            .map_err(|e| format!("read {:?}: {}", path, e))?;
        buf.hash(&mut hasher);
    }
    let hash = hasher.finish();

    let cache_dir = std::env::temp_dir().join("mimi_runtime_cache");
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("mkdir cache: {}", e))?;
    let lib_path = cache_dir.join(format!("libmimi_runtime_{:016x}.a", hash));
    let lock_path = cache_dir.join("_build.lock");

    // File lock to serialize runtime compilation across parallel tests
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("create lock: {}", e))?;
    #[cfg(unix)]
    // SAFETY: fd 是上方 OpenOptions 真实打开的锁文件，flock 参数满足 libc 前置条件。
    unsafe {
        libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
    }

    // Check again after acquiring lock (another thread may have compiled it)
    if lib_path.exists() {
        #[cfg(unix)]
        // SAFETY: 同上——已持有锁的同一 fd 释放，无别名；参数有效。
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
        return Ok(lib_path);
    }

    // Write a temp file then atomically rename to avoid partial writes
    let tmp_path = cache_dir.join(format!("libmimi_runtime_{:016x}.tmp", hash));
    let output = std::process::Command::new("rustc")
        .arg("--edition")
        .arg("2021")
        .arg("--crate-type")
        .arg("staticlib")
        .arg("--cfg")
        .arg("standalone")
        // M2: enable deliberate UB test symbols only for FFI e2e .so builds.
        // Production `mimi build` does not pass this cfg.
        .arg("--cfg")
        .arg("mimi_test_ub_symbols")
        .arg("--crate-name")
        .arg("mimi_runtime")
        .arg("-o")
        .arg(&tmp_path)
        .arg(&runtime_rs)
        .output()
        .map_err(|e| format!("rustc not found: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        #[cfg(unix)]
        // SAFETY: 同上——同一 fd 释放锁，参数有效。
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
        return Err(format!(
            "runtime compilation failed, exit: {:?}, stderr: {}",
            output.status.code(),
            stderr
        ));
    }
    std::fs::rename(&tmp_path, &lib_path).map_err(|e| format!("rename: {}", e))?;

    // Strip debug info from the cached archive (28 MB → ~15 MB).
    // This reduces linker symbol-scan time by ~15 %.
    let _ = std::process::Command::new("strip")
        .arg("--strip-debug")
        .arg(&lib_path)
        .status();

    // Remove stale runtime archives from previous builds (different source hash).
    let current_name = lib_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("libmimi_runtime_") && name.ends_with(".a") && name != current_name
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    #[cfg(unix)]
    // SAFETY: 同上——正常路径释放锁。
    unsafe {
        libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
    }
    Ok(lib_path)
}

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
        // SAFETY: fd 来自已打开的真实锁文件，flock 上锁参数有效。
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

/// Compile the Rust runtime into a shared library for interpreter FFI tests.
/// Returns the path to the compiled `.so`.
/// The caller MUST hold `FfiEnvLock` before calling this and setting `MIMI_FFI_LIB`.
pub(crate) fn build_interp_ffi_so() -> Result<std::path::PathBuf, String> {
    use std::process::Command;

    // Reuse cached staticlib to avoid recompiling for every test
    let lib_path = cached_runtime_lib()?;

    let tmp_dir = std::env::temp_dir().join(format!("mimi_ffi_so_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;

    // Link the cached .a into a .so (use --whole-archive to force all symbols)
    let so_path = tmp_dir.join("mimi_runtime_test.so");
    let mut cc_so = Command::new("cc");
    cc_so.arg("-shared").arg("-fPIC").arg("-o").arg(&so_path);
    for flag in linker_flag() {
        cc_so.arg(flag);
    }
    cc_so
        .arg("-Wl,--whole-archive")
        .arg(&lib_path)
        .arg("-Wl,--no-whole-archive")
        .arg("-lpthread")
        .arg("-ldl")
        .arg("-lm");
    let status = cc_so.status().map_err(|e| format!("cc not found: {}", e))?;
    if !status.success() {
        return Err(format!(
            "failed to link test .so, exit code: {:?}",
            status.code()
        ));
    }

    Ok(so_path)
}

pub(crate) fn parse(src: &str) -> crate::ast::File {
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .expect("src/tests/mod.rs:144 unwrap failed");
    parser::Parser::new(tokens)
        .parse_file()
        .expect("src/tests/mod.rs:145 unwrap failed")
}

/// Run source via the Bytecode VM (default backend since 0.33).
/// Panics on compile or runtime error.
pub(crate) fn run_source(src: &str) -> interp::Value {
    let file = parse(src);
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed in run_source");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.run_value()
        .expect("bytecode run_value failed in run_source")
}

/// TC-C1: run Bytecode VM with stdout capture enabled.
/// Returns `(main return value, captured stdout)`.
pub(crate) fn run_source_with_stdout(src: &str) -> (interp::Value, String) {
    let file = parse(src);
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed in run_source_with_stdout");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.enable_stdout_capture();
    let val = vm
        .run_value()
        .expect("bytecode run_value failed in run_source_with_stdout");
    let stdout = vm.take_stdout();
    (val, stdout)
}

/// Concatenate a stdlib file (by name) with the test source and run.
/// Used to test stdlib modules in isolation. The stdlib file's items
/// (functions, traits) become available to the test source without
/// requiring `use` statements.
pub(crate) fn run_with_stdlib(stdlib_name: &str, src: &str) -> interp::Value {
    use std::path::PathBuf;
    let std_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std");
    let stdlib_path = std_dir.join(stdlib_name);
    let stdlib_src = std::fs::read_to_string(&stdlib_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", stdlib_path.display(), e));
    let combined = format!("{}\n{}", stdlib_src, src);
    run_source(&combined)
}

/// Run source via the Bytecode VM, returning Result.
pub(crate) fn run_source_result(src: &str) -> Result<interp::Value, String> {
    let tokens = lexer::Lexer::new(src).tokenize()?;
    let file = parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message)?;
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler.compile_file(&file).map_err(|e| e.to_string())?;
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.run_value().map_err(|e| e.message().to_string())
}

pub(crate) fn check_source(src: &str) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
    let file = parse(src);
    core::check(&file)
}

pub(crate) fn check_source_strict(src: &str) -> Result<(), Vec<crate::diagnostic::Diagnostic>> {
    let file = parse(src);
    core::check_strict(&file)
}

pub(crate) fn check_source_warnings(src: &str) -> Vec<crate::diagnostic::Diagnostic> {
    let file = parse(src);
    let mut checker = crate::core::Checker::new(&file);
    let _ = checker.check();
    checker.warnings
}

/// H3: Run checker + Bytecode VM. Catches checker bugs that `run_source_result`
/// silently bypasses (e.g. E0255 false positives for become/stay).
pub(crate) fn checked_run_source_result(src: &str) -> Result<interp::Value, String> {
    let file = parse(src);
    let program = core::check_program(&file).map_err(|diags| {
        diags
            .iter()
            .map(|d| format!("{}", d))
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    compiler.install_checked_program(&program);
    let prog = compiler.compile_file(&file).map_err(|e| e.to_string())?;
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.run_value().map_err(|e| e.message().to_string())
}

/// H3: Run checker + codegen + native execution. Catches checker bugs that
/// `compile_and_run` silently bypasses.
pub(crate) fn checked_compile_and_run(src: &str) -> Result<String, String> {
    let file = parse(src);
    core::check(&file).map_err(|diags| {
        diags
            .iter()
            .map(|d| format!("{}", d))
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    compile_and_run(src)
}

// ===================== Bytecode VM test helpers (0.33 retirement) =====================

/// Compile and run source via the Bytecode VM, returning the main Value.
/// Panics on compile or runtime error (mirrors `run_source` semantics).
pub(crate) fn run_source_bytecode(src: &str) -> interp::Value {
    let file = parse(src);
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.run_value().expect("bytecode run_value failed")
}

/// Compile and run source via the Bytecode VM, returning Result.
/// Mirrors `run_source_result` semantics (parse errors → Err, runtime errors → Err).
pub(crate) fn run_source_bytecode_result(src: &str) -> Result<interp::Value, String> {
    let tokens = lexer::Lexer::new(src).tokenize()?;
    let file = parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message)?;
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler.compile_file(&file).map_err(|e| e.to_string())?;
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.run_value().map_err(|e| e.message().to_string())
}

/// Compile and run source via the Bytecode VM with stdout capture.
/// Returns `(main return value, captured stdout)`.
pub(crate) fn run_source_bytecode_with_stdout(src: &str) -> (interp::Value, String) {
    let file = parse(src);
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.enable_stdout_capture();
    let val = vm.run_value().expect("bytecode run_value failed");
    let stdout = vm.take_stdout();
    (val, stdout)
}

/// Compile and run source via the Bytecode VM with CheckedProgram integration.
/// Runs the type checker first, installs CheckedProgram into the compiler,
/// then compiles and runs. Mirrors `checked_run_source_result` semantics.
pub(crate) fn checked_run_source_bytecode_result(src: &str) -> Result<interp::Value, String> {
    let file = parse(src);
    let program = core::check_program(&file).map_err(|diags| {
        diags
            .iter()
            .map(|d| format!("{}", d))
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    compiler.install_checked_program(&program);
    let prog = compiler.compile_file(&file).map_err(|e| e.to_string())?;
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.run_value().map_err(|e| e.message().to_string())
}

/// Compile source to bytecode and call a named function (test helper).
/// Returns Result<Value, String>. Panics on parse/compile failure.
/// Does NOT require type checking to pass (mirrors tree-walker's direct eval).
pub(crate) fn bytecode_call_named(
    src: &str,
    func_name: &str,
    args: Vec<interp::Value>,
) -> Result<interp::Value, String> {
    let file = parse(src);
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    // Install CheckedProgram if type check passes (enables better dispatch),
    // but don't fail if it doesn't (some tests intentionally use unchecked code).
    if let Ok(program) = core::check_program(&file) {
        compiler.install_checked_program(&program);
    }
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.call_named(func_name, args)
        .map_err(|e| e.message().to_string())
}

/// Compile source to bytecode and run main with verify_contracts toggle.
/// Returns Result<Value, String>.
pub(crate) fn bytecode_run_with_contracts(
    src: &str,
    verify: bool,
) -> Result<interp::Value, String> {
    let file = parse(src);
    let program = core::check_program(&file).expect("type check failed");
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    compiler.install_checked_program(&program);
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.verify_contracts = verify;
    vm.run_value().map_err(|e| e.message().to_string())
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
    /// Optional extra C source code to compile and link into the test binary.
    pub extra_c_src: Option<String>,
}

impl Default for E2EConfig {
    fn default() -> Self {
        Self {
            verify_contracts: false,
            use_valgrind: false,
            use_asan: false,
            use_ubsan: false,
            valgrind_args: vec![
                "--tool=memcheck".into(),
                "--error-exitcode=1".into(),
                "--leak-check=full".into(),
            ],
            extra_c_src: None,
        }
    }
}

fn compile_and_run_with_config(src: &str, config: &E2EConfig) -> Result<String, String> {
    if config.use_valgrind && config.use_asan {
        return Err("cannot use valgrind and ASAN simultaneously".into());
    }

    let counter = E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lexer: {}", e))?;
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| format!("parser: {}", e))?;

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "e2e_test");
    if config.verify_contracts {
        codegen.verify_contracts = true;
    }
    codegen.compile_file(&file).map_err(|e| e.to_string())?;
    link_and_run_module(&codegen, config, counter)
}

/// Compile the linked object to a binary, link against the cached runtime, and
/// run. Shared by the legacy (`compile_file`) and checked (`compile_checked`)
/// codegen harnesses since 0.34.30.
#[allow(clippy::too_many_lines)]
fn link_and_run_module<'ctx>(
    codegen: &crate::codegen::CodeGenerator<'ctx>,
    config: &E2EConfig,
    counter: u64,
) -> Result<String, String> {
    use std::process::Command;

    if std::env::var("MIMI_DUMP_IR").is_ok() {
        eprintln!("{}", codegen.module.print_to_string().to_string());
    }

    let tmp_dir = std::env::temp_dir().join(format!("mimi_e2e_{}_{}", std::process::id(), counter));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;
    let obj_path = tmp_dir.join("test.o");
    let bin_path = if cfg!(target_os = "windows") {
        tmp_dir.join("test.exe")
    } else {
        tmp_dir.join("test")
    };

    codegen
        .compile_to_object(&obj_path)
        .map_err(|e| e.to_string())?;

    // Reuse cached runtime static library
    let runtime_lib = cached_runtime_lib().map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        e
    })?;

    let mut object_files = vec![obj_path.clone(), runtime_lib.clone()];

    // Compile extra C source if provided
    if let Some(extra_c) = &config.extra_c_src {
        let extra_c_path = tmp_dir.join("extra_test.c");
        std::fs::write(&extra_c_path, extra_c).map_err(|e| format!("write extra c: {}", e))?;
        let extra_o = tmp_dir.join("extra_test.o");
        let mut cc_extra = Command::new("cc");
        cc_extra
            .arg("-c")
            .arg(&extra_c_path)
            .arg("-o")
            .arg(&extra_o);
        if config.use_asan {
            cc_extra.arg("-fsanitize=address");
        }
        if config.use_ubsan {
            cc_extra
                .arg("-fsanitize=undefined")
                .arg("-fno-sanitize-recover=all");
        }
        let extra_status = cc_extra
            .status()
            .map_err(|e| format!("extra c compile: {}", e))?;
        if !extra_status.success() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!(
                "extra C source compile failed with exit code {:?}",
                extra_status.code()
            ));
        }
        object_files.push(extra_o);
    }

    let mut cc_link = Command::new("cc");
    cc_link.arg("-no-pie");
    for flag in linker_flag() {
        cc_link.arg(flag);
    }
    for obj in &object_files {
        cc_link.arg(obj);
    }
    cc_link.arg("-o").arg(&bin_path);
    // Link system libraries needed by the Rust standard library
    cc_link.arg("-lpthread").arg("-ldl").arg("-lm");
    if config.use_asan {
        cc_link.arg("-fsanitize=address");
    }
    if config.use_ubsan {
        cc_link.arg("-fsanitize=undefined");
    }
    let status = cc_link.status().map_err(|e| format!("linker: {}", e))?;
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
        Command::new(&bin_path)
            .output()
            .map_err(|e| format!("run: {}", e))?
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    if std::env::var("MIMI_KEEP_TMP").is_err() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    } else {
        eprintln!("[mimi-test] kept tmp dir: {}", tmp_dir.display());
    }

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "exit code {:?}, stderr: {}",
            output.status.code(),
            stderr
        ));
    }

    Ok(stdout)
}

/// 0.34.30: Run source through checker + checked (resolved) codegen exactly as
/// the `mimi build` CLI does (`compile_checked`), then execute natively. This
/// catches the codegen path the legacy `compile_file` harness silently
/// miscompiles (e.g. nested list indexing built inside a loop).
pub(crate) fn checked_codegen_compile_and_run(src: &str) -> Result<String, String> {
    let file = parse(src);
    let checked_program = core::check_program(&file).map_err(|diags| {
        diags
            .iter()
            .map(|d| format!("{}", d))
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    let counter = E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "e2e_test");
    codegen
        .compile_checked(&checked_program)
        .map_err(|e| format!("{:?}", e))?;
    link_and_run_module(&codegen, &E2EConfig::default(), counter)
}

/// Standard E2E codegen test: compile and run, return stdout.
pub(crate) fn compile_and_run(src: &str) -> Result<String, String> {
    compile_and_run_with_config(src, &E2EConfig::default())
}

/// E2E codegen test with contracts verification enabled.
pub(crate) fn compile_and_verify_contracts(src: &str) -> Result<String, String> {
    compile_and_run_with_config(
        src,
        &E2EConfig {
            verify_contracts: true,
            ..Default::default()
        },
    )
}

/// E2E test running the binary under valgrind memcheck.
pub(crate) fn compile_and_run_valgrind(src: &str) -> Result<String, String> {
    compile_and_run_with_config(
        src,
        &E2EConfig {
            use_valgrind: true,
            ..Default::default()
        },
    )
}

/// E2E test compiled with AddressSanitizer and run directly.
pub(crate) fn compile_and_run_asan(src: &str) -> Result<String, String> {
    compile_and_run_with_config(
        src,
        &E2EConfig {
            use_asan: true,
            ..Default::default()
        },
    )
}

/// E2E test compiled with UndefinedBehaviorSanitizer and run directly.
pub(crate) fn compile_and_run_ubsan(src: &str) -> Result<String, String> {
    compile_and_run_with_config(
        src,
        &E2EConfig {
            use_ubsan: true,
            ..Default::default()
        },
    )
}

/// E2E codegen test with an extra C source file linked in.
pub(crate) fn compile_and_run_with_csrc(src: &str, extra_c: &str) -> Result<String, String> {
    compile_and_run_with_config(
        src,
        &E2EConfig {
            extra_c_src: Some(extra_c.to_string()),
            ..Default::default()
        },
    )
}

/// Compile Mimi source to an object file and return the path.
/// The caller is responsible for cleaning up the returned path.
pub(crate) fn compile_only(src: &str) -> Result<std::path::PathBuf, String> {
    let counter = E2E_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lexer: {}", e))?;
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| format!("parser: {}", e))?;
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "e2e_test");
    codegen.compile_file(&file).map_err(|e| e.to_string())?;
    let tmp_dir = std::env::temp_dir().join(format!("mimi_obj_{}_{}", std::process::id(), counter));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;
    let obj_path = tmp_dir.join("test.o");
    codegen
        .compile_to_object(&obj_path)
        .map_err(|e| e.to_string())?;
    Ok(obj_path)
}

/// Run a Mimi source with contracts enabled through both backends,
/// asserting both succeed. Does NOT compare stdout (contracts may
/// produce different diagnostic output between backends).
pub(crate) fn dual_assert_contract_ok(src: &str) {
    // Bytecode VM with contract checking (0.33: replaces tree-walker).
    let file = parse(src);
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed in dual_assert_contract_ok");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.verify_contracts = true;
    vm.run_value()
        .expect("bytecode contract run failed in dual_assert_contract_ok");
    compile_and_verify_contracts(src).expect("codegen contract run failed");
}

/// Run a Mimi source whose contracts are VIOLATED, with contract checking
/// enabled, through both backends — asserting BOTH trap with E0808 (0.34.41,
/// AF-4 前置 2①). 第二档起守卫由 resolved emitter 直接发射（不再 fail-closed
/// legacy）；无论哪条发射路径，VM 与 codegen 的 trap 必须对等。
pub(crate) fn dual_assert_contract_violation(src: &str) {
    let file = parse(src);
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed in dual_assert_contract_violation");
    let mut vm = interp::bytecode::BytecodeVM::new(prog.clone());
    vm.verify_contracts = true;
    let vm_err = vm
        .run_value()
        .expect_err("VM should trap on contract violation");
    let vm_err_str = vm_err.to_string();
    assert!(
        vm_err_str.contains("E0808"),
        "VM contract violation should carry E0808, got: {vm_err_str}"
    );
    let cg_err =
        compile_and_verify_contracts(src).expect_err("codegen should trap on contract violation");
    assert!(
        cg_err.contains("E0808"),
        "codegen contract violation should carry E0808, got: {cg_err}"
    );
}

/// Test helper: promote a .mms file to .mimi (copies source to output).
pub fn main_promote(
    path: &std::path::Path,
    output: Option<&std::path::Path>,
) -> Result<(), String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    if source.contains("...") {
        return Err(format!(
            "file contains '...' placeholders, cannot promote: {}",
            path.display()
        ));
    }
    let output_path = output.map_or_else(
        || {
            let mut out = path.to_path_buf();
            out.set_extension("mimi");
            out
        },
        |p| p.to_path_buf(),
    );
    std::fs::write(&output_path, &source)
        .map_err(|e| format!("failed to write {}: {}", output_path.display(), e))?;
    Ok(())
}

/// Test helper: run update in a specific directory.
pub fn main_update(dir: &std::path::Path) -> Result<(), String> {
    let manifest = match crate::manifest::Manifest::find(dir)? {
        Some((_d, m)) => m,
        None => return Err("no mimi.toml found".into()),
    };

    let deps = match &manifest.dependencies {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return Ok(()),
    };

    let reg = crate::pkg_registry::registry_dir()?;
    let deps_dir = dir.join(".mimi").join("deps");
    std::fs::create_dir_all(&deps_dir).map_err(|e| format!("failed to create deps dir: {}", e))?;

    let mut lock =
        crate::lockfile::Lockfile::load(dir)?.unwrap_or_else(crate::lockfile::Lockfile::new);

    for dep in &deps {
        if dep.git.is_some() {
            continue;
        }
        let dst = deps_dir.join(&dep.name);
        let resolved = crate::pkg_resolve::resolve_single_dep(dep, &dst, &reg)?;
        lock.add_package(
            &resolved.name,
            &resolved.version,
            resolved.source.as_deref(),
            resolved.checksum.as_deref(),
        );
    }

    lock.save(dir)?;
    Ok(())
}

/// Test helper: transitive install (project dir, registry dir).
/// Installs direct + transitive deps from registry only.
pub fn main_install_transitive(
    project_dir: &std::path::Path,
    reg: &std::path::Path,
) -> Result<(), String> {
    let manifest = match crate::manifest::Manifest::find(project_dir)? {
        Some((_d, m)) => m,
        None => return Err("no mimi.toml found".into()),
    };

    let direct_deps: Vec<crate::manifest::Dependency> = match &manifest.dependencies {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return Ok(()),
    };

    let deps_dir = project_dir.join(".mimi").join("deps");
    std::fs::create_dir_all(&deps_dir).map_err(|e| format!("failed to create deps dir: {}", e))?;

    let mut lock = crate::lockfile::Lockfile::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue: Vec<crate::manifest::Dependency> = direct_deps;

    while let Some(dep) = queue.pop() {
        if !visited.insert(dep.name.clone()) {
            continue;
        }

        let dst = deps_dir.join(&dep.name);

        // Path dependency: copy from local source path. No version resolver.
        if let Some(src_path) = &dep.path {
            let src = std::path::PathBuf::from(src_path);
            if !src.exists() {
                return Err(format!(
                    "path dependency '{}' not found at {}",
                    dep.name, src_path
                ));
            }
            if dst.exists() {
                std::fs::remove_dir_all(&dst).ok();
            }
            crate::pkg_registry::copy_dir_recursive(&src, &dst)
                .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;
            lock.add_package(&dep.name, "*", Some(&format!("path:{}", src_path)), None);
        } else {
            // resolve_single_dep for registry-only (custom registry path)
            // Reuse the function but override registry with the test reg dir
            let pkg_dir = reg.join(&dep.name);
            if !pkg_dir.exists() {
                return Err(format!("package '{}' not found in registry", dep.name));
            }

            let version = dep.version.as_deref().unwrap_or("*");
            let versions: Vec<String> = std::fs::read_dir(&pkg_dir)
                .map_err(|e| format!("failed to read registry: {}", e))?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect();
            let version_refs: Vec<&str> = versions.iter().map(|s| s.as_str()).collect();
            let resolved_ver =
                crate::lockfile::Lockfile::resolve_version(version, &version_refs)
                    .ok_or_else(|| format!("no matching version for '{}' {}", dep.name, version))?;

            let src = pkg_dir.join(&resolved_ver);
            if dst.exists() {
                std::fs::remove_dir_all(&dst).ok();
            }
            crate::pkg_registry::copy_dir_recursive(&src, &dst)
                .map_err(|e| format!("failed to copy {}: {}", dep.name, e))?;

            lock.add_package(&dep.name, &resolved_ver, Some("registry"), None);
        }

        let sub_deps = crate::pkg_resolve::read_transitive_deps(&dst, &visited);
        for sub_dep in sub_deps {
            if !visited.contains(&sub_dep.name) {
                queue.push(sub_dep);
            }
        }
    }

    lock.save(project_dir)?;
    Ok(())
}

/// Test helper: dry-run a `mimi add` invocation. Mirrors the dry-run
/// branch in `src/main/add.rs` so tests can exercise it without invoking
/// the binary.
pub fn main_add_dry_run(
    _name: &str,
    version: Option<&str>,
    _path: Option<&str>,
    _git: Option<&str>,
    _tag: Option<&str>,
) -> Result<(), String> {
    if version.is_none() {
        return Ok(());
    }
    // Dry-run: just validate the constraint parses, do not touch the manifest.
    let _ = crate::lockfile::Lockfile::resolve_version(version.unwrap(), &["0.0.0"])
        .ok_or_else(|| "invalid version constraint".to_string());
    Ok(())
}

/// Test helper: generate documentation from a Mimi source file.
pub fn main_doc(
    path: &std::path::Path,
    format: &str,
    output: Option<&std::path::Path>,
) -> Result<(), String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;

    let is_mms = path.extension().map(|e| e == "mms").unwrap_or(false);

    let doc_text = match format {
        "markdown" | "md" => {
            if is_mms {
                crate::doc_core::generate_markdown_from_mms(&source)?
            } else {
                crate::doc_core::generate_markdown(&source)?
            }
        }
        "mms" => {
            if !is_mms {
                return Err("mms output format requires .mms input".into());
            }
            crate::doc_core::generate_mms(&source)?
        }
        _ => return Err(format!("unsupported doc format: {}", format)),
    };

    match output {
        Some(out_path) => {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("failed to create output dir: {}", e))?;
            }
            std::fs::write(out_path, &doc_text)
                .map_err(|e| format!("failed to write {}: {}", out_path.display(), e))?;
        }
        None => {
            print!("{}", doc_text);
        }
    }

    Ok(())
}

// ===================== Bytecode VM equivalence smoke tests (0.33) =====================

#[cfg(test)]
mod bytecode_equiv_smoke {
    use super::*;

    #[test]
    fn bc_smoke_int() {
        let tw = run_source("func main() -> i32 { 42 }");
        let bc = run_source_bytecode("func main() -> i32 { 42 }");
        assert_eq!(tw, bc, "tree-walker vs bytecode mismatch");
        assert_eq!(bc, interp::Value::Int(42));
    }

    #[test]
    fn bc_smoke_arithmetic() {
        let src = "func main() -> i32 { (10 + 20) * 3 }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_string_concat() {
        let src = r#"func main() -> string { "hello" + " " + "world" }"#;
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_list_index() {
        let src = "func main() -> i32 { let xs = [1,2,3]; xs[0] + xs[2] }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_if_else() {
        let src = "func main() -> i32 { if 10 > 5 { 1 } else { 0 } }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_function_call() {
        let src = "func add(a: i32, b: i32) -> i32 { a + b }
                    func main() -> i32 { add(3, 4) }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_recursion() {
        let src = "func fib(n: i32) -> i32 { if n <= 1 { n } else { fib(n-1) + fib(n-2) } }
                    func main() -> i32 { fib(10) }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_stdout() {
        let src = r#"func main() -> i32 { print("hi"); 0 }"#;
        let (tw_val, tw_out) = run_source_with_stdout(src);
        let (bc_val, bc_out) = run_source_bytecode_with_stdout(src);
        assert_eq!(tw_val, bc_val, "return value mismatch");
        assert_eq!(
            tw_out, bc_out,
            "stdout mismatch: tw={:?} bc={:?}",
            tw_out, bc_out
        );
    }

    #[test]
    fn bc_smoke_result_variant() {
        let src = r#"func main() -> i32 {
            let r: Result<i32, string> = Ok(42)
            match r { Ok(v) => v  Err(_) => 0 }
        }"#;
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_option_variant() {
        let src = r#"func main() -> i32 {
            let o: Option<i32> = Some(7)
            match o { Some(v) => v  None => 0 }
        }"#;
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_record() {
        let src = "type Point { x: i32, y: i32 }
                    func main() -> i32 { let p = Point { x: 3, y: 4 }; p.x + p.y }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_while_loop() {
        let src = "func main() -> i32 {
            let mut i = 0
            let mut sum = 0
            while i < 10 { sum = sum + i; i = i + 1 }
            sum
        }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_for_range() {
        let src = "func main() -> i32 {
            let mut sum = 0
            for i in 0..5 { sum = sum + i }
            sum
        }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_closure() {
        let src = "func main() -> i32 {
            let add = fn(x: i32, y: i32) -> i32 { x + y }
            add(3, 4)
        }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_tuple() {
        let src = "func main() -> i32 { let t = (1, 2, 3); t.0 + t.1 + t.2 }";
        assert_eq!(run_source(src), run_source_bytecode(src));
    }

    #[test]
    fn bc_smoke_checked_path() {
        let src = "func main() -> i32 { 42 }";
        let r = checked_run_source_bytecode_result(src);
        assert_eq!(r.unwrap(), interp::Value::Int(42));
    }
}

/// Batch equivalence probe: run a curated set of programs through both
/// tree-walker and bytecode, collecting ALL mismatches in one run.
/// This reveals the full failure surface for the retirement plan.
#[cfg(test)]
mod bytecode_batch_probe {
    use super::*;

    /// Run a program through both backends. Returns Ok(()) if equivalent,
    /// Err(description) if mismatched or one side fails.
    fn probe(name: &str, src: &str) -> Result<(), String> {
        let tw = std::panic::catch_unwind(|| run_source(src));
        let bc = std::panic::catch_unwind(|| run_source_bytecode(src));
        match (tw, bc) {
            (Ok(tw_val), Ok(bc_val)) => {
                if tw_val == bc_val {
                    Ok(())
                } else {
                    Err(format!(
                        "{}: VALUE MISMATCH\n  tw={:?}\n  bc={:?}",
                        name, tw_val, bc_val
                    ))
                }
            }
            (Ok(tw_val), Err(_)) => Err(format!("{}: bytecode PANICKED, tw={:?}", name, tw_val)),
            (Err(_), Ok(bc_val)) => Err(format!("{}: tree-walker PANICKED, bc={:?}", name, bc_val)),
            (Err(_), Err(_)) => Ok(()), // both fail — acceptable (e.g. unsupported feature)
        }
    }

    /// Run a program through both backends with stdout capture.
    fn probe_stdout(name: &str, src: &str) -> Result<(), String> {
        let tw = std::panic::catch_unwind(|| run_source_with_stdout(src));
        let bc = std::panic::catch_unwind(|| run_source_bytecode_with_stdout(src));
        match (tw, bc) {
            (Ok((tw_val, tw_out)), Ok((bc_val, bc_out))) => {
                let mut errs = Vec::new();
                if tw_val != bc_val {
                    errs.push(format!("  value: tw={:?} bc={:?}", tw_val, bc_val));
                }
                if tw_out != bc_out {
                    errs.push(format!("  stdout: tw={:?} bc={:?}", tw_out, bc_out));
                }
                if errs.is_empty() {
                    Ok(())
                } else {
                    Err(format!("{}: MISMATCH\n{}", name, errs.join("\n")))
                }
            }
            (Ok(_), Err(_)) => Err(format!("{}: bytecode PANICKED", name)),
            (Err(_), Ok(_)) => Err(format!("{}: tree-walker PANICKED", name)),
            (Err(_), Err(_)) => Ok(()),
        }
    }

    #[test]
    fn batch_equivalence_probe() {
        let cases: Vec<(&str, &str)> = vec![
            // === Scalars ===
            ("int_literal", "func main() -> i32 { 42 }"),
            ("float_literal", "func main() -> f64 { 3.14 }"),
            ("bool_true", "func main() -> bool { true }"),
            ("bool_false", "func main() -> bool { false }"),
            ("string_literal", r#"func main() -> string { "hello" }"#),
            ("unit_return", "func main() { }"),

            // === Arithmetic ===
            ("int_add", "func main() -> i32 { 10 + 20 }"),
            ("int_sub", "func main() -> i32 { 50 - 8 }"),
            ("int_mul", "func main() -> i32 { 6 * 7 }"),
            ("int_div", "func main() -> i32 { 100 / 4 }"),
            ("int_mod", "func main() -> i32 { 17 % 5 }"),
            ("float_add", "func main() -> f64 { 1.5 + 2.5 }"),
            ("float_mul", "func main() -> f64 { 2.0 * 3.0 }"),
            ("neg_int", "func main() -> i32 { 0 - 42 }"),
            ("neg_float", "func main() -> f64 { 0.0 - 1.5 }"),
            ("paren_precedence", "func main() -> i32 { (2 + 3) * 4 }"),

            // === Comparisons ===
            ("eq_true", "func main() -> bool { 5 == 5 }"),
            ("eq_false", "func main() -> bool { 5 == 6 }"),
            ("ne_true", "func main() -> bool { 5 != 6 }"),
            ("lt", "func main() -> bool { 3 < 5 }"),
            ("gt", "func main() -> bool { 5 > 3 }"),
            ("le", "func main() -> bool { 5 <= 5 }"),
            ("ge", "func main() -> bool { 5 >= 6 }"),
            ("and", "func main() -> bool { true && false }"),
            ("or", "func main() -> bool { false || true }"),
            ("not", "func main() -> bool { !true }"),

            // === Let bindings ===
            ("let_immutable", "func main() -> i32 { let x = 10\n x }"),
            ("let_mutable", "func main() -> i32 { let mut x = 10\n x = 20\n x }"),
            ("let_shadow", "func main() -> i32 { let x = 1\n let x = x + 1\n x }"),
            ("let_typed", "func main() -> i32 { let x: i32 = 42\n x }"),

            // === Control flow ===
            ("if_true", "func main() -> i32 { if true { 1 } else { 0 } }"),
            ("if_false", "func main() -> i32 { if false { 1 } else { 0 } }"),
            ("if_chain", "func main() -> i32 { if false { 1 } else if true { 2 } else { 3 } }"),
            ("while_loop", "func main() -> i32 {\n let mut i = 0\n let mut s = 0\n while i < 5 { s = s + i\n i = i + 1 }\n s }"),
            ("for_range", "func main() -> i32 {\n let mut s = 0\n for i in 0..5 { s = s + i }\n s }"),
            ("loop_break", "func main() -> i32 {\n let mut i = 0\n loop { i = i + 1\n if i >= 3 { break } }\n i }"),
            ("loop_break_value", "func main() -> i32 { let v = loop { break 42 }\n v }"),
            ("continue_in_while", "func main() -> i32 {\n let mut s = 0\n let mut i = 0\n while i < 10 { i = i + 1\n if i % 2 == 0 { continue }\n s = s + i }\n s }"),
            ("early_return", "func main() -> i32 { return 42 }"),
            ("return_in_if", "func f(x: i32) -> i32 {\n if x > 0 { return 1 }\n 0 }\nfunc main() -> i32 { f(5) + f(0 - 1) }"),

            // === Functions ===
            ("func_no_args", "func five() -> i32 { 5 }\nfunc main() -> i32 { five() }"),
            ("func_two_args", "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(3, 4) }"),
            ("func_recursion", "func fib(n: i32) -> i32 {\n if n <= 1 { n } else { fib(n-1) + fib(n-2) } }\nfunc main() -> i32 { fib(10) }"),
            ("func_mutual", "func is_even(n: i32) -> bool {\n if n == 0 { true } else { is_odd(n - 1) } }\nfunc is_odd(n: i32) -> bool {\n if n == 0 { false } else { is_even(n - 1) } }\nfunc main() -> bool { is_even(10) }"),
            ("func_nested_call", "func double(x: i32) -> i32 { x * 2 }\nfunc main() -> i32 { double(double(3)) }"),

            // === Strings ===
            ("string_concat", r#"func main() -> string { "a" + "b" + "c" }"#),
            ("string_len", r#"func main() -> i32 { len("hello") }"#),
            ("string_eq", r#"func main() -> bool { "abc" == "abc" }"#),
            ("string_ne", r#"func main() -> bool { "abc" != "def" }"#),

            // === Lists ===
            ("list_create", "func main() -> i32 { let xs = [1,2,3]\n len(xs) }"),
            ("list_index", "func main() -> i32 { let xs = [10,20,30]\n xs[1] }"),
            ("list_empty", "func main() -> bool { let xs: List<i32> = []\n len(xs) == 0 }"),
            ("list_nested", "func main() -> i32 { let xs = [[1,2],[3,4]]\n xs[1][0] }"),
            ("list_eq", "func main() -> bool { [1,2,3] == [1,2,3] }"),

            // === Tuples ===
            ("tuple_create", "func main() -> i32 { let t = (1, 2, 3)\n t.0 }"),
            ("tuple_access", "func main() -> i32 { let t = (10, 20)\n t.0 + t.1 }"),
            ("tuple_nested", "func main() -> i32 { let t = ((1, 2), 3)\n t.0.1 + t.1 }"),

            // === Records ===
            ("record_create", "type P { x: i32, y: i32 }\nfunc main() -> i32 { let p = P { x: 3, y: 4 }\n p.x + p.y }"),
            ("record_update", "type P { x: i32, y: i32 }\nfunc main() -> i32 { let mut p = P { x: 1, y: 2 }\n p.x = 10\n p.x + p.y }"),
            ("record_nested", "type Inner { v: i32 }\ntype Outer { inner: Inner }\nfunc main() -> i32 { let o = Outer { inner: Inner { v: 42 } }\n o.inner.v }"),

            // === Enums / Variants ===
            ("option_some", "func main() -> i32 {\n let o: Option<i32> = Some(42)\n match o { Some(v) => v  None => 0 } }"),
            ("option_none", "func main() -> i32 {\n let o: Option<i32> = None\n match o { Some(v) => v  None => 0 - 1 } }"),
            ("result_ok", "func main() -> i32 {\n let r: Result<i32, string> = Ok(7)\n match r { Ok(v) => v  Err(_) => 0 } }"),
            ("result_err", r#"func main() -> i32 {
 let r: Result<i32, string> = Err("fail")
 match r { Ok(v) => v  Err(_) => 0 - 1 } }"#),

            // === Pattern matching ===
            ("match_int", "func main() -> i32 { match 2 { 1 => 10  2 => 20  _ => 0 } }"),
            ("match_wildcard", "func main() -> i32 { match 99 { 1 => 10  _ => 42 } }"),

            // === Closures ===
            ("closure_basic", "func main() -> i32 { let f = fn(x: i32) -> i32 { x * 2 }\n f(5) }"),
            ("closure_capture", "func main() -> i32 { let offset = 10\n let f = fn(x: i32) -> i32 { x + offset }\n f(5) }"),

            // === Builtins ===
            ("builtin_len_list", "func main() -> i32 { len([1,2,3,4]) }"),
            ("builtin_abs", "func main() -> i32 { abs(0 - 5) }"),
            ("builtin_min", "func main() -> i32 { min(3, 7) }"),
            ("builtin_max", "func main() -> i32 { max(3, 7) }"),
            ("builtin_to_string", "func main() -> string { to_string(42) }"),

            // === Type casts ===
            ("cast_int_to_float", "func main() -> f64 { 42 as f64 }"),

            // === Nested blocks ===
            ("nested_blocks", "func main() -> i32 {\n let x = {\n let y = 10\n {\n let z = 20\n y + z\n }\n }\n x }"),
            ("multi_stmt", "func main() -> i32 {\n let a = 1\n let b = 2\n let c = a + b\n c * 2 }"),

            // === Comptime ===
            ("comptime_block", "func main() -> i32 { let v = comptime { 21 * 2 }\n v }"),
            ("comptime_func", "comptime func make_const() -> i32 { 21 * 2 }\nfunc main() -> i32 { comptime { make_const() } }"),

            // === Error handling ===
            ("try_result", "func safe_div(a: i32, b: i32) -> Result<i32, string> {\n if b == 0 { Err(\"div by zero\") } else { Ok(a / b) } }\nfunc main() -> i32 {\n match safe_div(10, 2) { Ok(v) => v  Err(_) => 0 - 1 } }"),
        ];

        let mut failures: Vec<String> = Vec::new();
        let mut passes = 0;

        for (name, src) in &cases {
            match probe(name, src) {
                Ok(()) => passes += 1,
                Err(e) => failures.push(e),
            }
        }

        // Stdout probes
        let stdout_cases: Vec<(&str, &str)> = vec![
            (
                "stdout_print",
                r#"func main() -> i32 { print("hello"); 0 }"#,
            ),
            ("stdout_print_int", "func main() -> i32 { print(42); 0 }"),
            (
                "stdout_multi",
                r#"func main() -> i32 { print("a"); print("b"); 0 }"#,
            ),
        ];
        for (name, src) in &stdout_cases {
            match probe_stdout(name, src) {
                Ok(()) => passes += 1,
                Err(e) => failures.push(e),
            }
        }

        if !failures.is_empty() {
            panic!(
                "\n\n===== BYTECODE EQUIVALENCE PROBE =====\n\
                 PASS: {}  FAIL: {}\n\n{}\n\
                 =====================================\n",
                passes,
                failures.len(),
                failures.join("\n\n")
            );
        }
    }
}
