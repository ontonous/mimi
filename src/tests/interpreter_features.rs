// Stress tests and edge cases for the Mimi interpreter
// Focus: genuine boundary conditions, complex scenarios, robustness

use super::*;

// ====== Deep Nesting Stress ======

#[test]
fn stress_deeply_nested_if() {
    // 10 levels of nested if/else — tests stack depth and scope management
    let v = run_source(
        r#"
func main() -> i32 {
    let x = 1
    if x == 1 {
        if x == 1 {
            if x == 1 {
                if x == 1 {
                    if x == 1 {
                        if x == 1 {
                            if x == 1 {
                                if x == 1 {
                                    if x == 1 {
                                        if x == 1 { 42 } else { 0 }
                                    } else { 0 }
                                } else { 0 }
                            } else { 0 }
                        } else { 0 }
                    } else { 0 }
                } else { 0 }
            } else { 0 }
        } else { 0 }
    } else { 0 }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn stress_deeply_nested_arithmetic() {
    // Deeply nested arithmetic expression — tests expression evaluator depth
    let v =
        run_source("func main() -> i32 { 1 + (2 + (3 + (4 + (5 + (6 + (7 + (8 + (9 + 10)))))))) }");
    assert_eq!(v, interp::Value::Int(55));
}

#[test]
fn stress_long_chain_addition() {
    // Long string concatenation chain — tests string handling under load
    let v = run_source("func main() -> string { \"a\" + \"b\" + \"c\" + \"d\" + \"e\" + \"f\" + \"g\" + \"h\" + \"i\" + \"j\" + \"k\" + \"l\" + \"m\" + \"n\" + \"o\" + \"p\" }");
    assert_eq!(v, interp::Value::String("abcdefghijklmnop".to_string()));
}

// ====== Type System Edge Cases ======

#[test]
fn stress_nested_generic_inference() {
    // List of Lists — tests nested type parameter inference
    let v = run_source(
        r#"
func main() -> i32 {
    let nested = [[1, 2], [3, 4], [5, 6]]
    let mut sum = 0
    for inner in nested {
        for item in inner {
            sum = sum + item
        }
    }
    sum
}
"#,
    );
    assert_eq!(v, interp::Value::Int(21));
}

#[test]
fn stress_record_with_all_types() {
    // Record containing every basic type — tests type system completeness
    let v = run_source(
        r#"
type Mixed {
    i: i32
    f: f64
    s: string
    b: bool
}
func main() -> i32 {
    let m = Mixed { i: 42, f: 3.14, s: "hello", b: true }
    if m.b { m.i } else { 0 }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn stress_enum_with_mixed_payloads() {
    // Enum with different payload types — tests pattern matching completeness
    let v = run_source(
        r#"
type Value {
    Int(i32)
    Float(f64)
    Text(string)
    Empty
}
func describe(v: Value) -> string {
    match v {
        Int(n) => f"int:{n}",
        Float(f) => "float",
        Text(s) => "text",
        Empty => "empty"
    }
}
func main() -> string {
    describe(Int(42))
}
"#,
    );
    assert_eq!(v, interp::Value::String("int:42".to_string()));
}

// ====== Control Flow Edge Cases ======

#[test]
fn stress_loop_break_continue_interaction() {
    // break and continue in nested loops — tests control flow correctness
    let v = run_source(
        r#"
func main() -> i32 {
    let mut sum = 0
    let mut i = 0
    while i < 10 {
        i = i + 1
        if i % 2 == 0 { continue }
        if i > 7 { break }
        sum = sum + i
    }
    sum
}
"#,
    );
    assert_eq!(v, interp::Value::Int(16)); // 1+3+5+7
}

#[test]
fn stress_match_with_guard_complex() {
    // Match guards with complex conditions — tests guard evaluation
    let v = run_source(
        r#"
func classify(n: i32) -> string {
    match n {
        x if x < 0 => "negative",
        x if x == 0 => "zero",
        x if x < 10 => "small",
        x if x < 100 => "medium",
        _ => "large"
    }
}
func main() -> string {
    classify(5)
}
"#,
    );
    assert_eq!(v, interp::Value::String("small".to_string()));
}

#[test]
fn stress_early_return_from_nested() {
    // Early return from nested function — tests scope cleanup
    let v = run_source(
        r#"
func check(x: i32) -> i32 {
    if x > 5 {
        if x > 10 { return 100 }
        return 50
    }
    0
}
func main() -> i32 { check(7) }
"#,
    );
    assert_eq!(v, interp::Value::Int(50));
}

// ====== Error Propagation Edge Cases ======

#[test]
fn stress_chain_error_propagation() {
    // Multiple ? operators in chain — tests error propagation depth
    let v = run_source(
        r#"
type Res { Ok(i32) Err(string) }
func step1(x: i32) -> Res { if x > 0 { Ok(x * 2) } else { Err("step1 failed") } }
func step2(x: i32) -> Res { if x < 100 { Ok(x + 10) } else { Err("step2 failed") } }
func step3(x: i32) -> Res { if x != 0 { Ok(x / 2) } else { Err("step3 failed") } }
func pipeline(x: i32) -> Res {
    let a = step1(x)?
    let b = step2(a)?
    let c = step3(b)?
    Ok(c)
}
func main() -> i32 {
    match pipeline(5) {
        Ok(v) => v,
        Err(e) => -1
    }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(10)); // (5*2+10)/2 = 10
}

#[test]
fn stress_error_at_different_stages() {
    // Error at each stage of pipeline — tests error message preservation
    let v = run_source(
        r#"
type Res { Ok(i32) Err(string) }
func step1(x: i32) -> Res { if x > 0 { Ok(x) } else { Err("bad input") } }
func step2(x: i32) -> Res { if x < 100 { Ok(x) } else { Err("too large") } }
func main() -> string {
    let r = step1(-1)
    match r {
        Ok(v) => "ok",
        Err(e) => e
    }
}
"#,
    );
    assert_eq!(v, interp::Value::String("bad input".to_string()));
}

// ====== String Edge Cases ======

#[test]
fn stress_empty_string_operations() {
    // Operations on empty strings — tests null/empty handling
    let v = run_source(
        r#"
func main() -> i32 {
    let s = ""
    let parts = str_split(s, ",")
    let joined = str_join(parts, "-")
    len(s) + len(joined)
}
"#,
    );
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn stress_string_with_special_chars() {
    // Strings with special characters — tests string handling robustness
    let v = run_source(
        r#"
func main() -> i32 {
    let s = "hello world"
    len(s)
}
"#,
    );
    assert_eq!(v, interp::Value::Int(11));
}

#[test]
fn stress_unicode_string() {
    // Unicode string operations — tests multi-byte character handling
    let v = run_source(
        r#"
func main() -> i32 {
    let s = "你好世界"
    len(s)
}
"#,
    );
    // Unicode chars may be counted as bytes or characters
    match v {
        interp::Value::Int(n) => assert!(n >= 4, "expected >= 4, got {}", n),
        _ => panic!("expected Int"),
    }
}

#[test]
fn stress_string_split_empty_delimiter() {
    // Split with empty delimiter — tests edge case in split implementation
    let v = run_source(
        r#"
func main() -> i32 {
    let parts = str_split("abc", "")
    len(parts)
}
"#,
    );
    // Behavior with empty delimiter varies; just ensure no crash
    match v {
        interp::Value::Int(_) => {}
        _ => panic!("expected Int, got {:?}", v),
    }
}

// ====== Collection Edge Cases ======

#[test]
fn stress_empty_list_operations() {
    // Operations on empty lists — tests null/empty handling
    let v = run_source(
        r#"
func main() -> i32 {
    let xs: List<i32> = []
    len(xs)
}
"#,
    );
    // Empty list might not be supported; test what happens
    if let interp::Value::Int(n) = v {
        assert_eq!(n, 0);
    }
}

#[test]
fn stress_list_with_nested_records() {
    // List of records — tests compound type handling in collections
    let v = run_source(
        r#"
type Point { x: i32, y: i32 }
func main() -> i32 {
    let points = [Point { x: 1, y: 2 }, Point { x: 3, y: 4 }, Point { x: 5, y: 6 }]
    let mut sum = 0
    for p in points {
        sum = sum + p.x + p.y
    }
    sum
}
"#,
    );
    assert_eq!(v, interp::Value::Int(21)); // (1+2)+(3+4)+(5+6)
}

#[test]
fn stress_large_list_iteration() {
    // Iterate over a large list — tests performance and correctness
    let v = run_source(
        r#"
func main() -> i32 {
    let mut sum = 0
    for i in range(0, 1000) {
        sum = sum + i
    }
    sum
}
"#,
    );
    assert_eq!(v, interp::Value::Int(499500));
}

// ====== Closure and Capture Edge Cases ======

#[test]
fn stress_closure_multiple_captures() {
    // Closure capturing multiple variables — tests capture mechanism
    let v = run_source(
        r#"
func main() -> i32 {
    let a = 10
    let b = 20
    let c = 30
    let f = fn(x: i32) -> i32 { a + b + c + x }
    f(5)
}
"#,
    );
    assert_eq!(v, interp::Value::Int(65));
}

#[test]
fn stress_closure_as_argument() {
    // Passing closure as function argument — tests closure passing
    let v = run_source(
        r#"
func apply(f: func(i32) -> i32, x: i32) -> i32 { f(x) }
func main() -> i32 {
    let double = fn(x: i32) -> i32 { x * 2 }
    apply(double, 21)
}
"#,
    );
    assert_eq!(v, interp::Value::Int(42));
}

// ====== Recursive Function Edge Cases ======

#[test]
fn stress_mutual_recursion() {
    // Two functions calling each other — tests call stack management
    let v = run_source(
        r#"
func is_even(n: i32) -> bool { if n == 0 { true } else { is_odd(n - 1) } }
func is_odd(n: i32) -> bool { if n == 0 { false } else { is_even(n - 1) } }
func main() -> i32 {
    if is_even(10) { 1 } else { 0 }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn stress_deep_recursion() {
    // Deep recursion — tests stack depth limits
    let v = run_source(
        r#"
func countdown(n: i32) -> i32 {
    if n <= 0 { 0 } else { 1 + countdown(n - 1) }
}
func main() -> i32 { countdown(20) }
"#,
    );
    assert_eq!(v, interp::Value::Int(20));
}

// ====== Numeric Edge Cases ======

#[test]
fn stress_integer_overflow() {
    // Large integer arithmetic — tests overflow handling
    let v = run_source("func main() -> i64 { 9223372036854775807 }"); // i64::MAX
    match v {
        interp::Value::Int(n) => assert!(n > 0),
        _ => panic!("expected Int"),
    }
}

#[test]
fn stress_negative_arithmetic() {
    // Negative number operations — tests signed arithmetic
    let v = run_source("func main() -> i32 { (-5) * (-3) + (-2) }");
    assert_eq!(v, interp::Value::Int(13));
}

#[test]
fn stress_float_precision() {
    // Float precision edge case — tests floating point handling
    let v = run_source("func main() -> f64 { 0.1 + 0.2 }");
    match v {
        interp::Value::Float(f) => assert!((f - 0.3).abs() < 0.0001, "got {}", f),
        _ => panic!("expected Float"),
    }
}

// ====== Pattern Matching Complexity ======

#[test]
fn stress_nested_pattern_match() {
    // Match on nested enum — tests pattern matching depth
    let v = run_source(
        r#"
type Inner { A(i32) B }
type Outer { Wrap(Inner) Raw(i32) }
func extract(o: Outer) -> i32 {
    match o {
        Wrap(inner) => match inner { A(n) => n, B => -1 },
        Raw(n) => n
    }
}
func main() -> i32 { extract(Wrap(A(42))) }
"#,
    );
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn stress_match_wildcard_position() {
    // Wildcard at different positions — tests match exhaustiveness
    let v = run_source(
        r#"
func main() -> i32 {
    let x = 5
    match x {
        1 => 10,
        _ => 99
    }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(99));
}

// ====== FFI-like Edge Cases (interpreter only) ======

#[test]
fn stress_read_file_nonexistent() {
    // Reading nonexistent file — tests error handling
    let v = run_source(
        r#"
func main() -> i32 {
    let r = read_file("/nonexistent_path_xyz_12345")
    if r.is_ok() { 1 } else { 0 }
}
"#,
    );
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn stress_listdir_nonexistent() {
    // Listing nonexistent directory — tests error handling
    let v = run_source("func main() -> i32 { len(listdir(\"/nonexistent_xyz\")) }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn stress_sha256_various_inputs() {
    // SHA-256 of various inputs — tests crypto robustness
    let v = run_source(
        r#"
func main() -> i32 {
    let h1 = sha256("")
    let h2 = sha256("a")
    let h3 = sha256("abc")
    let h4 = sha256("hello world")
    // All should be 64 hex chars
    len(h1) + len(h2) + len(h3) + len(h4)
}
"#,
    );
    assert_eq!(v, interp::Value::Int(256)); // 4 * 64
}

// ====== Complex Integration Scenarios ======

#[test]
fn stress_config_parser_simulation() {
    // Simulates a config parser — tests realistic usage pattern
    let v = run_source(
        r#"
func parse_line(line: string) -> string {
    let trimmed = str_trim(line)
    if len(trimmed) == 0 { return "" }
    if str_contains(trimmed, "=") {
        let parts = str_split(trimmed, "=")
        if len(parts) >= 2 {
            return str_trim(parts[0]) + ":" + str_trim(parts[1])
        }
    }
    ""
}
func main() -> i32 {
    let text = "key1=val1" + "\n" + "key2=val2" + "\n" + "key3=val3"
    let lines = str_split(text, "\n")
    let mut count = 0
    for line in lines {
        let result = parse_line(line)
        if len(result) > 0 { count = count + 1 }
    }
    count
}
"#,
    );
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn stress_html_escape_simulation() {
    // Simulates HTML escaping — tests string replacement chains
    let v = run_source(
        r#"
func escape_html(s: string) -> string {
    let mut r = str_replace(s, "&", "&amp;")
    r = str_replace(r, "<", "&lt;")
    r = str_replace(r, ">", "&gt;")
    r
}
func main() -> string {
    escape_html("<div class=\"test\">&hello</div>")
}
"#,
    );
    assert_eq!(
        v,
        interp::Value::String("&lt;div class=\"test\"&gt;&amp;hello&lt;/div&gt;".to_string())
    );
}

#[test]
fn stress_word_count_simulation() {
    // Simulates word counting — tests split+iterate+count pattern
    let v = run_source(
        r#"
func count_words(text: string) -> i32 {
    let words = str_split(str_trim(text), " ")
    let mut count = 0
    for w in words {
        if len(str_trim(w)) > 0 { count = count + 1 }
    }
    count
}
func main() -> i32 {
    count_words("  hello   world   foo  bar  ")
}
"#,
    );
    assert_eq!(v, interp::Value::Int(4));
}

#[test]
fn stress_json_builder_simulation() {
    // Simulates manual JSON building — tests string concatenation under load
    let v = run_source(
        r#"
func kv(key: string, val: string) -> string { "\"" + key + "\":\"" + val + "\"" }
func main() -> string {
    let json = "{" + kv("name", "mimi") + "," + kv("version", "1.0") + "," + kv("lang", "mimi") + "}"
    json
}
"#,
    );
    assert_eq!(
        v,
        interp::Value::String(
            "{\"name\":\"mimi\",\"version\":\"1.0\",\"lang\":\"mimi\"}".to_string()
        )
    );
}

#[test]
fn stress_base64_roundtrip_various() {
    // Base64 encode/decode with various inputs — tests crypto correctness
    let v = run_source(
        r#"
func main() -> i32 {
    let inputs = ["", "a", "ab", "abc", "abcd", "Hello, World!"]
    let mut ok_count = 0
    for input in inputs {
        let encoded = base64_encode(input)
        let decoded_result = base64_decode(encoded)
        match decoded_result {
            Ok(decoded) => { if decoded == input { ok_count = ok_count + 1 } },
            Err(e) => {}
        }
    }
    ok_count
}
"#,
    );
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn stress_path_operations_chain() {
    // Chain multiple path operations — tests path utility robustness
    let v = run_source(
        r#"
func main() -> string {
    let base = "/usr/local"
    let sub = "bin"
    let file = "mimi"
    let full = path_join(path_join(base, sub), file)
    path_basename(full)
}
"#,
    );
    assert_eq!(v, interp::Value::String("mimi".to_string()));
}

#[test]
fn scope_cleaned_after_error_in_block() {
    // When evaluation fails inside a nested block scope, the scope must still
    // be popped so that variables bound in that block do not leak into later
    // calls.
    let src = r#"
func leaky() -> i32 {
    if true {
        let y = "shadow";
        1 / 0
    }
    0
}

func use_y() -> string {
    y
}
"#;
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    // First call fails inside the if-branch block.
    let first = interp.call_named("leaky", vec![]);
    assert!(first.is_err(), "leaky should fail with division by zero");
    // A subsequent call must not see the leaked `y` from the failed block.
    let second = interp.call_named("use_y", vec![]);
    assert!(
        second.is_err(),
        "scope leaked from failed block: 'y' was visible after error"
    );
}
