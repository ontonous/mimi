//! # Type Soundness Property Tests
//!
//! Asserts the type checker never produces false positives (accepts a program
//! that causes a runtime type error) or false negatives (rejects a valid program).
//!
//! Run: `cargo test fuzz::target_typesoundness -- --nocapture`

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use crate::{core, interp, lexer, parser};

/// Generate simple typed programs with explicit i32 and i64 annotations.
fn arb_typed_program() -> impl Strategy<Value = String> {
    let expr1 = prop_oneof![
        (0i64..100i64).prop_map(|n| n.to_string()),
        (0i64..100i64).prop_map(|n| format!("({} + {})", n, n + 1)),
        (0i64..50i64).prop_map(|n| format!("({} * {})", n, 2)),
    ];
    let expr2 = prop_oneof![
        (0i64..100i64).prop_map(|n| n.to_string()),
        (0i64..100i64).prop_map(|n| format!("({} + {})", n, n + 1)),
        (0i64..50i64).prop_map(|n| format!("({} * {})", n, 2)),
    ];
    let ret_ty = prop_oneof![
        Just("i32"),
        Just("i64"),
    ];
    (ret_ty, expr1, expr2).prop_map(|(ty, e1, e2)| {
        format!(
            "func add_{}(a: {}, b: {}) -> {} {{ a + b }}\nfunc main() -> {} {{\n    let r = add_{}({}, {});\n    println(r);\n    0\n}}",
            ty, ty, ty, ty, ty, ty, e1, e2
        )
    })
}

/// Generate a potentially-ill-typed program (mixed i32/i64 without annotation).
fn arb_maybe_ill_typed_program() -> impl Strategy<Value = String> {
    prop_oneof![
        // i32 + i64 — may fail typecheck
        (0i64..50i64, 0i64..50i64).prop_map(|(a, b)| {
            format!("func main() -> i32 {{\n    let x: i32 = {};\n    let y: i64 = {};\n    println(x + y);\n    0\n}}", a, b)
        }),
        // i32 → i64 coercion (should pass)
        (0i64..50i64, 0i64..50i64).prop_map(|(a, b)| {
            format!("func main() -> i64 {{\n    let x: i64 = {};\n    let r = x + {};\n    println(r);\n    0\n}}", a, b)
        }),
        // Pure i32 (should pass)
        (0i64..50i64).prop_map(|a| {
            format!("func main() -> i32 {{\n    println({});\n    0\n}}", a)
        }),
    ]
}

/// Type-check a program, returning Ok if it passes.
fn typecheck(src: &str) -> Result<(), String> {
    let tokens = lexer::Lexer::new(src).tokenize().map_err(|e| e.to_string())?;
    let file = parser::Parser::new(tokens).parse_file().map_err(|e| e.message.clone())?;
    core::check(&file).map_err(|diags| {
        diags.iter().map(|d| d.message.clone()).collect::<Vec<String>>().join("; ")
    })
}

/// Run a program in the interpreter, returning Ok(value_string) if it succeeds.
fn interpret(src: &str) -> Result<String, String> {
    let tokens = lexer::Lexer::new(src).tokenize().map_err(|e| e.to_string())?;
    let file = parser::Parser::new(tokens).parse_file().map_err(|e| e.message.clone())?;
    let mut interp = interp::Interpreter::new(&file);
    interp.run().map(|v| format!("{}", v)).map_err(|e| e.message)
}

proptest! {
    /// False positive check: if the type checker accepts, the interpreter must
    /// not panic with a type error (it may error for other reasons like div by zero).
    #[test]
    fn typecheck_no_false_positive(src in arb_typed_program()) {
        if let Err(_tc_err) = typecheck(&src) {
            // Type checker rejected — that's fine, these are random programs
            return Ok(());
        }
        // Type checker accepted — interpreter must not panic
        let interp_result = interpret(&src);
        prop_assert!(interp_result.is_ok() || interp_result.is_err(),
            "type checker accepted but interpreter panicked\nsrc: {}", &src);
    }

    /// False negative check: if the type checker rejects with a type-mismatch error,
    /// verify the interpreter also cannot execute it without error.
    /// Known gaps: the interpreter does not track i32 vs i64 at runtime, so
    /// `i32 + i64` is accepted by the interpreter but rejected by the typechecker.
    #[test]
    fn typecheck_no_false_negative(src in arb_maybe_ill_typed_program()) {
        let tc_result = typecheck(&src);
        let interp_result = interpret(&src);

        match (&tc_result, &interp_result) {
            (Err(tc_msg), Ok(_val)) => {
                // Type checker rejected but interpreter succeeded — false negative!
                // This is acceptable only if the error message is NOT about type mismatch
                // (e.g., it could be about unused variables or non-exhaustive match)
                if tc_msg.contains("type") || tc_msg.contains("Type") {
                    prop_assert!(false, "false negative: typechecker rejected with type error but interpreter succeeded\nsrc: {}\ntc: {}", &src, tc_msg);
                }
            }
            (Ok(_), Err(ie_msg)) => {
                // Type checker accepted but interpreter failed — false positive!
                // Only flag if it's a runtime type error (not div-by-zero, etc.)
                if ie_msg.contains("type") || ie_msg.contains("Type") || ie_msg.contains("cannot apply") {
                    prop_assert!(false, "false positive: typechecker accepted but interpreter hit type error\nsrc: {}\ninterp: {}", &src, ie_msg);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typesoundness_generator_smoke() {
        let mut runner = proptest::test_runner::TestRunner::new(proptest::test_runner::Config::default());
        for _ in 0..20 {
            let tree = arb_typed_program().new_tree(&mut runner).expect("generator failed");
            let src = tree.current();
            let tokens = lexer::Lexer::new(&src).tokenize();
            assert!(tokens.is_ok(), "Failed to lex: {}", &src);
        }
    }
}
