// Additional codegen and FFI boundary tests (v0.28.0)
// Focused on codegen path coverage

use super::*;

fn can_codegen() -> bool {
    std::process::Command::new("cc").arg("--version").output().is_ok()
}

// ====== Codegen String Operations ======

#[test]
fn cg_str_concat() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(\"hello\" + \" \" + \"world\"); 0 }").unwrap();
    assert_eq!(out.trim(), "hello world");
}

#[test]
fn cg_str_len() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(len(\"hello\")); 0 }").unwrap();
    assert_eq!(out.trim(), "5");
}

#[test]
fn cg_str_contains() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { if str_contains(\"hello\", \"ell\") { println(\"yes\") } else { println(\"no\") } 0 }").unwrap();
    assert_eq!(out.trim(), "yes");
}

#[test]
fn cg_str_split() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let parts = str_split(\"a,b,c\", \",\"); println(len(parts)); 0 }").unwrap();
    assert_eq!(out.trim(), "3");
}

#[test]
fn cg_str_replace() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(str_replace(\"hello\", \"l\", \"r\")); 0 }").unwrap();
    assert_eq!(out.trim(), "herro");
}

// cg_str_to_upper: known codegen limitation — string ops may return garbage
// cg_str_trim: known codegen limitation — string ops may return garbage

// ====== Codegen Math Operations ======

#[test]
fn cg_abs() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(abs(-5)); 0 }").unwrap();
    assert_eq!(out.trim(), "5");
}

#[test]
fn cg_min_max() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(min(3, 7)); println(max(3, 7)); 0 }").unwrap();
    assert_eq!(out.trim(), "3\n7");
}

#[test]
fn cg_sqrt() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(sqrt(9.0)); 0 }").unwrap();
    assert!(out.trim().starts_with("3"), "expected 3.x, got {}", out.trim());
}

// ====== Codegen List Operations ======

#[test]
fn cg_list_len() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(len([1, 2, 3])); 0 }").unwrap();
    assert_eq!(out.trim(), "3");
}

#[test]
fn cg_list_contains() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { if contains([1, 2, 3], 2) { println(\"yes\") } else { println(\"no\") } 0 }").unwrap();
    assert_eq!(out.trim(), "yes");
}

#[test]
fn cg_list_sum() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(sum([1, 2, 3, 4, 5])); 0 }").unwrap();
    assert_eq!(out.trim(), "15");
}

#[test]
fn cg_list_range() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let r = range(0, 5); println(len(r)); 0 }").unwrap();
    assert_eq!(out.trim(), "5");
}

// ====== Codegen Control Flow ======

#[test]
fn cg_if_else() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { if true { println(\"yes\") } else { println(\"no\") } 0 }").unwrap();
    assert_eq!(out.trim(), "yes");
}

#[test]
fn cg_while_loop() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let mut i = 0; let mut s = 0; while i < 5 { s = s + i; i = i + 1; } println(s); 0 }").unwrap();
    assert_eq!(out.trim(), "10");
}

#[test]
fn cg_for_loop() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let mut s = 0; for i in range(0, 5) { s = s + i; } println(s); 0 }").unwrap();
    assert_eq!(out.trim(), "10");
}

#[test]
fn cg_match_basic() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let x = 2; match x { 1 => println(\"one\"), 2 => println(\"two\"), _ => println(\"other\") } 0 }").unwrap();
    assert_eq!(out.trim(), "two");
}

// ====== Codegen Crypto ======

#[test]
fn cg_sha256_hello() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(sha256(\"hello\")); 0 }").unwrap();
    assert_eq!(out.trim(), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
}

#[test]
fn cg_base64_encode() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(base64_encode(\"Hello\")); 0 }").unwrap();
    assert_eq!(out.trim(), "SGVsbG8=");
}

// ====== Codegen Path Operations ======

#[test]
fn cg_path_join() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(path_join(\"a\", \"b\")); 0 }").unwrap();
    assert_eq!(out.trim(), "a/b");
}

#[test]
fn cg_path_ext() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(path_ext(\"file.txt\")); 0 }").unwrap();
    assert_eq!(out.trim(), "txt");
}

#[test]
fn cg_path_basename() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(path_basename(\"/a/b/c.txt\")); 0 }").unwrap();
    assert_eq!(out.trim(), "c.txt");
}

#[test]
fn cg_path_dirname() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(path_dirname(\"/a/b/c.txt\")); 0 }").unwrap();
    assert_eq!(out.trim(), "/a/b");
}

#[test]
fn cg_is_dir() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { if is_dir(\".\") { println(\"dir\") } else { println(\"not\") } 0 }").unwrap();
    assert_eq!(out.trim(), "dir");
}

#[test]
fn cg_is_file() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { if is_file(\"examples/hello.mimi\") { println(\"file\") } else { println(\"not\") } 0 }").unwrap();
    assert_eq!(out.trim(), "file");
}

#[test]
fn cg_listdir() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let e = listdir(\"examples\"); println(len(e)); 0 }").unwrap();
    let n: i32 = out.trim().parse().unwrap_or(0);
    assert!(n > 0);
}

#[test]
fn cg_walk_dir() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let e = walk_dir(\"examples\"); println(len(e)); 0 }").unwrap();
    let n: i32 = out.trim().parse().unwrap_or(0);
    assert!(n > 10);
}

#[test]
fn cg_for_listdir() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { let mut c = 0; for e in listdir(\"examples\") { c = c + 1; } println(c); 0 }").unwrap();
    let n: i32 = out.trim().parse().unwrap_or(0);
    assert!(n > 0);
}

// ====== Codegen Record Operations ======

#[test]
fn cg_record_create() {
    if !can_codegen() { return; }
    let out = compile_and_run(r#"
type Point { x: i32, y: i32 }
func main() -> i32 {
    let p = Point { x: 3, y: 4 }
    println(p.x)
    println(p.y)
    0
}
"#).unwrap();
    assert_eq!(out.trim(), "3\n4");
}

// cg_record_field_access: known codegen limitation — record passing returns garbage

// ====== Codegen Function Calls ======

#[test]
fn cg_func_call() {
    if !can_codegen() { return; }
    let out = compile_and_run(r#"
func add(a: i32, b: i32) -> i32 { a + b }
func main() -> i32 { println(add(3, 4)); 0 }
"#).unwrap();
    assert_eq!(out.trim(), "7");
}

#[test]
fn cg_func_recursion() {
    if !can_codegen() { return; }
    let out = compile_and_run(r#"
func fib(n: i32) -> i32 { if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }
func main() -> i32 { println(fib(10)); 0 }
"#).unwrap();
    assert_eq!(out.trim(), "55");
}

#[test]
fn cg_closure_basic() {
    if !can_codegen() { return; }
    let out = compile_and_run(r#"
func main() -> i32 {
    let add = fn(a: i32, b: i32) -> i32 { a + b }
    println(add(3, 4))
    0
}
"#).unwrap();
    assert_eq!(out.trim(), "7");
}

// cg_result_ok/err: known codegen limitation — .is_ok() method not compiled for Result

// ====== Codegen Conversion ======

#[test]
fn cg_to_string_int() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(to_string(42)); 0 }").unwrap();
    assert_eq!(out.trim(), "42");
}

#[test]
fn cg_to_string_float() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(to_string(3.14)); 0 }").unwrap();
    assert!(out.trim().starts_with("3.14"), "expected 3.14..., got {}", out.trim());
}

// ====== Codegen JSON ======

#[test]
fn cg_to_json_string() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(to_json(\"hello\")); 0 }").unwrap();
    assert_eq!(out.trim(), "\"hello\"");
}

#[test]
fn cg_to_json_int() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { println(to_json(42)); 0 }").unwrap();
    assert_eq!(out.trim(), "42");
}

#[test]
fn cg_is_valid_json() {
    if !can_codegen() { return; }
    let out = compile_and_run("func main() -> i32 { if json_is_valid(\"{\\\"a\\\":1}\") { println(\"valid\") } else { println(\"invalid\") } 0 }").unwrap();
    assert_eq!(out.trim(), "valid");
}
