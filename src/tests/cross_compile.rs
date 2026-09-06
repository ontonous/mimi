use crate::codegen::CodeGenerator;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// Verify that compile_to_object accepts a custom target triple and produces
/// an object file. This tests the LLVM codegen cross-compilation plumbing
/// without needing a cross-linker toolchain.
///
/// The `llvm18-host-dynamic` validation profile intentionally initializes only
/// the host X86 target because it is built against the installed LLVM 18
/// shared library without an all-target development wrapper. Keep this test
/// active under the normal multi-target profile and report it as not
/// applicable under the host-only profile.
#[cfg_attr(
    feature = "llvm18-host-dynamic",
    ignore = "host-only LLVM validation profile does not initialize cross targets"
)]
#[test]
fn cross_compile_windows_target_llvm_object() {
    let src = "func main() -> i32 { 42 }";
    let tokens = Lexer::new(src).tokenize().expect("lex");
    let file = Parser::new(tokens).parse_file().expect("parse");

    let context = inkwell::context::Context::create();
    let mut codegen = CodeGenerator::new(&context, "cross_test");
    codegen.target_triple = Some("x86_64-pc-windows-gnu".to_string());
    codegen.compile_file(&file).expect("compile");

    let tmp_dir = std::env::temp_dir().join(format!("mimi_cross_test_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let obj_path = tmp_dir.join("test.o");

    let result = codegen.compile_to_object(&obj_path);
    assert!(
        result.is_ok(),
        "compile_to_object with Windows target should succeed: {:?}",
        result
    );

    assert!(obj_path.exists(), "object file should exist");
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// Verify that cross-compiling to aarch64-linux-gnu also works.
#[cfg_attr(
    feature = "llvm18-host-dynamic",
    ignore = "host-only LLVM validation profile does not initialize cross targets"
)]
#[test]
fn cross_compile_aarch64_linux_object() {
    let src = "func main() -> i32 { 1 + 2 }";
    let tokens = Lexer::new(src).tokenize().expect("lex");
    let file = Parser::new(tokens).parse_file().expect("parse");

    let context = inkwell::context::Context::create();
    let mut codegen = CodeGenerator::new(&context, "cross_test_aarch64");
    codegen.target_triple = Some("aarch64-unknown-linux-gnu".to_string());
    codegen.compile_file(&file).expect("compile");

    let tmp_dir = std::env::temp_dir().join(format!("mimi_cross_aarch64_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let obj_path = tmp_dir.join("test.o");

    let result = codegen.compile_to_object(&obj_path);
    assert!(
        result.is_ok(),
        "compile_to_object with aarch64 target should succeed: {:?}",
        result
    );

    assert!(obj_path.exists(), "object file should exist");
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

/// Verify cross-compile with no_std mode and a custom target.
#[cfg_attr(
    feature = "llvm18-host-dynamic",
    ignore = "host-only LLVM validation profile does not initialize cross targets"
)]
#[test]
fn cross_compile_no_std_windows() {
    let src = "func main() -> i32 { 0 }";
    let tokens = Lexer::new(src).tokenize().expect("lex");
    let file = Parser::new(tokens).parse_file().expect("parse");

    let context = inkwell::context::Context::create();
    let mut codegen = CodeGenerator::new(&context, "cross_no_std");
    codegen.target_triple = Some("x86_64-pc-windows-gnu".to_string());
    codegen.no_std = true;
    codegen.compile_file(&file).expect("compile");

    let tmp_dir = std::env::temp_dir().join(format!("mimi_cross_no_std_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("mkdir");
    let obj_path = tmp_dir.join("test.o");

    let result = codegen.compile_to_object(&obj_path);
    assert!(
        result.is_ok(),
        "compile_to_object with no_std + Windows target should succeed: {:?}",
        result
    );

    assert!(obj_path.exists(), "object file should exist");
    let _ = std::fs::remove_dir_all(&tmp_dir);
}
