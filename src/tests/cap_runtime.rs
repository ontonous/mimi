use super::*;

#[test]
fn cap_declaration() {
    let src = r#"
cap FileRead;

func main() -> i32 {
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn cap_combined_declaration() {
    let src = r#"
cap A;
cap B = A;

func main() -> i32 {
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

#[test]
fn cap_combined_split() {
    let src = r#"
cap A;
cap B;
cap Combined = A + B;

func main() -> i32 {
    let (a, b) = Combined.split();
    42
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "combined cap split should work");
}

#[test]
fn cap_drop() {
    let src = r#"
cap IO;

func main() -> i32 {
    drop(IO);
    42
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(42));
}

// 0.34.18c (§4.2): cap_in_function_with_effect removed — it tested that a
// function may declare `with Cap`, but the `with` effect clause is abolished.
// cap_declaration covers cap declaration.

#[test]
fn cap_closure_consume() {
    // 0.34.18c (§4.2): `with IO` effect clause removed from use_io; the test
    // keeps its real subject — closure consumption.
    let src = r#"
cap IO;
func use_io(f: i32) { println(f) }
func main() -> i32 {
    let runner = fn(x: i32) -> i32 { println(x); x };
    runner(42)
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "cap closure should work: {:?}", result);
}

#[test]
fn cap_closure_capture_drop() {
    let src = r#"
cap Resource;
func main() -> i32 {
    let r = Resource;
    let f = fn() -> i32 { drop(r); 42 };
    f()
}
"#;
    let result = run_source_bytecode_result(src);
    assert!(
        result.is_ok(),
        "cap drop in closure should work: {:?}",
        result
    );
}
