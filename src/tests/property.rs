use crate::ast::Type;
use crate::core::{self, fmt_type, same_type, is_int, is_numeric, is_bool, is_string};
use crate::tests::*;
use proptest::prelude::*;

// ── Type utility property tests (core/mod.rs) ──

/// Generate arbitrary types for property testing of type system utilities.
fn arb_type() -> impl Strategy<Value = Type> {
    let leaf = prop_oneof![
        Just(Type::Name("i32".into(), vec![])),
        Just(Type::Name("i64".into(), vec![])),
        Just(Type::Name("f64".into(), vec![])),
        Just(Type::Name("bool".into(), vec![])),
        Just(Type::Name("string".into(), vec![])),
        Just(Type::Name("unit".into(), vec![])),
        Just(Type::Name("unknown".into(), vec![])),
        Just(Type::Nothing),
        Just(Type::Allocator),
        Just(Type::RawString),
        Just(Type::Infer),
        Just(Type::Cap("read".into())),
        Just(Type::Cap("write".into())),
    ];
    leaf.prop_recursive(
        3,    // depth
        16,   // max nodes
        8,    // items per collection
        |inner| {
            prop_oneof![
                (inner.clone(),).prop_map(|(i,)| Type::Ref(None, Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::RefMut(None, Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::Option(Box::new(i))),
                (inner.clone(), inner.clone()).prop_map(|(ok, err)| Type::Result(Box::new(ok), Box::new(err))),
                (inner.clone(), inner.clone()).prop_map(|(l, r)| Type::ExternFunc(vec![l], Box::new(r))),
                (inner.clone(),).prop_map(|(i,)| Type::Shared(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::LocalShared(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::Weak(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::WeakLocal(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::RawPtr(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::RawPtrMut(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::CShared(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::CBorrow(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::CBorrowMut(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::CBuffer(Box::new(i))),
                (inner.clone(),).prop_map(|(i,)| Type::Slice(Box::new(i))),
                (inner.clone(), 0..8usize).prop_map(|(i, s)| Type::Array(Box::new(i), s)),
                prop::collection::vec(inner.clone(), 0..4).prop_map(Type::Tuple),
                prop::collection::vec(inner.clone(), 0..3)
                    .prop_map(|args| Type::Name("List".into(), args)),
                prop::collection::vec(inner.clone(), 0..3)
                    .prop_map(|args| Type::Name("Result".into(), args)),
                prop::collection::vec(inner.clone(), 0..3)
                    .prop_map(|args| Type::Name("Option".into(), args)),
                (prop::collection::vec(inner.clone(), 0..3), inner.clone())
                    .prop_map(|(args, ret)| Type::Func(args, Box::new(ret))),
                ("[a-z]{1,8}", inner.clone())
                    .prop_map(|(name, i)| Type::Newtype(name, Box::new(i))),
            ]
        },
    )
}

proptest! {
    /// fmt_type should never panic on any valid Type variant.
    #[test]
    fn fmt_type_never_panics(t in arb_type()) {
        let _ = fmt_type(&t);
    }

    /// same_type is reflexive: t == t for all types.
    #[test]
    fn same_type_is_reflexive(t in arb_type()) {
        prop_assert!(core::same_type(&t, &t), "same_type({:?}, {:?}) should be true", t, t);
    }

    /// same_type is symmetric: same_type(a,b) == same_type(b,a).
    #[test]
    fn same_type_is_symmetric(a in arb_type(), b in arb_type()) {
        prop_assert_eq!(core::same_type(&a, &b), core::same_type(&b, &a));
    }

    /// Infer is only compatible with itself, not with arbitrary types.
    /// This prevents Infer from leaking as a soundness hole where any type check passes.
    /// Note: Name("unknown", _) is a separate special placeholder that remains universally compatible.
    #[test]
    fn infer_only_compatible_with_itself(t in arb_type()) {
        let infer = Type::Infer;
        let also_infer = Type::Infer;
        prop_assert!(core::same_type(&infer, &also_infer), "Infer should be compatible with itself");
        // Infer should NOT be compatible with non-Infer types (except the "unknown" placeholder)
        let is_infer_or_unknown = matches!(&t, Type::Infer) || matches!(&t, Type::Name(n, _) if n == "unknown");
        if !is_infer_or_unknown {
            prop_assert!(!core::same_type(&infer, &t), "Infer should NOT be universally compatible with {:?}", t);
        }
    }

    /// unknown is compatible with any type.
    #[test]
    fn unknown_matches_only_unknown(t in arb_type()) {
        let unknown = Type::Name("unknown".into(), vec![]);
        // 'unknown' only matches another 'unknown' — single-sided match
        // would mask cascade errors downstream.
        if format!("{:?}", &t) == format!("{:?}", &unknown) {
            prop_assert!(core::same_type(&unknown, &t));
        } else {
            prop_assert!(!core::same_type(&unknown, &t),
                "unknown should NOT be compatible with {:?} — would mask type errors", t);
            prop_assert!(!core::same_type(&t, &unknown),
                "{:?} should NOT be compatible with unknown — would mask type errors", t);
        }
    }
}

// ── Type-checking fuzz tests: verify check_source never panics ──

/// Strategy for generating type-checkable expressions.
/// These are *syntactically* valid snippets that exercise the type checker.
fn arb_type_check_expr() -> impl Strategy<Value = String> {
    fn intv() -> impl Strategy<Value = i64> { -100i64..100i64 }
    fn boolv() -> impl Strategy<Value = bool> { any::<bool>() }
    fn strv() -> impl Strategy<Value = String> { "[a-z]{0,10}".prop_map(|s: String| s) }

    prop_oneof![
        // Literals
        intv().prop_map(|n| n.to_string()),
        boolv().prop_map(|b| b.to_string()),
        strv().prop_map(|s| format!("\"{}\"", s)),
        // Unary ops
        intv().prop_map(|n| format!("-{}", n)),
        boolv().prop_map(|b| format!("!{}", b)),
        // Binary ops on literals
        (intv(), intv()).prop_map(|(a, b)| format!("{} + {}", a, b)),
        (intv(), intv()).prop_map(|(a, b)| format!("{} * {}", a, b)),
        (intv(), intv()).prop_map(|(a, b)| format!("({} > {})", a, b)),
        (intv(), intv()).prop_map(|(a, b)| format!("({} == {})", a, b)),
        // Bool combinations
        (boolv(), boolv()).prop_map(|(a, b)| format!("{} && {}", a, b)),
        (boolv(), boolv()).prop_map(|(a, b)| format!("{} || {}", a, b)),
        // Division/modulo (may be zero)
        (intv(), "[1-9][0-9]{0,2}").prop_map(|(a, b): (i64, String)| format!("{} / {}", a, b)),
        (intv(), "[1-9][0-9]{0,2}").prop_map(|(a, b): (i64, String)| format!("{} % {}", a, b)),
        // String ops
        (strv(), strv()).prop_map(|(a, b): (String, String)| format!("\"{}\" + \"{}\"", a, b)),
        // Float ops
        any::<f64>().prop_map(|n| format!("{}", n)),
        // List literals
        prop::collection::vec(intv(), 0..5).prop_map(|vs| {
            let items: Vec<String> = vs.into_iter().map(|v| v.to_string()).collect();
            format!("[{}]", items.join(", "))
        }),
        // Block expressions (if-else)
        (boolv(), intv(), intv()).prop_map(|(c, a, b)| format!("if {} {{ {} }} else {{ {} }}", c, a, b)),
        // Function call with literal
        intv().prop_map(|n| format!("abs({})", n)),
    ]
}

proptest! {
    /// Type checker should never panic on randomly generated expressions,
    /// even if they are semantically invalid.
    #[test]
    fn type_check_never_panics(expr_str in arb_type_check_expr()) {
        let src = format!("func main() -> i64 {{ {} }}", expr_str);
        // Should not panic regardless of whether it passes or fails type checking
        let tokens = crate::lexer::Lexer::new(&src).tokenize();
        if let Ok(tokens) = tokens {
            if let Ok(file) = crate::parser::Parser::new(tokens).parse_file() {
                let _ = core::check(&file);
            }
        }
    }
}

/// Strategy for generating programs that can be compiled and run.
/// These produce syntactically valid Mimi programs with a main function.
fn arb_runnable_program() -> impl Strategy<Value = String> {
    fn intv() -> impl Strategy<Value = i64> { -50i64..50i64 }
    fn boolv() -> impl Strategy<Value = bool> { any::<bool>() }
    fn ident() -> impl Strategy<Value = String> { "[a-z][a-z0-9_]{0,6}".prop_map(|s: String| s) }

    // A local expression: literal, variable, or simple op
    let local_expr = prop_oneof![
        intv().prop_map(|n| n.to_string()),
        Just("x".into()),
        Just("y".into()),
        (intv(), intv()).prop_map(|(a, b)| format!("{} + {}", a, b)),
        (intv(), intv()).prop_map(|(a, b)| format!("{} * {}", a, b)),
    ];

    let stmt = prop_oneof![
        // let x = <expr>;
        (ident(), local_expr.clone()).prop_map(|(name, expr)| format!("let {} = {};", name, expr)),
        // let mut x = <expr>;
        (ident(), local_expr.clone()).prop_map(|(name, expr)| format!("let mut {} = {};", name, expr)),
        // x = <expr>;  (only assign to x, which is declared as mutable)
        (local_expr.clone()).prop_map(|expr| format!("x = {};", expr)),
    ];

    proptest::collection::vec(stmt, 0..6).prop_map(|stmts| {
        let body = if stmts.is_empty() {
            "0".to_string()
        } else {
            let mut s = stmts.join("\n    ");
            s.push_str("\n    x");
            s
        };
        format!("func main() -> i64 {{\n    let x = 0;\n    let mut y = 0;\n    {}\n}}", body)
    })
}

proptest! {
    /// Interpreter should never panic on random runnable programs.
    /// Runtime errors (overflow, div by zero) are acceptable — panics are not.
    #[test]
    fn interp_never_panics_on_runnable(src in arb_runnable_program()) {
        let lexer = crate::lexer::Lexer::new(&src);
        if let Ok(toks) = lexer.tokenize() {
            if let Ok(file) = crate::parser::Parser::new(toks).parse_file() {
                if core::check(&file).is_ok() {
                    let mut interp = crate::interp::Interpreter::new(&file);
                    let _ = interp.run();
                }
            }
        }
    }

    /// Interpreter never panics running float-literal programs.
    #[test]
    fn interp_never_panics_on_floats(src in any::<f64>()) {
        let s = format!("func main() -> f64 {{ {} }}", src);
        let lexer = crate::lexer::Lexer::new(&s);
        if let Ok(toks) = lexer.tokenize() {
            if let Ok(file) = crate::parser::Parser::new(toks).parse_file() {
                if core::check(&file).is_ok() {
                    let mut interp = crate::interp::Interpreter::new(&file);
                    let _ = interp.run();
                }
            }
        }
    }
}

// ── Interpreter correctness proptests ──

proptest! {
    #[test]
    fn interp_float_literal(n in -1e10f64..1e10f64) {
        // Skip NaN — NaN != NaN in equality check
        prop_assume!(!n.is_nan());
        let src = format!("func main() -> f64 {{ {} }}", n);
        if let crate::interp::Value::Float(result) = run_source(&src) {
            prop_assert!((result - n).abs() <= f64::EPSILON * n.abs().max(1.0),
                "float literal mismatch: expected {}, got {}", n, result);
        }
    }

    #[test]
    fn interp_float_negation(n in -1e5f64..1e5f64) {
        let src = format!("func main() -> f64 {{ -({}) }}", n);
        if let crate::interp::Value::Float(result) = run_source(&src) {
            prop_assert!((result - (-n)).abs() <= f64::EPSILON * n.abs().max(1.0),
                "float neg mismatch: expected {}, got {}", -n, result);
        }
    }

    #[test]
    fn interp_string_len(s in "[a-z]{0,20}") {
        let src = format!("func main() -> i32 {{ len(\"{}\") }}", s);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, s.len() as i64);
        }
    }

    #[test]
    fn interp_if_bool_identity(b in any::<bool>()) {
        let src = format!("func main() -> i32 {{ if {} {{ 1 }} else {{ 0 }} }}", b);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            let expected = if b { 1 } else { 0 };
            prop_assert_eq!(result, expected);
        }
    }
}

// ── Closet capture / borrow proptests ──

proptest! {
    #[test]
    fn closure_capture_simple(n in -100i64..100) {
        let src = format!(r#"
func main() -> i64 {{
    let x = {};
    let f = fn() -> i64 {{ x }};
    f()
}}"#, n);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, n);
        }
    }

    #[test]
    fn ref_identity(n in -100i64..100i64) {
        let src = format!(r#"
func main() -> i64 {{
    let x = {};
    let r = &x;
    *r
}}"#, n);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, n);
        }
    }

    #[test]
    fn list_literal_len(n in 0usize..8) {
        let items: Vec<String> = (0..n).map(|i| format!("{}", i)).collect();
        let list_str = items.join(", ");
        let src = format!("func main() -> i32 {{ len([{}]) }}", list_str);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, n as i64);
        }
    }
}

// ── Type system property tests ──

proptest! {
    /// subst_type_params should never change the type when no type params match.
    #[test]
    fn subst_type_params_noop_with_empty_map(t in arb_type()) {
        let empty_map = std::collections::HashMap::new();
        let result = core::subst_type_params(&t, &[], &empty_map);
        prop_assert_eq!(format!("{:?}", &result), format!("{:?}", &t),
            "subst_type_params with empty map changed the type");
    }

    /// fmt_type should produce the same result for same_type types.
    #[test]
    fn fmt_type_consistent_for_equal_types(a in arb_type(), b in arb_type()) {
        if core::same_type(&a, &b) {
            prop_assert_eq!(crate::core::fmt_type(&a), crate::core::fmt_type(&b),
                "same_type types should have the same fmt_type output");
        }
    }

    /// is_int, is_numeric, is_bool, is_string are mutually exclusive for base types.
    #[test]
    fn type_predicates_mutually_exclusive(t in arb_type()) {
        let int_count = [is_int(&t), is_numeric(&t), is_bool(&t), is_string(&t)]
            .iter().filter(|&&x| x).count();
        // A type can be is_int AND is_numeric (they overlap), but not bool+int, etc.
        prop_assert!(int_count <= 2, "too many true predicates for {:?}", t);
        if is_bool(&t) {
            prop_assert!(!is_int(&t) && !is_string(&t));
        }
    }
}

// ── Property tests for type-inference edge cases ──

proptest! {
    /// Arithmetic with matching types should succeed type-checking.
    #[test]
    fn arithmetic_matching_types(a in -100i64..100, b in -100i64..100) {
        let src = format!("func main() -> i64 {{ {} + {} }}", a, b);
        let result = check_source(&src);
        // Should pass: i64 + i64 is valid
        assert!(result.is_ok(), "arithmetic on matching types should pass: {:?}", result.err());
    }

    /// Comparison should work between integers.
    #[test]
    fn comparison_integers(a in -100i64..100, b in -100i64..100) {
        let src = format!("func main() -> bool {{ {} == {} }}", a, b);
        let result = check_source(&src);
        assert!(result.is_ok(), "int comparison should pass: {:?}", result.err());
    }

    /// Bool expression in if condition should type-check.
    #[test]
    fn bool_in_if_condition(b in any::<bool>()) {
        let src = format!("func main() -> i32 {{ if {} {{ 1 }} else {{ 0 }} }}", b);
        let result = check_source(&src);
        assert!(result.is_ok(), "if bool should pass: {:?}", result.err());
    }

    /// Not operator on bool should type-check.
    #[test]
    fn not_bool(b in any::<bool>()) {
        let src = format!("func main() -> bool {{ !{} }}", b);
        let result = check_source(&src);
        assert!(result.is_ok(), "!bool should pass: {:?}", result.err());
    }

    /// String concatenation works.
    #[test]
    fn string_concat(s1 in "[a-z]{0,5}", s2 in "[a-z]{0,5}") {
        let src = format!("func main() -> string {{ \"{}\" + \"{}\" }}", s1, s2);
        let result = check_source(&src);
        assert!(result.is_ok(), "string concat should pass: {:?}", result.err());
    }
}

// ── Borrow checker property tests ──

proptest! {
    /// NLL: A mutable borrow followed by an immutable borrow after last use should pass.
    #[test]
    fn nll_mut_then_imm_after_last_use(x in -100i64..100) {
        let src = format!(r#"
func main() -> i64 {{
    let mut x = {};
    let r = &mut x;
    *r = *r + 1;
    let r2 = &x;
    *r2
}}"#, x);
        let result = check_source(&src);
        // Should be ok: &mut borrow ends before & borrow
        assert!(result.is_ok(), "NLL: mut then imm after last use should pass: {:?}", result.err());
    }

    /// Mutable variable reassignment should type-check when types match.
    #[test]
    fn reassign_matching_types(n in -100i64..100) {
        let src = format!(r#"
func main() -> i64 {{
    let mut x = {};
    x = x + 1;
    x
}}"#, n);
        let result = check_source(&src);
        assert!(result.is_ok(), "reassign with same type should pass: {:?}", result.err());
    }
}

// ── Original property-based tests ──

proptest! {
    #[test]
    fn eval_int_literal(n in -1000i64..1000i64) {
        let src = format!("func main() -> i64 {{ {} }}", n);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, n);
        }
    }

    #[test]
    fn eval_int_addition(a in -100i64..100, b in -100i64..100) {
        let src = format!("func main() -> i64 {{ {} + {} }}", a, b);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, a.wrapping_add(b));
        }
    }

    #[test]
    fn eval_int_multiply(a in -50i64..50, b in -50i64..50) {
        let src = format!("func main() -> i64 {{ {} * {} }}", a, b);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, a.wrapping_mul(b));
        }
    }

    #[test]
    fn eval_int_negate(n in -1000i64..1000) {
        let src = format!("func main() -> i64 {{ -{} }}", n);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, n.wrapping_neg());
        }
    }

    #[test]
    fn eval_bool_not(b in any::<bool>()) {
        let src = format!("func main() -> bool {{ !{} }}", b);
        if let crate::interp::Value::Bool(result) = run_source(&src) {
            prop_assert_eq!(result, !b);
        }
    }

    #[test]
    fn eval_string_length(s in "[a-z]{0,50}") {
        let src = format!("func main() -> i64 {{ len(\"{}\") }}", s);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, s.len() as i64);
        }
    }

    #[test]
    fn eval_range_for_sum(n in 1i64..20) {
        let src = format!(r#"
func main() -> i64 {{
    let mut sum = 0;
    for i in 0..{} {{
        sum = sum + i;
    }}
    sum
}}"#, n);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            let expected = n * (n - 1) / 2;
            prop_assert_eq!(result, expected, "n={}", n);
        }
    }

    #[test]
    fn eval_if_else(a in -100i64..100, b in -100i64..100) {
        let src = format!("func main() -> i64 {{ if {} > 0 {{ {} }} else {{ {} }} }}", a, a, b);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            let expected = if a > 0 { a } else { b };
            prop_assert_eq!(result, expected);
        }
    }

    #[test]
    fn eval_while_loop(n in 0i64..50) {
        let src = format!(r#"
func main() -> i64 {{
    let mut i = 0;
    let mut sum = 0;
    while i < {} {{
        sum = sum + i;
        i = i + 1;
    }}
    sum
}}"#, n);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            let expected = n * (n - 1) / 2;
            prop_assert_eq!(result, expected, "n={}", n);
        }
    }

    #[test]
    fn eval_func_composition(a in -50i64..50) {
        let src = format!(r#"
func double(x: i64) -> i64 {{
    x * 2
}}
func add_one(x: i64) -> i64 {{
    x + 1
}}
func main() -> i64 {{
    double(add_one({}))
}}"#, a);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, (a + 1) * 2);
        }
    }

    #[test]
    fn eval_list_len(n in 0usize..20) {
        let items: Vec<String> = (0..n).map(|i| format!("{}", i)).collect();
        let list_str = items.join(", ");
        let src = format!("func main() -> i64 {{ len([{}]) }}", list_str);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, n as i64);
        }
    }

    #[test]
    fn eval_type_name_int(n in 1i64..100) {
        let src = format!("func main() -> string {{ type_name({}) }}", n);
        if let crate::interp::Value::String(result) = run_source(&src) {
            prop_assert!(result == "i64" || result == "i32", "unexpected type_name: {}", result);
        }
    }

    #[test]
    fn eval_pow(base in 0i64..10, exp in 0u32..6) {
        let src = format!("func main() -> i64 {{ pow({}, {}) }}", base, exp);
        if let crate::interp::Value::Int(result) = run_source(&src) {
            prop_assert_eq!(result, base.pow(exp));
        }
    }
}
