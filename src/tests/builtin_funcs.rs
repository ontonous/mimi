use super::*;

#[test]
fn builtin_len_string() {
    let src = r#"
func main() -> i32 {
    len("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_list() {
    let src = r#"
func main() -> i32 {
    len([1, 2, 3, 4, 5])
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_len_empty_string() {
    let src = r#"
func main() -> i32 {
    len("")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_to_string_int() {
    let src = r#"
func main() -> string {
    to_string(42)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("42".to_string()));
}

#[test]
fn builtin_to_string_bool() {
    let src = r#"
func main() -> string {
    to_string(true)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("true".to_string()));
}

#[test]
fn builtin_abs_int() {
    let src = r#"
func main() -> i32 {
    abs(-5)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn builtin_abs_float() {
    let src = r#"
func main() -> f64 {
    abs(-3.14)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(3.14));
}

#[test]
fn builtin_push() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = push(a, 4);
    len(result)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(4));
}

#[test]
fn builtin_pop() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (popped, _) = result;
    popped
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_pop_returns_remaining() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let result = pop(a);
    let (_, new_list) = result;
    len(new_list)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn builtin_min_int() {
    let src = r#"
func main() -> i32 {
    min(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_max_int() {
    let src = r#"
func main() -> i32 {
    max(3, 7)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn builtin_min_float() {
    let src = r#"
func main() -> f64 {
    min(3.14, 2.71)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Float(2.71));
}

#[test]
fn builtin_contains_list() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3, 4, 5];
    if contains(a, 3) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_string() {
    let src = r#"
func main() -> i32 {
    let s = "hello world";
    if contains(s, "world") { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_contains_not_found() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    if contains(a, 99) { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

// ===================== MimiSpec Runtime Functions Tests =====================

#[test]
fn builtin_lexer_basic() {
    let src = r#"
func main() -> i32 {
    let tokens = lexer("module Test:")
    len(tokens)
}
"#;
    let v = run_source(src);
    // Should return a list of tokens
    match v {
        interp::Value::Int(n) => assert!(n > 0, "lexer should return tokens"),
        _ => panic!("lexer should return a list"),
    }
}

#[test]
fn builtin_parse_basic() {
    let src = r#"
func main() -> i32 {
    let result = parse("module Test:")
    0
}
"#;
    let v = run_source(src);
    // Should return without error
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_parse_with_error() {
    let src = r#"
func main() -> i32 {
    let result = parse("module Test")
    0
}
"#;
    let v = run_source(src);
    // Should return without crashing
    assert_eq!(v, interp::Value::Int(0));
}

// ===================== String Operations Tests =====================

#[test]
fn builtin_str_split() {
    let src = r#"
func main() -> i32 {
    let parts = str_split("a,b,c", ",")
    len(parts)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_str_join() {
    let src = r#"
func main() -> string {
    let parts = ["a", "b", "c"]
    str_join(parts, ",")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("a,b,c".to_string()));
}

#[test]
fn builtin_str_trim() {
    let src = r#"
func main() -> string {
    str_trim("  hello  ")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

#[test]
fn builtin_str_starts_with() {
    let src = r#"
func main() -> bool {
    str_starts_with("hello world", "hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn builtin_str_ends_with() {
    let src = r#"
func main() -> bool {
    str_ends_with("hello world", "world")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn builtin_str_replace() {
    let src = r#"
func main() -> string {
    str_replace("hello world", "world", "mimi")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello mimi".to_string()));
}

#[test]
fn builtin_str_to_upper() {
    let src = r#"
func main() -> string {
    str_to_upper("hello")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("HELLO".to_string()));
}

#[test]
fn builtin_str_to_lower() {
    let src = r#"
func main() -> string {
    str_to_lower("HELLO")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

#[test]
fn builtin_str_repeat() {
    let src = r#"
func main() -> string {
    str_repeat("ab", 3)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::String("ababab".to_string()));
}

#[test]
fn builtin_str_contains() {
    let src = r#"
func main() -> bool {
    str_contains("hello world", "world")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

#[test]
fn builtin_str_index_of() {
    let src = r#"
func main() -> i32 {
    let (found, idx) = str_index_of("hello world", "world")
    idx
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

// ===================== Map Operations Tests =====================

#[test]
fn builtin_map_new() {
    let src = r#"
func main() -> i32 {
    let m = map_new()
    map_size(m)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_map_set_get() {
    let src = r#"
func main() -> i32 {
    let m = map_new()
    let m = map_set(m, "name", "mimi")
    let (found, val) = map_get(m, "name")
    if found { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_map_size() {
    let src = r#"
func main() -> i32 {
    let m = map_new()
    let m = map_set(m, "a", 1)
    let m = map_set(m, "b", 2)
    map_size(m)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn builtin_map_remove() {
    let src = r#"
func main() -> i32 {
    let m = map_new()
    let m = map_set(m, "x", 1)
    let m = map_remove(m, "x")
    map_size(m)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_map_from_list() {
    let src = r#"
func main() -> i32 {
    let pairs = [("a", 1), ("b", 2)]
    let m = map_from_list(pairs)
    let (found, val) = map_get(m, "b")
    if found { val } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

// ===================== IO Operations Tests =====================

#[test]
fn builtin_file_exists() {
    let src = r#"
func main() -> bool {
    file_exists("/tmp")
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Bool(true));
}

// ===================== Result Return Tests =====================

#[test]
fn builtin_read_file_returns_result_ok() {
    let src = r#"
func main() -> i32 {
    let result = read_file("/etc/hostname")
    match result {
        Ok(_) => 1,
        Err(_) => 0,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_read_file_returns_result_err() {
    let src = r#"
func main() -> i32 {
    let result = read_file("/nonexistent/file/path.txt")
    match result {
        Ok(_) => 0,
        Err(_) => 1,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_write_file_returns_result() {
    let src = r#"
func main() -> i32 {
    let result = write_file("/tmp/mimi_test_result.txt", "hello")
    match result {
        Ok(_) => 1,
        Err(_) => 0,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}
