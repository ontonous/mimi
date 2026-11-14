/// Fuzz corpus seed inputs: edge-case Mimi source programs.
///
/// These exercise parser, typechecker, interpreter and codegen with
/// boundary conditions that are known to trigger edge cases.
use crate::tests::{check_source, compile_and_run, run_source, run_source_with_stdout};

fn assert_float_helper_seed(src: &str, expected: f64) {
    let (bytecode_value, bytecode_stdout) = run_source_with_stdout(src);
    assert_eq!(bytecode_value, crate::interp::Value::Int(0));
    let bytecode_float = bytecode_stdout
        .trim()
        .parse::<f64>()
        .expect("Bytecode helper output should be a float");
    assert_eq!(bytecode_float, expected);

    let native_stdout = compile_and_run(src).expect("native float helper seed should run");
    let native_float = native_stdout
        .trim()
        .parse::<f64>()
        .expect("native helper output should be a float");
    assert_eq!(native_float, expected);
}

fn assert_string_helper_seed(src: &str, expected: &str) {
    let expected_stdout = format!("{}\n", expected);
    let (bytecode_value, bytecode_stdout) = run_source_with_stdout(src);
    assert_eq!(bytecode_value, crate::interp::Value::Int(0));
    assert_eq!(bytecode_stdout, expected_stdout, "Bytecode helper output");

    let native_stdout = compile_and_run(src).expect("native string helper seed should run");
    assert_eq!(native_stdout, expected_stdout, "native helper output");
}

/// Empty program (no items).
#[test]
fn corpus_empty_file() {
    let src = "";
    let _ = check_source(src);
}

/// Main with no return value (unit return).
#[test]
fn corpus_unit_main() {
    let src = "func main() {}";
    assert!(check_source(src).is_ok());
}

/// Negative integer literals at boundaries.
#[test]
fn corpus_i64_boundaries() {
    let src = "func main() -> i64 { -9223372036854775807 - 1 }";
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Int(i64::MIN));
}

#[test]
fn corpus_max_i64() {
    let src = "func main() -> i64 { 9223372036854775807 }";
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Int(i64::MAX));
}

/// Float edge cases.
#[test]
fn corpus_float_zero() {
    let src = r#"
func corpus_float_value() -> f64 { 0.0 }
func main() -> i32 { println(corpus_float_value()); 0 }
"#;
    assert_float_helper_seed(src, 0.0);
}

#[test]
fn corpus_float_negative() {
    let src = r#"
func corpus_float_value() -> f64 { -2.5 }
func main() -> i32 { println(corpus_float_value()); 0 }
"#;
    assert_float_helper_seed(src, -2.5);
}

/// String edge cases.
#[test]
fn corpus_string_special_chars() {
    let src = r#"
func corpus_string_value() -> string { "tab:\tnewline:\nbackslash:\\quote:\"" }
func main() -> i32 { println(corpus_string_value()); 0 }
"#;
    assert_string_helper_seed(src, "tab:\tnewline:\nbackslash:\\quote:\"");
}

#[test]
fn corpus_string_multiline() {
    let src = "func corpus_string_value() -> string { \"line1\\nline2\" }\n\
               func main() -> i32 { println(corpus_string_value()); 0 }";
    assert_string_helper_seed(src, "line1\nline2");
}

/// Boolean operations.
#[test]
fn corpus_bool_not() {
    let src = "func main() -> bool { !false }";
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Bool(true));
}

/// List edge cases.
#[test]
fn corpus_list_large() {
    let src = "func main() -> i32 { let xs = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]; len(xs) }";
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Int(10));
}

#[test]
fn corpus_list_nested_deep() {
    let src = r#"
        func main() -> i32 {
            let xs = [[[[1]]]]
            xs[0][0][0][0]
        }
    "#;
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Int(1));
}

/// Tuple edge cases.
#[test]
fn corpus_tuple_many_elements() {
    let src = "func main() -> i32 { let t = (1, 2, 3, 4, 5, 6, 7, 8, 9, 10); t.9 }";
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Int(10));
}

/// Edge case: match with all wildcard.
#[test]
fn corpus_match_all_wildcard() {
    let src = r#"
        func main() -> i32 {
            let x = 42;
            match x { _ => 0 }
        }
    "#;
    assert!(check_source(src).is_ok());
}

/// Edge case: chained arithmetic.
#[test]
fn corpus_chained_arith() {
    let src = r#"
        func main() -> i32 {
            (1 + 2 * 3) - 4 / 2
        }
    "#;
    assert!(check_source(src).is_ok());
}

/// Edge case: multiple functions with mutual recursion.
#[test]
fn corpus_mutual_recursion() {
    let src = r#"
        func even(n: i32) -> bool {
            if n == 0 { true } else { odd(n - 1) }
        }
        func odd(n: i32) -> bool {
            if n == 0 { false } else { even(n - 1) }
        }
        func main() -> bool { even(10) }
    "#;
    assert!(check_source(src).is_ok());
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Bool(true));
}

/// Edge case: complex boolean short-circuit.
#[test]
fn corpus_boolean_short_circuit() {
    let src = r#"
        func main() -> bool { true && (false || true) }
    "#;
    let _ = run_source(src);
}

/// Edge case: nested tuple access.
#[test]
fn corpus_nested_tuple_access() {
    let src = r#"
        func main() -> i32 {
            let t = ((1, 2), (3, 4));
            let inner = t.1;
            inner.0
        }
    "#;
    let val = run_source(src);
    assert_eq!(val, crate::interp::Value::Int(3));
}
