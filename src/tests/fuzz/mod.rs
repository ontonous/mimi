//! # Mimi Fuzz Test Suite
//!
//! Rust-level fuzzing: 验证解释器与编译器的行为一致性，
//! 以及穷尽性匹配检查等完备性验证。

use crate::interp;
use crate::{core, lexer, parser};

/// 对一段随机 Mimi 代码进行双路径一致性检查
/// 注: 需要 `cc` 和运行时 C 文件可用
pub(crate) fn check_dual_path_consistency(src: &str) -> Result<String, String> {
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lexer: {}", e))?;
    let file = parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| format!("parser: {}", e))?;

    core::check(&file).map_err(|ds| {
        format!("type error: {:?}", ds)
    })?;

    let mut interp = interp::Interpreter::new(&file);
    let interp_result = interp
        .run()
        .map_err(|e| format!("interpreter error: {}", e.message))?;
    let interp_output = format!("{}", interp_result);

    let compiled_output = try_compile_and_run(&file)?;

    let interp_trimmed = interp_output.trim();
    let compiled_trimmed = compiled_output.trim();
    if interp_trimmed != compiled_trimmed {
        return Err(format!(
            "MISMATCH:\n  interpreter: {}\n  compiled:    {}\n  src: {}",
            interp_trimmed, compiled_trimmed, src
        ));
    }

    Ok(interp_output)
}

fn try_compile_and_run(file: &crate::ast::File) -> Result<String, String> {
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static E2E_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = E2E_COUNTER.fetch_add(1, Ordering::Relaxed);

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "fuzz_test");
    codegen.compile_file(file)?;

    let tmp_dir = std::env::temp_dir().join(format!("mimi_fuzz_{}_{}", std::process::id(), counter));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;
    let obj_path = tmp_dir.join("test.o");
    let bin_path = if cfg!(target_os = "windows") { tmp_dir.join("test.exe") } else { tmp_dir.join("test") };

    codegen.compile_to_object(&obj_path)?;

    let runtime_c = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/runtime/mimi_runtime.c");
    let runtime_o = tmp_dir.join("mimi_runtime.o");
    let rt_status = Command::new("cc")
        .arg("-c").arg(&runtime_c).arg("-o").arg(&runtime_o)
        .status()
        .map_err(|e| format!("runtime compile: {}", e))?;
    if !rt_status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("runtime compile failed".into());
    }

    let status = Command::new("cc")
        .arg("-no-pie").arg(&obj_path).arg(&runtime_o).arg("-o").arg(&bin_path)
        .status()
        .map_err(|e| format!("linker: {}", e))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("linker failed".into());
    }

    let output = Command::new(&bin_path)
        .output()
        .map_err(|e| format!("run: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("binary exit={:?}, stderr={}", output.status.code(), stderr));
    }

    Ok(stdout)
}

// ==============================
// 穷尽性检查测试
// ==============================

#[test]
fn test_exhaustive_wildcard_ok() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    match x {
        1 => 10,
        2 => 20,
        _ => 0,
    }
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn test_exhaustive_bool_complete() {
    let src = r#"
func main() -> i32 {
    let b = true;
    match b { true => 1, false => 0 }
}
"#;
    assert!(check_source(src).is_ok());
}

#[test]
fn test_exhaustive_enum_data_complete() {
    let src = r#"
type Opt { Some(i32) None }
func main() -> i32 {
    let x = Some(42);
    match x {
        Some(v) => v,
        None => 0,
    }
}
"#;
    assert!(check_source(src).is_ok());
}

// ==============================
// 双路径一致性测试 (需要 cc)
// ==============================

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_arithmetic() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    let y = 58;
    x + y
}
"#;
    let result = check_dual_path_consistency(src);
    assert!(result.is_ok(), "Dual-path mismatch: {:?}", result.err());
    assert_eq!(result.unwrap().trim(), "100");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_conditional() {
    let src = r#"
func main() -> i32 {
    let a = 10;
    let b = 20;
    if a > b { a - b } else { b - a }
}
"#;
    let result = check_dual_path_consistency(src);
    assert!(result.is_ok(), "Dual-path mismatch: {:?}", result.err());
    assert_eq!(result.unwrap().trim(), "10");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_loop_accumulate() {
    let src = r#"
func main() -> i32 {
    let mut s = 0;
    let i = 0;
    while i <= 5 {
        s = s + i;
        i = i + 1;
    };
    s
}
"#;
    let result = check_dual_path_consistency(src);
    assert!(result.is_ok(), "Dual-path mismatch: {:?}", result.err());
    assert_eq!(result.unwrap().trim(), "15");
}

#[test]
#[ignore = "requires cc linker toolchain"]
fn test_dual_path_simple_func() {
    let src = r#"
func square(x: i32) -> i32 { x * x }
func main() -> i32 { square(6) }
"#;
    let result = check_dual_path_consistency(src);
    assert!(result.is_ok(), "Dual-path mismatch: {:?}", result.err());
    assert_eq!(result.unwrap().trim(), "36");
}

// ==============================
// 线性能力检查测试
// ==============================

#[test]
fn test_cap_declaration_ok() {
    let src = r#"
cap FileReadCap;

func main() -> i32 { 42 }
"#;
    assert!(check_source(src).is_ok());
}

// ==============================
// FFI 合约验证测试 (不崩溃即可)
// ==============================

#[test]
fn test_ffi_verify_no_crash() {
    let src = r#"
extern "C" {
    func process(x: i32) -> i32;
}
func main() -> i32 { process(5) }
"#;
    let tokens = lexer::Lexer::new(src).tokenize().unwrap();
    let file = parser::Parser::new(tokens).parse_file().unwrap();
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_ffi = true;
    let _ = interp.run();
}

// Helper wrappers
use crate::tests::check_source;
