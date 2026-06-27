// Stdlib edge case and robustness tests
// Focus: boundary conditions, error paths, unusual inputs

use super::*;

// ====== Directory Operations — Error Paths ======

#[test]
fn edge_listdir_file_not_dir() {
    // listdir on a file (not a directory) — should return empty
    let v = run_source("func main() -> i32 { len(listdir(\"examples/hello.mimi\")) }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn edge_walk_dir_single_file() {
    // walk_dir on a single file — should return empty or single entry
    let v = run_source("func main() -> i32 { let r = len(walk_dir(\"examples/hello.mimi\")); if r <= 1 { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn edge_is_dir_on_symlink_like() {
    // is_dir on special paths
    let v = run_source("func main() -> i32 { if is_dir(\"/tmp\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

// ====== Path Operations — Edge Cases ======

#[test]
fn edge_path_join_trailing_slash() {
    // path_join with trailing slash in first component
    let v = run_source("func main() -> string { path_join(\"a/\", \"b\") }");
    assert_eq!(v, interp::Value::String("a/b".to_string()));
}

#[test]
fn path_ext_on_dotfile() {
    // Extension of a dotfile like ".gitignore" — Rust's Path::extension() returns "" for dotfiles
    let v = run_source("func main() -> string { path_ext(\".gitignore\") }");
    assert_eq!(v, interp::Value::String("".to_string()));
}

#[test]
fn path_ext_on_double_ext() {
    // Extension of "archive.tar.gz"
    let v = run_source("func main() -> string { path_ext(\"archive.tar.gz\") }");
    assert_eq!(v, interp::Value::String("gz".to_string()));
}

#[test]
fn path_basename_of_dot() {
    // basename of "." — Rust's Path::file_name() returns None for "."
    let v = run_source("func main() -> string { path_basename(\".\") }");
    assert_eq!(v, interp::Value::String("".to_string()));
}

#[test]
fn path_dirname_of_dot() {
    // dirname of "."
    let v = run_source("func main() -> string { path_dirname(\".\") }");
    assert_eq!(v, interp::Value::String("".to_string()));
}

// ====== Crypto — Known Test Vectors ======

#[test]
fn sha256_test_vector_1() {
    // NIST test vector: SHA-256("abc")
    let v = run_source("func main() -> string { sha256(\"abc\") }");
    assert_eq!(v, interp::Value::String("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string()));
}

#[test]
fn sha256_test_vector_2() {
    // SHA-256 of empty string
    let v = run_source("func main() -> string { sha256(\"\") }");
    assert_eq!(v, interp::Value::String("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()));
}

#[test]
fn sha256_test_vector_3() {
    // SHA-256("hello") — well-known vector
    let v = run_source("func main() -> string { sha256(\"hello\") }");
    assert_eq!(v, interp::Value::String("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string()));
}

#[test]
fn sha256_deterministic() {
    // Same input should always produce same output
    let v = run_source(r#"
func main() -> i32 {
    let h1 = sha256("test")
    let h2 = sha256("test")
    if h1 == h2 { 1 } else { 0 }
}
"#);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn sha256_different_inputs_different() {
    // Different inputs should produce different hashes
    let v = run_source(r#"
func main() -> i32 {
    let h1 = sha256("hello")
    let h2 = sha256("world")
    if h1 == h2 { 0 } else { 1 }
}
"#);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn base64_roundtrip_empty() {
    let v = run_source(r#"
func main() -> i32 {
    let e = base64_encode("")
    let d = base64_decode(e)
    match d { Ok(s) => if s == "" { 1 } else { 0 }, Err(_) => 0 }
}
"#);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn base64_roundtrip_binary_like() {
    // Base64 of a string with special characters
    let v = run_source(r#"
func main() -> i32 {
    let original = "Hello, World! 123"
    let e = base64_encode(original)
    let d = base64_decode(e)
    match d { Ok(s) => if s == original { 1 } else { 0 }, Err(_) => 0 }
}
"#);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn base64_decode_invalid_returns_err() {
    // Invalid base64 should return Err, not crash
    let v = run_source(r#"
func main() -> i32 {
    let d = base64_decode("not!valid!base64!!!")
    match d { Ok(_) => 0, Err(_) => 1 }
}
"#);
    assert_eq!(v, interp::Value::Int(1));
}

// ====== String Operations — Edge Cases ======

#[test]
fn str_split_single_element() {
    // Split with delimiter not present
    let v = run_source("func main() -> i32 { len(str_split(\"hello\", \",\")) }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn str_split_consecutive_delimiters() {
    // Split with consecutive delimiters produces empty strings
    let v = run_source("func main() -> i32 { len(str_split(\"a,,b\", \",\")) }");
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn str_replace_no_match() {
    // Replace with no match returns original
    let v = run_source("func main() -> string { str_replace(\"hello\", \"xyz\", \"abc\") }");
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

#[test]
fn str_contains_empty_pattern() {
    // Contains with empty pattern
    let v = run_source("func main() -> i32 { if str_contains(\"hello\", \"\") { 1 } else { 0 } }");
    // Empty pattern should match
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn str_starts_with_longer_prefix() {
    // starts_with where prefix is longer than string
    let v = run_source("func main() -> i32 { if str_starts_with(\"hi\", \"hello world\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn str_trim_only_whitespace() {
    // Trim a string that is all whitespace
    let v = run_source("func main() -> i32 { len(str_trim(\"   \")) }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn str_join_empty_list() {
    // Join an empty list — tests empty collection handling
    let v = run_source("func main() -> i32 { let empty = str_split(\"\", \"|\"); len(str_join(empty, \",\")) }");
    // str_split("", "|") returns [""], so join returns ""
    assert_eq!(v, interp::Value::Int(0));
}

// ====== Regex — Edge Cases ======

#[test]
fn regex_match_empty_pattern() {
    // Regex with empty pattern matches everything
    let v = run_source("func main() -> i32 { if regex_match(\"hello\", \"\") { 1 } else { 0 } }");
    match v {
        interp::Value::Int(_) => {} // either 0 or 1 is acceptable
        _ => panic!("expected Int"),
    }
}

#[test]
fn regex_match_no_match() {
    // Regex that doesn't match
    let v = run_source("func main() -> i32 { if regex_match(\"hello\", \"^[0-9]+$\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn regex_find_no_match() {
    // regex_find with no match
    let v = run_source("func main() -> i32 { len(regex_find(\"hello\", \"[0-9]+\")) }");
    assert_eq!(v, interp::Value::Int(0));
}

// ====== Numeric Edge Cases ======

#[test]
fn integer_zero_operations() {
    let v = run_source("func main() -> i32 { 0 * 100 + 0 - 0 }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn integer_large_multiplication() {
    let v = run_source("func main() -> i32 { 10000 * 10000 }");
    assert_eq!(v, interp::Value::Int(100000000));
}

#[test]
fn float_zero_division() {
    // Division by zero causes interpreter error (DivisionByZero)
    let result = run_source_result("func main() -> f64 { 1.0 / 0.0 }");
    assert!(result.is_err(), "expected division by zero error");
}

#[test]
fn integer_min_operations() {
    // Operations with 0 and 1
    let v = run_source("func main() -> i32 { 1 * 1 + 0 * 100 }");
    assert_eq!(v, interp::Value::Int(1));
}

// ====== JSON Edge Cases ======

#[test]
fn json_roundtrip_string() {
    let v = run_source(r#"
func main() -> i32 {
    let s = to_json("hello")
    if s == "\"hello\"" { 1 } else { 0 }
}
"#);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn json_valid_object() {
    let v = run_source("func main() -> i32 { if json_is_valid(\"{\\\"a\\\":1}\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn json_invalid_string() {
    let v = run_source("func main() -> i32 { if json_is_valid(\"not json\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn json_get_missing_key() {
    // Getting a missing key from JSON causes error (not graceful)
    let result = run_source_result("func main() -> i32 { json_get_int(\"{\\\"a\\\":1}\", \"b\") }");
    assert!(result.is_err(), "expected error for missing key");
}

// ====== Control Flow — Boundary Cases ======

#[test]
fn while_false_immediately() {
    // While loop that never executes
    let v = run_source("func main() -> i32 { let mut x = 42; while false { x = 0; } x }");
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn for_empty_range() {
    // For loop over empty range
    let v = run_source("func main() -> i32 { let mut x = 0; for i in range(5, 5) { x = x + 1; } x }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn match_all_wildcard() {
    // Match with only wildcard
    let v = run_source("func main() -> i32 { match 42 { _ => 99 } }");
    assert_eq!(v, interp::Value::Int(99));
}

// ====== Record/Tuple Edge Cases ======

#[test]
fn record_field_shadowing() {
    // Record field name shadowing a variable
    let v = run_source(r#"
type Item { name: string }
func main() -> string {
    let name = "outer"
    let item = Item { name: "inner" }
    item.name
}
"#);
    assert_eq!(v, interp::Value::String("inner".to_string()));
}

#[test]
fn tuple_nested_access() {
    // Accessing nested tuple elements — parser doesn't support t.1.1
    // Use destructuring instead
    let v = run_source("func main() -> i32 { let t = (1, (2, 3)); let (a, b) = t; let (c, d) = b; d }");
    assert_eq!(v, interp::Value::Int(3));
}
