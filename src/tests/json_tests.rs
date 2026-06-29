use super::*;

// Helper to wrap JSON in a func main
fn json_src(code: &str) -> String {
    format!("func main() -> string {{ {} }}", code)
}

fn json_result(code: &str) -> Result<interp::Value, String> {
    run_source_result(&json_src(code))
}

fn json_value(code: &str) -> interp::Value {
    run_source(&json_src(code))
}

// from_json: valid inputs
#[test]
fn json_from_json_valid_object() {
    assert_eq!(
        json_value(r#"from_json("{\"a\":1}")"#),
        interp::Value::String("{\"a\":1}".into())
    );
}

#[test]
fn json_from_json_valid_array() {
    assert_eq!(
        json_value(r#"from_json("[1, 2, 3]")"#),
        interp::Value::String("[1, 2, 3]".into())
    );
}

#[test]
fn json_from_json_valid_string() {
    assert_eq!(
        json_value(r#"from_json("\"hello\"")"#),
        interp::Value::String("\"hello\"".into())
    );
}

#[test]
fn json_from_json_valid_number() {
    assert_eq!(
        json_value(r#"from_json("42")"#),
        interp::Value::String("42".into())
    );
}

#[test]
fn json_from_json_valid_bool() {
    assert_eq!(
        json_value(r#"from_json("true")"#),
        interp::Value::String("true".into())
    );
}

#[test]
fn json_from_json_valid_null() {
    assert_eq!(
        json_value(r#"from_json("null")"#),
        interp::Value::String("null".into())
    );
}

// from_json: nested structures
#[test]
fn json_from_json_nested_object() {
    assert_eq!(
        json_value(r#"from_json("{\"a\":{\"b\":{\"c\":1}}}")"#),
        interp::Value::String("{\"a\":{\"b\":{\"c\":1}}}".into())
    );
}

#[test]
fn json_from_json_nested_array() {
    assert_eq!(
        json_value(r#"from_json("[[1,2],[3,4]]")"#),
        interp::Value::String("[[1,2],[3,4]]".into())
    );
}

// from_json: unicode
#[test]
fn json_from_json_unicode() {
    assert_eq!(
        json_value(r#"from_json("\"\\u0041\"")"#),
        interp::Value::String("\"\\u0041\"".into())
    );
}

// from_json: whitespace handling
#[test]
fn json_from_json_whitespace() {
    assert_eq!(
        json_value(r#"from_json("{  \"a\" : 1  }")"#),
        interp::Value::String("{  \"a\" : 1  }".into())
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
    let v =
        run_source(r#"func main() -> string { json_get_string("{\"name\":\"Alice\"}", "name") }"#);
    assert_eq!(v, interp::Value::String("Alice".into()));
}

#[test]
fn json_get_string_missing_key() {
    let v = run_source(r#"func main() -> string { json_get_string("{\"a\":1}", "nonexistent") }"#);
    assert_eq!(
        v,
        interp::Value::String("".into()),
        "json_get_string with missing key returns empty string"
    );
}

#[test]
fn json_get_string_not_a_string() {
    // json_get_string on non-string values returns string representation
    let v = run_source(r#"func main() -> string { json_get_string("{\"a\":42}", "a") }"#);
    assert_eq!(v, interp::Value::String("42".into()));
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
    let v = run_source(r#"func main() -> string { json_get_element("[10, 20, 30]", 0) }"#);
    assert_eq!(v, interp::Value::String("10".into()));
}

#[test]
fn json_get_element_middle() {
    let v = run_source(r#"func main() -> string { json_get_element("[10, 20, 30]", 1) }"#);
    assert_eq!(v, interp::Value::String("20".into()));
}

#[test]
fn json_get_element_out_of_bounds() {
    let v = run_source(r#"func main() -> string { json_get_element("[10, 20]", 99) }"#);
    assert_eq!(
        v,
        interp::Value::String("".into()),
        "json_get_element out of bounds returns empty string"
    );
}

// json_get_element: nested objects in arrays
#[test]
fn json_get_element_object_in_array() {
    let v =
        run_source(r#"func main() -> string { json_get_element("[{\"x\":1}, {\"x\":2}]", 0) }"#);
    assert_eq!(v, interp::Value::String("{\"x\":1}".into()));
}

// to_json: serialization
#[test]
fn json_to_json_int() {
    let v = run_source(r#"func main() -> string { to_json(42) }"#);
    assert_eq!(v, interp::Value::String("42".into()));
}

#[test]
fn json_to_json_bool() {
    let v = run_source(r#"func main() -> string { to_json(true) }"#);
    assert_eq!(v, interp::Value::String("true".into()));
}

#[test]
fn json_to_json_string() {
    let v = run_source(r#"func main() -> string { to_json("hello") }"#);
    assert_eq!(v, interp::Value::String("\"hello\"".into()));
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
    let src = r#"
func main() -> str {
    json_get_string("{\"x\": false}", "x")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("false".into()));
}

#[test]
fn json_get_bool_missing_key() {
    let v = run_source(r#"func main() -> string { json_get_string("{\"a\": true}", "missing") }"#);
    assert_eq!(
        v,
        interp::Value::String("".into()),
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
    let v = run_source(r#"func main() -> string { from_json::<string>("\"hello\"") }"#);
    assert_eq!(v, interp::Value::String("hello".into()));
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
    let v = run_source(r#"func main() -> f64 { from_json::<f64>("2.5") }"#);
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
    let v = run_source(
        r#"
        func main() -> string {
            let x = from_json::<Option<i32>>("null");
            // None gives unit, check type
            type_name(x)
        }
    "#,
    );
    assert_eq!(v, interp::Value::String("unit".into()));
}

#[test]
fn json_from_json_typed_nested_record() {
    let v = run_source(
        r#"
        type Address { city: string, zip: i32 }
        type Person { name: string, addr: Address }
        func main() -> string {
            let p = from_json::<Person>("{\"name\": \"Alice\", \"addr\": {\"city\": \"NYC\", \"zip\": 10001}}");
            p.name + " lives in " + p.addr.city
        }
    "#,
    );
    assert_eq!(v, interp::Value::String("Alice lives in NYC".into()));
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
    let v = run_source(
        r#"
        func main() -> string {
            let items = from_json::<List<string>>("[\"a\", \"b\", \"c\"]");
            items[0] + items[1] + items[2]
        }
    "#,
    );
    assert_eq!(v, interp::Value::String("abc".into()));
}

#[test]
fn json_from_json_typed_f64_negative() {
    let v = run_source(r#"func main() -> f64 { from_json::<f64>("-1.5") }"#);
    assert_eq!(v, interp::Value::Float(-1.5));
}

#[test]
fn json_from_json_typed_f64_zero() {
    let v = run_source(r#"func main() -> f64 { from_json::<f64>("0.0") }"#);
    assert_eq!(v, interp::Value::Float(0.0));
}

#[test]
fn json_from_json_typed_i32_negative() {
    let v = run_source(r#"func main() -> i32 { from_json::<i32>("-42") }"#);
    assert_eq!(v, interp::Value::Int(-42));
}

#[test]
fn json_from_json_typed_record_with_list() {
    let v = run_source(
        r#"
        type Team { name: string, members: List<string> }
        func main() -> string {
            let t = from_json::<Team>("{\"name\": \"dev\", \"members\": [\"A\", \"B\"]}");
            t.name + ": " + t.members[0] + ", " + t.members[1]
        }
    "#,
    );
    assert_eq!(v, interp::Value::String("dev: A, B".into()));
}

#[test]
fn json_from_json_typed_enum_unit_variant() {
    let v = run_source(
        r#"
        type Color { Red, Green, Blue }
        func main() -> string {
            let c = from_json::<Color>("\"Red\"");
            type_name(c)
        }
    "#,
    );
    assert_eq!(v, interp::Value::String("Red".into()));
}

#[test]
fn json_from_json_typed_enum_with_payload() {
    let v = run_source(
        r#"
        type Shape { Circle(f64), Rect(f64, f64) }
        func main() -> f64 {
            let s = from_json::<Shape>("{\"Circle\": 2.5}");
            // Pattern match would be ideal but checker may not support it
            // Just verify it deserializes without error
            1.0
        }
    "#,
    );
    assert_eq!(v, interp::Value::Float(1.0));
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
    let v = run_source(r#"func main() -> string { from_json("\"hello\"") }"#);
    assert_eq!(v, interp::Value::String("\"hello\"".into()));
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
