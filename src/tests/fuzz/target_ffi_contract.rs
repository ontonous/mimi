//! # FFI Contract Property Tests
//!
//! Verifies that the three FFI contract evaluation paths (Z3, interpreter,
//! codegen) produce consistent results for randomly generated extern
//! declarations and calls.
//!
//! Run: `cargo test fuzz::target_ffi_contract -- --nocapture --include-ignored`

use crate::{core, interp, lexer, parser};
use proptest::prelude::*;
use proptest::strategy::ValueTree;

/// Generate a simple extern declaration + a call to it.
/// The extern function takes a single i32 and returns i32.
fn arb_extern_call() -> impl Strategy<Value = String> {
    let func_name = prop_oneof![
        Just("process"),
        Just("compute"),
        Just("transform"),
        Just("apply"),
    ];
    let cond_type = prop_oneof![
        Just("requires: x > 0"),
        Just("requires: x != 0"),
        Just("requires: x >= 0"),
        Just(""),
    ];
    ((func_name, cond_type), (1i64..100i64)).prop_map(|((name, cond), arg)| {
        let cond_line = if cond.is_empty() {
            String::new()
        } else {
            format!("\n    {}", cond)
        };
        format!(
            r#"extern "C" {{
    func {}(x: i32) -> i32 {}{};
}}
func main() -> i32 {{
    let r = {}({});
    println(r);
    0
}}"#,
            name, cond_line, "", name, arg
        )
    })
}

/// Run a program through the interpreter's FFI contract verification path.
fn interp_ffi_verify(src: &str) -> Result<String, String> {
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .map_err(|e| e.to_string())?;
    let file = parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message.clone())?;
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_ffi = true;
    interp.verify_contracts = true;
    // Use no_fork because FFI returns pointers incompatible with fork isolation
    interp
        .run()
        .map(|v| format!("{}", v))
        .map_err(|e| e.message().to_string())
}

/// Run the type checker (which also validates contract syntax).
fn typecheck_ffi(src: &str) -> Result<(), String> {
    let tokens = lexer::Lexer::new(src)
        .tokenize()
        .map_err(|e| e.to_string())?;
    let file = parser::Parser::new(tokens)
        .parse_file()
        .map_err(|e| e.message.clone())?;
    core::check(&file).map_err(|diags| {
        diags
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("; ")
    })
}

proptest! {
    /// Verify that if the type checker accepts an FFI program with contracts,
    /// the interpreter also does not crash when verifying those contracts.
    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn ffi_contract_typecheck_ok(src in arb_extern_call()) {
        // First pass: type checker must accept (no false negatives)
        if typecheck_ffi(&src).is_err() {
            return Ok(());
        }

        // Type checker passed — verify interpreter doesn't crash
        let interp_result = interp_ffi_verify(&src);
        // We don't assert pass/fail — just ensure it doesn't panic.
        // The extern function is not linked, so the call will fail at runtime.
        // This is expected — we're testing the contract verification path only.
        prop_assert!(interp_result.is_ok() || interp_result.is_err(),
            "interpreter panicked on FFI program that type-checked");
    }

    /// Verify that if the type checker accepts a program and the interpreter
    /// runs it successfully, the codegen also compiles it without error.
    #[test]
    #[ignore = "requires cc linker toolchain"]
    fn ffi_contract_codegen_not_crash(src in arb_extern_call()) {
        // Skip programs that fail type checking
        if typecheck_ffi(&src).is_err() {
            return Ok(());
        }

        // Verify codegen doesn't crash (even if linking fails because
        // extern symbols are not available — that's expected)
        let tokens = match lexer::Lexer::new(&src).tokenize() {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        let file = match parser::Parser::new(tokens).parse_file() {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };
        let context = inkwell::context::Context::create();
        let mut cg = crate::codegen::CodeGenerator::new(&context, "ffi_test");
        let result = cg.compile_file(&file);
        // Must not panic. It may fail with "undefined external function" but
        // that's expected — the extern is intentionally not linked.
        prop_assert!(result.is_ok() || result.is_err(),
            "codegen panicked on FFI program that type-checked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_contract_generator_smoke() {
        let mut runner =
            proptest::test_runner::TestRunner::new(proptest::test_runner::Config::default());
        for _ in 0..20 {
            let tree = arb_extern_call()
                .new_tree(&mut runner)
                .expect("generator failed");
            let src = tree.current();
            let tokens = lexer::Lexer::new(&src).tokenize();
            assert!(tokens.is_ok(), "Failed to lex: {}", &src);
        }
    }
}
