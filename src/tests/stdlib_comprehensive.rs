// Comprehensive tests for stdlib modules (v0.28.0)
// Covers: fs, crypto, paths, strings, collections, json, io, math

use super::*;

// ====== FS Module Tests ======

#[test]
fn std_fs_exists_true() {
    let v = run_source("func main() -> i32 { if file_exists(\"examples/hello.mimi\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_fs_exists_false() {
    let v = run_source("func main() -> i32 { if file_exists(\"/nonexistent\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_fs_read_file() {
    let v = run_source("func main() -> i32 { let r = read_file(\"examples/hello.mimi\"); if r.is_ok() { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_fs_read_nonexistent() {
    let v = run_source("func main() -> i32 { let r = read_file(\"/nonexistent\"); if r.is_ok() { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_fs_listdir_count() {
    let v = run_source("func main() -> i32 { len(listdir(\"examples\")) }");
    match v { interp::Value::Int(n) => assert!(n > 0), _ => panic!("expected Int") }
}

#[test]
fn std_fs_listdir_empty() {
    let v = run_source("func main() -> i32 { len(listdir(\"/nonexistent\")) }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_fs_walk_dir_count() {
    let v = run_source("func main() -> i32 { len(walk_dir(\"examples\")) }");
    match v { interp::Value::Int(n) => assert!(n > 20), _ => panic!("expected Int") }
}

#[test]
fn std_fs_is_dir_true() {
    let v = run_source("func main() -> i32 { if is_dir(\"examples\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_fs_is_dir_false() {
    let v = run_source("func main() -> i32 { if is_dir(\"examples/hello.mimi\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_fs_is_file_true() {
    let v = run_source("func main() -> i32 { if is_file(\"examples/hello.mimi\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_fs_is_file_false() {
    let v = run_source("func main() -> i32 { if is_file(\"examples\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_fs_mkdir_p() {
    let v = run_source("func main() -> i32 { if mkdir_p(\"/tmp/mimi_test_dir\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
    std::fs::remove_dir("/tmp/mimi_test_dir").ok();
}

#[test]
fn std_fs_remove_nonexistent() {
    let v = run_source("func main() -> i32 { if remove_file(\"/nonexistent\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

// ====== Path Tests ======

#[test]
fn std_path_join_basic() {
    let v = run_source("func main() -> string { path_join(\"a\", \"b\") }");
    assert_eq!(v, interp::Value::String("a/b".to_string()));
}

#[test]
fn std_path_join_empty_a() {
    let v = run_source("func main() -> string { path_join(\"\", \"b\") }");
    assert_eq!(v, interp::Value::String("b".to_string()));
}

#[test]
fn std_path_join_empty_b() {
    let v = run_source("func main() -> string { path_join(\"a\", \"\") }");
    // path_join("a", "") may return "a/" depending on implementation
    match v {
        interp::Value::String(s) => assert!(s == "a" || s == "a/", "got '{}'", s),
        _ => panic!("expected String"),
    }
}

#[test]
fn std_path_join_absolute() {
    let v = run_source("func main() -> string { path_join(\"/usr\", \"lib\") }");
    assert_eq!(v, interp::Value::String("/usr/lib".to_string()));
}

#[test]
fn std_path_join_chain() {
    let v = run_source("func main() -> string { path_join(path_join(\"a\", \"b\"), \"c\") }");
    assert_eq!(v, interp::Value::String("a/b/c".to_string()));
}

#[test]
fn std_path_ext_txt() {
    let v = run_source("func main() -> string { path_ext(\"file.txt\") }");
    assert_eq!(v, interp::Value::String("txt".to_string()));
}

#[test]
fn std_path_ext_mimi() {
    let v = run_source("func main() -> string { path_ext(\"test.mimi\") }");
    assert_eq!(v, interp::Value::String("mimi".to_string()));
}

#[test]
fn std_path_ext_none() {
    let v = run_source("func main() -> i32 { len(path_ext(\"Makefile\")) }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_path_ext_double() {
    let v = run_source("func main() -> string { path_ext(\"archive.tar.gz\") }");
    assert_eq!(v, interp::Value::String("gz".to_string()));
}

#[test]
fn std_path_basename_simple() {
    let v = run_source("func main() -> string { path_basename(\"/a/b/c.txt\") }");
    assert_eq!(v, interp::Value::String("c.txt".to_string()));
}

#[test]
fn std_path_basename_no_dir() {
    let v = run_source("func main() -> string { path_basename(\"file.txt\") }");
    assert_eq!(v, interp::Value::String("file.txt".to_string()));
}

#[test]
fn std_path_dirname_simple() {
    let v = run_source("func main() -> string { path_dirname(\"/a/b/c.txt\") }");
    assert_eq!(v, interp::Value::String("/a/b".to_string()));
}

#[test]
fn std_path_dirname_no_dir() {
    let v = run_source("func main() -> string { path_dirname(\"file.txt\") }");
    assert_eq!(v, interp::Value::String("".to_string()));
}

// ====== Crypto Tests ======

#[test]
fn std_sha256_hello() {
    let v = run_source("func main() -> string { sha256(\"hello\") }");
    assert_eq!(v, interp::Value::String("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string()));
}

#[test]
fn std_sha256_empty() {
    let v = run_source("func main() -> string { sha256(\"\") }");
    assert_eq!(v, interp::Value::String("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()));
}

#[test]
fn std_sha256_abc() {
    let v = run_source("func main() -> string { sha256(\"abc\") }");
    assert_eq!(v, interp::Value::String("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string()));
}

#[test]
fn std_sha256_len() {
    let v = run_source("func main() -> i32 { len(sha256(\"test\")) }");
    assert_eq!(v, interp::Value::Int(64));
}

#[test]
fn std_base64_encode_hello() {
    let v = run_source("func main() -> string { base64_encode(\"Hello\") }");
    assert_eq!(v, interp::Value::String("SGVsbG8=".to_string()));
}

#[test]
fn std_base64_encode_empty() {
    let v = run_source("func main() -> string { base64_encode(\"\") }");
    assert_eq!(v, interp::Value::String("".to_string()));
}

#[test]
fn std_base64_encode_long() {
    let v = run_source("func main() -> string { base64_encode(\"Hello, World!\") }");
    assert_eq!(v, interp::Value::String("SGVsbG8sIFdvcmxkIQ==".to_string()));
}

#[test]
fn std_base64_decode_valid() {
    let v = run_source(r#"func main() -> string { let r = base64_decode("SGVsbG8="); match r { Ok(s) => s, Err(e) => "err" } }"#);
    assert_eq!(v, interp::Value::String("Hello".to_string()));
}

#[test]
fn std_base64_decode_invalid() {
    let v = run_source(r#"func main() -> string { let r = base64_decode("not!valid!"); match r { Ok(s) => s, Err(e) => "err" } }"#);
    assert_eq!(v, interp::Value::String("err".to_string()));
}

#[test]
fn std_base64_roundtrip() {
    let v = run_source(r#"func main() -> string { let e = base64_encode("Mimi"); let r = base64_decode(e); match r { Ok(s) => s, Err(e) => "err" } }"#);
    assert_eq!(v, interp::Value::String("Mimi".to_string()));
}

// ====== String Tests ======

#[test]
fn std_str_split_count() {
    let v = run_source("func main() -> i32 { len(str_split(\"a,b,c\", \",\")) }");
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn std_str_join_basic() {
    let v = run_source("func main() -> string { str_join([\"a\", \"b\", \"c\"], \",\") }");
    assert_eq!(v, interp::Value::String("a,b,c".to_string()));
}

#[test]
fn std_str_contains_true() {
    let v = run_source("func main() -> i32 { if str_contains(\"hello world\", \"world\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_str_contains_false() {
    let v = run_source("func main() -> i32 { if str_contains(\"hello\", \"xyz\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_str_starts_with_true() {
    let v = run_source("func main() -> i32 { if str_starts_with(\"hello\", \"hel\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_str_ends_with_true() {
    let v = run_source("func main() -> i32 { if str_ends_with(\"hello\", \"llo\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_str_replace_basic() {
    let v = run_source("func main() -> string { str_replace(\"hello world\", \"world\", \"mimi\") }");
    assert_eq!(v, interp::Value::String("hello mimi".to_string()));
}

#[test]
fn std_str_to_upper() {
    let v = run_source("func main() -> string { str_to_upper(\"hello\") }");
    assert_eq!(v, interp::Value::String("HELLO".to_string()));
}

#[test]
fn std_str_to_lower() {
    let v = run_source("func main() -> string { str_to_lower(\"HELLO\") }");
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

#[test]
fn std_str_trim() {
    let v = run_source("func main() -> string { str_trim(\"  hello  \") }");
    assert_eq!(v, interp::Value::String("hello".to_string()));
}

#[test]
fn std_str_len() {
    let v = run_source("func main() -> i32 { len(\"hello\") }");
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn std_str_char_at() {
    let v = run_source("func main() -> string { str_char_at(\"hello\", 1) }");
    assert_eq!(v, interp::Value::String("e".to_string()));
}

#[test]
fn std_str_substring() {
    let v = run_source("func main() -> string { str_substring(\"hello\", 1, 3) }");
    assert_eq!(v, interp::Value::String("el".to_string()));
}

#[test]
fn std_str_repeat() {
    let v = run_source("func main() -> string { str_repeat(\"ab\", 3) }");
    assert_eq!(v, interp::Value::String("ababab".to_string()));
}

#[test]
fn std_str_index_of_found() {
    let v = run_source("func main() -> i32 { str_index_of(\"hello\", \"ll\") }");
    // str_index_of returns Option<i32> = Variant("Some", [Int]) or Variant("None", [])
    match v {
        interp::Value::Variant(name, vals) => {
            assert_eq!(name, "Some");
            if let interp::Value::Int(n) = vals[0] { assert_eq!(n, 2); }
        }
        _ => panic!("expected Option variant, got {:?}", v),
    }
}

#[test]
fn std_str_index_of_not_found() {
    let v = run_source("func main() -> i32 { str_index_of(\"hello\", \"xyz\") }");
    // str_index_of returns None
    match v {
        interp::Value::Variant(name, _) => {
            assert_eq!(name, "None");
        }
        _ => panic!("expected None variant, got {:?}", v),
    }
}

// ====== Regex Tests ======

#[test]
fn std_regex_match_true() {
    let v = run_source("func main() -> i32 { if regex_match(\"hello123\", \"[a-z]+[0-9]+\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_regex_match_false() {
    let v = run_source("func main() -> i32 { if regex_match(\"123\", \"[a-z]+\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_regex_find_basic() {
    let v = run_source("func main() -> string { regex_find(\"abc 123 def\", \"[0-9]+\") }");
    assert_eq!(v, interp::Value::String("123".to_string()));
}

#[test]
fn std_regex_replace_basic() {
    let v = run_source("func main() -> string { regex_replace(\"hello world\", \"world\", \"mimi\") }");
    assert_eq!(v, interp::Value::String("hello mimi".to_string()));
}

// ====== Math Tests ======

#[test]
fn std_math_abs_positive() {
    let v = run_source("func main() -> i32 { abs(5) }");
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn std_math_abs_negative() {
    let v = run_source("func main() -> i32 { abs(-5) }");
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn std_math_min() {
    let v = run_source("func main() -> i32 { min(3, 7) }");
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn std_math_max() {
    let v = run_source("func main() -> i32 { max(3, 7) }");
    assert_eq!(v, interp::Value::Int(7));
}

#[test]
fn std_math_sqrt() {
    let v = run_source("func main() -> f64 { sqrt(9.0) }");
    assert_eq!(v, interp::Value::Float(3.0));
}

#[test]
fn std_math_pow() {
    let v = run_source("func main() -> f64 { pow(2.0, 3.0) }");
    assert_eq!(v, interp::Value::Float(8.0));
}

#[test]
fn std_math_floor() {
    let v = run_source("func main() -> i32 { floor(3.7) }");
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn std_math_ceil() {
    let v = run_source("func main() -> i32 { ceil(3.2) }");
    assert_eq!(v, interp::Value::Int(4));
}

// ====== Collection Tests ======

#[test]
fn std_list_len() {
    let v = run_source("func main() -> i32 { len([1, 2, 3]) }");
    assert_eq!(v, interp::Value::Int(3));
}

#[test]
fn std_list_contains_true() {
    let v = run_source("func main() -> i32 { if contains([1, 2, 3], 2) { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_list_contains_false() {
    let v = run_source("func main() -> i32 { if contains([1, 2, 3], 5) { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_list_push_pop() {
    let v = run_source("func main() -> i32 { let xs = [1, 2]; push(xs, 3); pop(xs); len(xs) }");
    // pop removes an element, list length should decrease
    assert_eq!(v, interp::Value::Int(2));
}

#[test]
fn std_list_range() {
    let v = run_source("func main() -> i32 { let r = range(0, 5); len(r) }");
    assert_eq!(v, interp::Value::Int(5));
}

#[test]
fn std_list_sum() {
    let v = run_source("func main() -> i32 { sum([1, 2, 3, 4, 5]) }");
    assert_eq!(v, interp::Value::Int(15));
}

#[test]
fn std_list_reverse() {
    let v = run_source("func main() -> i32 { let xs = [1, 2, 3]; reverse(xs); len(xs) }");
    // reverse modifies in-place, check length is preserved
    assert_eq!(v, interp::Value::Int(3));
}

// ====== Conversion Tests ======

#[test]
fn std_to_string_int() {
    let v = run_source("func main() -> string { to_string(42) }");
    assert_eq!(v, interp::Value::String("42".to_string()));
}

#[test]
fn std_to_string_float() {
    let v = run_source("func main() -> string { to_string(3.14) }");
    assert_eq!(v, interp::Value::String("3.14".to_string()));
}

#[test]
fn std_str_parse_int() {
    let v = run_source("func main() -> i32 { str_parse_int(\"42\") }");
    // str_parse_int may return int directly or tuple
    match v {
        interp::Value::Int(n) => assert_eq!(n, 42),
        interp::Value::Tuple(vals) => {
            if let interp::Value::Int(n) = vals[1] { assert_eq!(n, 42); }
        }
        _ => panic!("expected Int or Tuple, got {:?}", v),
    }
}

#[test]
fn std_str_parse_float() {
    let v = run_source("func main() -> f64 { str_parse_float(\"3.14\") }");
    // str_parse_float may return float directly or tuple
    match v {
        interp::Value::Float(n) => assert!((n - 3.14).abs() < 0.001),
        interp::Value::Tuple(vals) => {
            if let interp::Value::Float(n) = vals[1] { assert!((n - 3.14).abs() < 0.001); }
        }
        _ => panic!("expected Float or Tuple, got {:?}", v),
    }
}

// ====== Control Flow Tests ======

#[test]
fn std_if_else_true() {
    let v = run_source("func main() -> i32 { if true { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_if_else_false() {
    let v = run_source("func main() -> i32 { if false { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_while_loop() {
    let v = run_source("func main() -> i32 { let mut i = 0; let mut sum = 0; while i < 5 { sum = sum + i; i = i + 1; } sum }");
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn std_for_loop() {
    let v = run_source("func main() -> i32 { let mut sum = 0; for i in range(0, 5) { sum = sum + i; } sum }");
    assert_eq!(v, interp::Value::Int(10));
}

#[test]
fn std_match_basic() {
    let v = run_source("func main() -> i32 { let x = 2; match x { 1 => 10, 2 => 20, 3 => 30, _ => 0 } }");
    assert_eq!(v, interp::Value::Int(20));
}

// ====== Error Handling Tests ======

#[test]
fn std_result_ok() {
    let v = run_source("func main() -> i32 { let r = read_file(\"examples/hello.mimi\"); if r.is_ok() { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_result_err() {
    let v = run_source("func main() -> i32 { let r = read_file(\"/nonexistent\"); if r.is_ok() { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

// ====== JSON Tests ======

#[test]
fn std_to_json_string() {
    let v = run_source("func main() -> string { to_json(\"hello\") }");
    assert_eq!(v, interp::Value::String("\"hello\"".to_string()));
}

#[test]
fn std_to_json_int() {
    let v = run_source("func main() -> string { to_json(42) }");
    assert_eq!(v, interp::Value::String("42".to_string()));
}

#[test]
fn std_is_valid_json_true() {
    let v = run_source("func main() -> i32 { if json_is_valid(\"{\\\"a\\\":1}\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(1));
}

#[test]
fn std_is_valid_json_false() {
    let v = run_source("func main() -> i32 { if json_is_valid(\"not json\") { 1 } else { 0 } }");
    assert_eq!(v, interp::Value::Int(0));
}

#[test]
fn std_json_get_string() {
    let v = run_source("func main() -> string { json_get_string(\"{\\\"name\\\":\\\"Mimi\\\"}\", \"name\") }");
    assert_eq!(v, interp::Value::String("Mimi".to_string()));
}

#[test]
fn std_json_get_int() {
    let v = run_source("func main() -> i32 { json_get_int(\"{\\\"n\\\":42}\", \"n\") }");
    assert_eq!(v, interp::Value::Int(42));
}
