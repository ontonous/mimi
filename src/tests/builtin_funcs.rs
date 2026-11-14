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
func observed_to_string_int() -> string {
    to_string(42)
}
func main() -> i32 {
    if observed_to_string_int() == "42" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_to_string_bool() {
    let src = r#"
func observed_to_string_bool() -> string {
    to_string(true)
}
func main() -> i32 {
    if observed_to_string_bool() == "true" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
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
func observed_abs_float() -> f64 {
    abs(-2.5)
}
func main() -> i32 {
    if observed_abs_float() == 2.5 { 0 } else { 1 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_push() {
    let src = r#"
func main() -> i32 {
    let mut a = [1, 2, 3];
    push(a, 4);
    len(a)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(4));
}

#[test]
fn builtin_pop() {
    // Dual with codegen: pop returns the element only (not (elem, list) tuple).
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    pop(a)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_pop_returns_element_type() {
    let src = r#"
func main() -> i32 {
    let a = [1, 2, 3];
    let last = pop(a);
    last
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
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
func observed_min_float() -> f64 {
    min(3.14, 2.71)
}
func main() -> i32 {
    if observed_min_float() == 2.71 { 0 } else { 1 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
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
    let result = mms_parse("module Test:")
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
    let result = mms_parse("module Test")
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
func observed_str_join() -> string {
    let parts = ["a", "b", "c"]
    str_join(parts, ",")
}
func main() -> i32 {
    if observed_str_join() == "a,b,c" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_str_trim() {
    let src = r#"
func observed_str_trim() -> string {
    str_trim("  hello  ")
}
func main() -> i32 {
    if observed_str_trim() == "hello" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
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
func observed_str_replace() -> string {
    str_replace("hello world", "world", "mimi")
}
func main() -> i32 {
    if observed_str_replace() == "hello mimi" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_str_to_upper() {
    let src = r#"
func observed_str_to_upper() -> string {
    str_to_upper("hello")
}
func main() -> i32 {
    if observed_str_to_upper() == "HELLO" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_str_to_lower() {
    let src = r#"
func observed_str_to_lower() -> string {
    str_to_lower("HELLO")
}
func main() -> i32 {
    if observed_str_to_lower() == "hello" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_str_repeat() {
    let src = r#"
func observed_str_repeat() -> string {
    str_repeat("ab", 3)
}
func main() -> i32 {
    if observed_str_repeat() == "ababab" { 0 } else { 1 }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
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
    let found = str_index_of("hello world", "world")
    match found {
        Some(idx) => idx,
        None => -1,
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(6));
}

// ===================== Map Operations Tests =====================

#[test]
fn builtin_map_new() {
    let src = r#"
func report_map_failure(status: i32) -> i32 {
    println("unexpected Map error: " + to_string(status))
    1000000 + status
}
func main() -> i32 {
    match map_new() {
        Ok(m) => {
            let size = map_size(m)
            drop(m)
            size
        },
        Err(status) => report_map_failure(status),
    }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn builtin_map_set_get() {
    let src = r#"
func report_map_failure(status: i32) -> i32 {
    println("unexpected Map error: " + to_string(status))
    1000000 + status
}
func main() -> i32 {
    let m = match map_new() {
        Ok(value) => value,
        Err(status) => return report_map_failure(status),
    }
    let updated = match map_set(m, "name", "mimi") {
        Ok(value) => {
            drop(m)
            value
        },
        Err(status) => {
            drop(m)
            return report_map_failure(status)
        },
    }
    let pair = map_get(updated, "name")
    let found = pair.0
    drop(pair)
    drop(updated)
    if found { 1 } else { 0 }
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_map_size() {
    let src = r#"
func report_map_failure(status: i32) -> i32 {
    println("unexpected Map error: " + to_string(status))
    1000000 + status
}
func main() -> i32 {
    let m0 = match map_new() {
        Ok(value) => value,
        Err(status) => return report_map_failure(status),
    }
    let m1 = match map_set(m0, "a", 1) {
        Ok(value) => {
            drop(m0)
            value
        },
        Err(status) => {
            drop(m0)
            return report_map_failure(status)
        },
    }
    let m2 = match map_set(m1, "b", 2) {
        Ok(value) => {
            drop(m1)
            value
        },
        Err(status) => {
            drop(m1)
            return report_map_failure(status)
        },
    }
    let size = map_size(m2)
    drop(m2)
    size
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn builtin_map_remove() {
    let src = r#"
func report_map_failure(status: i32) -> i32 {
    println("unexpected Map error: " + to_string(status))
    1000000 + status
}
func main() -> i32 {
    let m0 = match map_new() {
        Ok(value) => value,
        Err(status) => return report_map_failure(status),
    }
    let m1 = match map_set(m0, "x", 1) {
        Ok(value) => {
            drop(m0)
            value
        },
        Err(status) => {
            drop(m0)
            return report_map_failure(status)
        },
    }
    let removed = map_remove(m1, "x")
    drop(m1)
    let size = map_size(removed)
    drop(removed)
    size
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
    let pair = map_get(m, "b")
    let found = pair.0
    let val = pair.1
    let result = if found { val } else { 0 }
    drop(pair)
    drop(pairs)
    drop(m)
    result
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

#[test]
fn builtin_getenv_exists() {
    let src = r#"
func observed_getenv_path() -> Result<string, string> {
    getenv("PATH")
}
func main() -> i32 {
    match observed_getenv_path() {
        Ok(path) => if len(path) > 0 { 0 } else { 1 },
        Err(_) => 2,
    }
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::Int(0),
        "getenv(\"PATH\") should return a non-empty Ok(string)"
    );
}

#[test]
fn builtin_getenv_missing() {
    let src = r#"
func observed_getenv_missing() -> Result<string, string> {
    getenv("MIMI_NONEXISTENT_VAR_ZZZ")
}
func main() -> i32 {
    match observed_getenv_missing() {
        Ok(_) => 1,
        Err(_) => 0,
    }
}
"#;
    assert_eq!(run_source(src), interp::Value::Int(0));
}

#[test]
fn builtin_args_exists() {
    let src = r#"
func main() -> i32 {
    len(args())
}
"#;
    let v = run_source(src);
    assert!(
        matches!(v, interp::Value::Int(n) if n >= 0),
        "args() should return a non-negative length"
    );
}

#[test]
fn builtin_now_returns_int() {
    let src = r#"
func main() -> i32 {
    now()
}
"#;
    let v = run_source(src);
    match v {
        interp::Value::Int(n) => assert!(
            n > 1000000000,
            "now() should return Unix timestamp > 1e9, got {}",
            n
        ),
        _ => panic!("expected i32, got {:?}", v),
    }
}

#[test]
fn builtin_now_ms_returns_int() {
    let src = r#"
func main() -> i32 {
    now_ms()
}
"#;
    let v = run_source(src);
    match v {
        interp::Value::Int(n) => assert!(
            n > 1000000000000,
            "now_ms() should return Unix ms > 1e12, got {}",
            n
        ),
        _ => panic!("expected i32, got {:?}", v),
    }
}

#[test]
fn builtin_timestamp_alias() {
    let src = r#"
func main() -> i32 {
    timestamp()
}
"#;
    let v = run_source(src);
    match v {
        interp::Value::Int(n) => assert!(
            n > 1000000000,
            "timestamp() should return Unix timestamp > 1e9, got {}",
            n
        ),
        _ => panic!("expected i32, got {:?}", v),
    }
}

#[test]
fn builtin_sleep_basic() {
    let src = r#"
func main() -> i32 {
    sleep(1);
    42
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}

#[test]
fn builtin_type_name_int() {
    let src = r#"
func observed_type_name_int() -> string {
    type_name(42)
}
func main() -> i32 {
    if observed_type_name_int() == "i32" { 0 } else { 1 }
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::Int(0),
        "type_name(42) should return i32"
    );
}

#[test]
fn builtin_type_name_string() {
    let src = r#"
func observed_type_name_string() -> string {
    type_name("hello")
}
func main() -> i32 {
    if observed_type_name_string() == "string" { 0 } else { 1 }
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::Int(0),
        "type_name(\"hello\") should return string"
    );
}

#[test]
fn builtin_type_name_bool() {
    let src = r#"
func observed_type_name_bool() -> string {
    type_name(true)
}
func main() -> i32 {
    if observed_type_name_bool() == "bool" { 0 } else { 1 }
}
"#;
    assert_eq!(
        run_source(src),
        interp::Value::Int(0),
        "type_name(true) should return bool"
    );
}

#[test]
fn builtin_push_in_if_block() {
    let src = r#"
func main() -> i32 {
    let mut xs = [1, 2]
    if true {
        push(xs, 3)
    }
    len(xs)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_push_in_while_loop() {
    let src = r#"
func main() -> i32 {
    let mut xs = [0]
    let mut i = 0
    while i < 3 {
        push(xs, i + 1)
        i += 1
    }
    len(xs)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(4));
}

#[test]
fn builtin_push_via_helper() {
    let src = r#"
func push_two(xs: List<i32>, a: i32, b: i32) -> i32 {
    let mut ys = xs;
    push(ys, a);
    push(ys, b);
    len(ys)
}

func main() -> i32 {
    push_two([1], 2, 3)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn builtin_option_value_or() {
    let src = r#"
func main() -> i32 {
    let found = str_index_of("hello", "ell")
    option_value_or(found, -1)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn builtin_option_value_or_default() {
    let src = r#"
func main() -> i32 {
    let found = str_index_of("hello", "xyz")
    option_value_or(found, -1)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(-1));
}

#[test]
fn builtin_value_or_method() {
    let src = r#"
func main() -> i32 {
    let found = str_index_of("hello", "xyz")
    found.value_or(-1)
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(-1));
}

#[test]
fn builtin_empty_list_as_type() {
    let src = r#"
func main() -> i32 {
    let mut xs = [] as List<i32>
    push(xs, 42)
    xs[0]
}
"#;
    let v = run_source(src);
    assert_eq!(v, interp::Value::Int(42));
}
