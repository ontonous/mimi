use super::*;
use crate::interp::SharedString;

// Keep the JSON-producing function out of `main`: entry points may return only
// scalar values. The scalar entry point calls the named owner and prints its
// result so the Rust harness can retain the original JSON value assertions.
fn json_src(code: &str) -> String {
    json_src_with_items("", code)
}

fn json_src_with_items(items: &str, code: &str) -> String {
    format!(
        "{items}\nfunc json_owner() -> string {{ {code} }}\nfunc main() -> i64 {{ println(json_owner()); 0 }}"
    )
}

fn json_owner_output(main_result: interp::Value, stdout: String) -> Result<interp::Value, String> {
    if main_result != interp::Value::Int(0) {
        return Err(format!(
            "JSON harness main returned {main_result:?}, expected 0"
        ));
    }
    let json = stdout
        .strip_suffix('\n')
        .ok_or_else(|| "JSON owner did not print a newline-terminated result".to_string())?;
    Ok(interp::Value::String(SharedString::new(json.into())))
}

fn json_result_with_items(items: &str, code: &str) -> Result<interp::Value, String> {
    let source = json_src_with_items(items, code);
    let tokens = lexer::Lexer::new(&source).tokenize()?;
    let file = parser::Parser::new(tokens)
        .parse_file()
        .map_err(|error| error.message)?;
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let program = compiler
        .compile_file(&file)
        .map_err(|error| error.to_string())?;
    let mut vm = interp::bytecode::BytecodeVM::new(program);
    vm.enable_stdout_capture();
    let main_result = vm
        .run_value()
        .map_err(|error| error.message().to_string())?;
    let stdout = vm.take_stdout();
    json_owner_output(main_result, stdout)
}

fn json_result(code: &str) -> Result<interp::Value, String> {
    json_result_with_items("", code)
}

fn json_value(code: &str) -> interp::Value {
    let (main_result, stdout) = run_source_with_stdout(&json_src(code));
    json_owner_output(main_result, stdout).expect("JSON owner should return a value")
}

fn json_value_with_items(items: &str, code: &str) -> interp::Value {
    let source = json_src_with_items(items, code);
    let (main_result, stdout) = run_source_with_stdout(&source);
    json_owner_output(main_result, stdout).expect("JSON owner should return a value")
}

// from_json: valid inputs
#[test]
fn json_from_json_valid_object() {
    assert_eq!(
        json_value(r#"from_json("{\"a\":1}")"#),
        interp::Value::String(SharedString::new("{\"a\":1}".into()))
    );
}

#[test]
fn json_from_json_valid_array() {
    assert_eq!(
        json_value(r#"from_json("[1, 2, 3]")"#),
        interp::Value::String(SharedString::new("[1, 2, 3]".into()))
    );
}

#[test]
fn json_from_json_valid_string() {
    assert_eq!(
        json_value(r#"from_json("\"hello\"")"#),
        interp::Value::String(SharedString::new("\"hello\"".into()))
    );
}

#[test]
fn json_from_json_valid_number() {
    assert_eq!(
        json_value(r#"from_json("42")"#),
        interp::Value::String(SharedString::new("42".into()))
    );
}

#[test]
fn json_from_json_valid_bool() {
    assert_eq!(
        json_value(r#"from_json("true")"#),
        interp::Value::String(SharedString::new("true".into()))
    );
}

#[test]
fn json_from_json_valid_null() {
    assert_eq!(
        json_value(r#"from_json("null")"#),
        interp::Value::String(SharedString::new("null".into()))
    );
}

// from_json: nested structures
#[test]
fn json_from_json_nested_object() {
    assert_eq!(
        json_value(r#"from_json("{\"a\":{\"b\":{\"c\":1}}}")"#),
        interp::Value::String(SharedString::new("{\"a\":{\"b\":{\"c\":1}}}".into()))
    );
}

#[test]
fn json_from_json_nested_array() {
    assert_eq!(
        json_value(r#"from_json("[[1,2],[3,4]]")"#),
        interp::Value::String(SharedString::new("[[1,2],[3,4]]".into()))
    );
}

// from_json: unicode
#[test]
fn json_from_json_unicode() {
    assert_eq!(
        json_value(r#"from_json("\"\\u0041\"")"#),
        interp::Value::String(SharedString::new("\"\\u0041\"".into()))
    );
}

// from_json: whitespace handling
#[test]
fn json_from_json_whitespace() {
    assert_eq!(
        json_value(r#"from_json("{  \"a\" : 1  }")"#),
        interp::Value::String(SharedString::new("{  \"a\" : 1  }".into()))
    );
}

// from_json: invalid inputs → error
#[test]
fn json_from_json_invalid_trash() {
    assert!(json_result(r#"from_json("{invalid}")"#).is_err());
}

#[test]
fn json_from_json_invalid_unclosed_brace() {
    assert!(json_result(r#"from_json("{\"a\":1")"#).is_err());
}

#[test]
fn json_from_json_invalid_trailing_garbage() {
    assert!(json_result(r#"from_json("42abc")"#).is_err());
}

#[test]
fn json_from_json_invalid_empty_string() {
    assert!(json_result(r#"from_json("")"#).is_err());
}

// json_get_string: extract string field
#[test]
fn json_get_string_exists() {
    let v = json_value(r#"json_get_string("{\"name\":\"Alice\"}", "name")"#);
    assert_eq!(v, interp::Value::String(SharedString::new("Alice".into())));
}

#[test]
fn json_get_string_missing_key() {
    let v = json_value(r#"json_get_string("{\"a\":1}", "nonexistent")"#);
    assert_eq!(
        v,
        interp::Value::String(SharedString::new("".into())),
        "json_get_string with missing key returns empty string"
    );
}

#[test]
fn json_get_string_not_a_string() {
    // json_get_string on non-string values returns string representation
    let v = json_value(r#"json_get_string("{\"a\":42}", "a")"#);
    assert_eq!(v, interp::Value::String(SharedString::new("42".into())));
}

// json_get_int: extract integer field
#[test]
fn json_get_int_field() {
    let v = run_source(r#"func main() -> i64 { json_get_int("{\"count\":42}", "count") }"#);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn json_get_int_missing_key() {
    let result =
        run_source_result(r#"func main() -> i64 { json_get_int("{\"a\":1}", "nonexistent") }"#);
    assert!(
        result.is_err(),
        "json_get_int with missing key should error"
    );
}

// json_get_element: extract from array
#[test]
fn json_get_element_first() {
    let v = json_value(r#"json_get_element("[10, 20, 30]", 0)"#);
    assert_eq!(v, interp::Value::String(SharedString::new("10".into())));
}

#[test]
fn json_get_element_middle() {
    let v = json_value(r#"json_get_element("[10, 20, 30]", 1)"#);
    assert_eq!(v, interp::Value::String(SharedString::new("20".into())));
}

#[test]
fn json_get_element_out_of_bounds() {
    let result = json_result(r#"json_get_element("[10, 20]", 99)"#);
    assert!(
        result.is_err(),
        "json_get_element out of bounds should error (tree-walker parity)"
    );
}

// json_get_element: nested objects in arrays
#[test]
fn json_get_element_object_in_array() {
    let v = json_value(r#"json_get_element("[{\"x\":1}, {\"x\":2}]", 0)"#);
    assert_eq!(
        v,
        interp::Value::String(SharedString::new("{\"x\":1}".into()))
    );
}

// to_json: serialization
#[test]
fn json_to_json_int() {
    let v = json_value("to_json(42)");
    assert_eq!(v, interp::Value::String(SharedString::new("42".into())));
}

#[test]
fn json_to_json_bool() {
    let v = json_value("to_json(true)");
    assert_eq!(v, interp::Value::String(SharedString::new("true".into())));
}

#[test]
fn json_to_json_string() {
    let v = json_value(r#"to_json("hello")"#);
    assert_eq!(
        v,
        interp::Value::String(SharedString::new("\"hello\"".into()))
    );
}

// stdlib-style wrappers (without module import)
#[test]
fn json_get_bool_true() {
    let v = run_source(
        r#"func main() -> bool { json_get_string("{\"active\":\"true\"}", "active") == "true" }"#,
    );
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_get_bool_false() {
    let v = run_source(
        r#"func main() -> bool { json_get_string("{\"active\":\"false\"}", "active") == "true" }"#,
    );
    assert_eq!(v, interp::Value::Bool(false));
}

#[test]
fn json_get_bool_result() {
    // Test that json_get_string returns "false" for boolean false
    let v = json_value(r#"json_get_string("{\"x\": false}", "x")"#);
    assert_eq!(v, interp::Value::String(SharedString::new("false".into())));
}

#[test]
fn json_get_bool_missing_key() {
    let v = json_value(r#"json_get_string("{\"a\": true}", "missing")"#);
    assert_eq!(
        v,
        interp::Value::String(SharedString::new("".into())),
        "json_get_string with missing key returns empty string"
    );
}

#[test]
fn json_has_key_present() {
    let v = run_source(r#"func main() -> bool { json_get_string("{\"x\":\"y\"}", "x") != "" }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_has_key_missing() {
    let v = run_source(r#"func main() -> bool { json_get_string("{\"x\":\"y\"}", "z") != "" }"#);
    assert_eq!(
        v,
        interp::Value::Bool(false),
        "json_get_string of missing key returns empty string, not equal to \"\""
    );
}

// ===== is_valid_json =====

#[test]
fn json_is_valid_object() {
    let v = run_source(r#"func main() -> bool { json_is_valid("{\"a\":1}") }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_is_valid_array() {
    let v = run_source(r#"func main() -> bool { json_is_valid("[1,2,3]") }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_is_valid_string() {
    let v = run_source(r#"func main() -> bool { json_is_valid("\"hello\"") }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_is_valid_number() {
    let v = run_source(r#"func main() -> bool { json_is_valid("42") }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_is_valid_bool() {
    let v = run_source(r#"func main() -> bool { json_is_valid("true") }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_is_valid_empty_string_json() {
    // Regression: '""' is valid JSON but was incorrectly detected as invalid
    let v = run_source(r#"func main() -> bool { json_is_valid("\"\"") }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_is_valid_empty_input() {
    let v = run_source(r#"func main() -> bool { json_is_valid("") }"#);
    assert_eq!(v, interp::Value::Bool(false));
}

#[test]
fn json_is_valid_invalid_trash() {
    let v = run_source(r#"func main() -> bool { json_is_valid("{invalid}") }"#);
    assert_eq!(v, interp::Value::Bool(false));
}

#[test]
fn json_is_valid_invalid_unclosed() {
    let v = run_source(r#"func main() -> bool { json_is_valid("{\"a\":1") }"#);
    assert_eq!(v, interp::Value::Bool(false));
}

#[test]
fn json_is_valid_trailing_garbage() {
    let v = run_source(r#"func main() -> bool { json_is_valid("42abc") }"#);
    assert_eq!(v, interp::Value::Bool(false));
}

// ─── from_json::<T> typed deserialization (v0.22.2) ──────────────────

#[test]
fn json_from_json_typed_i32() {
    let v = run_source(r#"func main() -> i32 { from_json::<i32>("42") }"#);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn json_from_json_typed_string() {
    let v = json_value(r#"from_json::<string>("\"hello\"")"#);
    assert_eq!(v, interp::Value::String(SharedString::new("hello".into())));
}

#[test]
fn json_from_json_typed_bool_true() {
    let v = run_source(r#"func main() -> bool { from_json::<bool>("true") }"#);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn json_from_json_typed_bool_false() {
    let v = run_source(r#"func main() -> bool { from_json::<bool>("false") }"#);
    assert_eq!(v, interp::Value::Bool(false));
}

#[test]
fn json_from_json_typed_f64() {
    let src = r#"
func json_typed_f64_result() -> f64 { from_json::<f64>("2.5") }
func main() -> i32 { 0 }
"#;
    let v = bytecode_call_named(src, "json_typed_f64_result", vec![])
        .expect("typed f64 JSON helper should run");
    assert_eq!(v, interp::Value::Float(2.5));
}

#[test]
fn json_from_json_typed_list_i32() {
    let v = run_source(
        r#"
        func main() -> i32 {
            let nums = from_json::<List<i32>>("[1, 2, 3]");
            nums[0] + nums[1] + nums[2]
        }
    "#,
    );
    assert_eq!(v, interp::Value::Int(6));
}

#[test]
fn json_from_json_typed_record() {
    let v = run_source(
        r#"
        type Point { x: i32, y: i32 }
        func main() -> i32 {
            let p = from_json::<Point>("{\"x\": 10, \"y\": 20}");
            p.x + p.y
        }
    "#,
    );
    assert_eq!(v, interp::Value::Int(30));
}

#[test]
fn json_from_json_typed_option_some() {
    let v = run_source(
        r#"
        func main() -> i32 {
            let x = from_json::<Option<i32>>("42");
            x.unwrap()
        }
    "#,
    );
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn json_from_json_typed_option_none() {
    let v = json_value(
        r#"
        let x = from_json::<Option<i32>>("null");
        // None gives unit, check type
        type_name(x)
    "#,
    );
    assert_eq!(v, interp::Value::String(SharedString::new("None".into())));
}

#[test]
fn json_from_json_typed_nested_record() {
    let v = json_value_with_items(
        r#"
        type Address { city: string, zip: i32 }
        type Person { name: string, addr: Address }
    "#,
        r#"
        let p = from_json::<Person>("{\"name\": \"Alice\", \"addr\": {\"city\": \"NYC\", \"zip\": 10001}}");
        p.name + " lives in " + p.addr.city
    "#,
    );
    assert_eq!(
        v,
        interp::Value::String(SharedString::new("Alice lives in NYC".into()))
    );
}

// ─── Additional typed deserialization edge cases ───────────────

#[test]
fn json_from_json_typed_empty_list() {
    let v = run_source(
        r#"
        func main() -> i32 {
            let nums = from_json::<List<i32>>("[]");
            nums.len()
        }
    "#,
    );
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn json_from_json_typed_list_string() {
    let v = json_value(
        r#"
        let items = from_json::<List<string>>("[\"a\", \"b\", \"c\"]");
        items[0] + items[1] + items[2]
    "#,
    );
    assert_eq!(v, interp::Value::String(SharedString::new("abc".into())));
}

#[test]
fn json_from_json_typed_f64_negative() {
    let src = r#"
func json_typed_f64_negative_result() -> f64 { from_json::<f64>("-1.5") }
func main() -> i32 { 0 }
"#;
    let v = bytecode_call_named(src, "json_typed_f64_negative_result", vec![])
        .expect("negative typed f64 JSON helper should run");
    assert_eq!(v, interp::Value::Float(-1.5));
}

#[test]
fn json_from_json_typed_f64_zero() {
    let src = r#"
func json_typed_f64_zero_result() -> f64 { from_json::<f64>("0.0") }
func main() -> i32 { 0 }
"#;
    let v = bytecode_call_named(src, "json_typed_f64_zero_result", vec![])
        .expect("zero typed f64 JSON helper should run");
    assert_eq!(v, interp::Value::Float(0.0));
}

#[test]
fn json_from_json_typed_i32_negative() {
    let v = run_source(r#"func main() -> i32 { from_json::<i32>("-42") }"#);
    assert_eq!(v, interp::Value::Int(-42));
}

#[test]
fn json_from_json_typed_record_with_list() {
    let v = json_value_with_items(
        r#"
        type Team { name: string, members: List<string> }
    "#,
        r#"
        let t = from_json::<Team>("{\"name\": \"dev\", \"members\": [\"A\", \"B\"]}");
        t.name + ": " + t.members[0] + ", " + t.members[1]
    "#,
    );
    assert_eq!(
        v,
        interp::Value::String(SharedString::new("dev: A, B".into()))
    );
}

#[test]
fn json_from_json_typed_enum_unit_variant() {
    let v = json_value_with_items(
        r#"
        type Color { Red, Green, Blue }
    "#,
        r#"
        let c = from_json::<Color>("\"Red\"");
        type_name(c)
    "#,
    );
    assert_eq!(v, interp::Value::String(SharedString::new("Red".into())));
}

#[test]
fn json_from_json_typed_enum_with_payload() {
    let src = r#"
type Shape { Circle(f64), Rect(f64, f64) }
func json_shape_result() -> Shape {
    from_json::<Shape>("{\"Circle\": 2.5}")
}
func main() -> i32 { 0 }
"#;
    let v = bytecode_call_named(src, "json_shape_result", vec![])
        .expect("typed enum JSON helper should run");
    let interp::Value::Record(Some(type_name), fields) = v else {
        panic!("typed enum JSON should decode to a tagged record, got {v:?}");
    };
    assert_eq!(type_name, "Shape");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields.get("Circle"), Some(&interp::Value::Float(2.5)));
}

#[test]
fn json_from_json_typed_invalid_json_error() {
    let result = run_source_result(r#"func main() -> i32 { from_json::<i32>("not json") }"#);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("JSON parse error") || err.contains("from_json"),
        "error: {}",
        err
    );
}

#[test]
fn json_from_json_typed_type_mismatch_string_as_int() {
    let result = run_source_result(r#"func main() -> i32 { from_json::<i32>("\"hello\"") }"#);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("expected integer"), "error: {}", err);
}

#[test]
fn json_from_json_untyped_backward_compat() {
    let v = json_value(r#"from_json("\"hello\"")"#);
    assert_eq!(
        v,
        interp::Value::String(SharedString::new("\"hello\"".into()))
    );
}

// ─── json_array_length tests ─────────────────────────────────

#[test]
fn json_array_length_empty() {
    let v = run_source(r#"func main() -> i32 { json_array_length("[]") }"#);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn json_array_length_simple() {
    let v = run_source(r#"func main() -> i32 { json_array_length("[1, 2, 3]") }"#);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn json_array_length_nested() {
    let v = run_source(r#"func main() -> i32 { json_array_length("[[1, 2], [3]]") }"#);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn json_array_length_objects() {
    let v = run_source(
        "func main() -> i32 { json_array_length(\"[{\\\"a\\\": 1}, {\\\"b\\\": 2}]\") }",
    );
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn json_array_length_mixed() {
    let v = run_source(
        "func main() -> i32 { json_array_length(\"[1, \\\"hello\\\", true, null, []]\") }",
    );
    assert_eq!(v, interp::Value::Int(5));
}
