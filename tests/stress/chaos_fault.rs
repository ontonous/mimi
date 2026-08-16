// Chaos / malformed-input smoke tests.
use super::run_program;

#[test]
fn stress_stdlib_json_malformed_no_panic() {
    // 畸形 JSON 输入必须由 stdlib 安全返回 bool，绝不崩溃。
    let source = r#"
func main() -> i32 {
    let cases = ["{", "}", "[1,", "\"\\uZZZZ\"", "{\"a\":}"]
    for s in cases {
        if json_is_valid(s) { println("bad") } else { println("ok") }
    }
    0
}
"#;
    let out = run_program(source).expect("malformed JSON stress failed");
    assert_eq!(out.trim(), "ok\nok\nok\nok\nok");
}

#[test]
fn stress_chaos_ieee_div_by_zero_no_panic() {
    // 除零必须被运行时错误通道捕获，而不是 Panic/崩溃。
    let source = r#"
func main() -> i32 {
    let x = 1.0 / 0.0
    println(x)
    0
}
"#;
    let err = run_program(source).expect_err("division by zero should fail loudly");
    assert!(err.contains("runtime error"), "unexpected error: {err}");
    assert!(!err.contains("panicked"), "unexpected panic: {err}");
}
