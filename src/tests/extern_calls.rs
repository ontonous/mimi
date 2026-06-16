use super::*;

#[test]
fn extern_block_parsing() {
    let src = r#"
extern "C" {
    func add(a: i32, b: i32) -> i32;
    func greet(name: string);
}

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "extern block should parse and type-check: {:?}", result.err());
}

#[test]
fn extern_func_not_found_in_nonexistent_lib() {
    let src = r#"
extern "C" {
    func missing_func(x: i32) -> i32;
}

func main() -> i32 {
    missing_func(42)
}
"#;
    std::env::set_var("MIMI_FFI_LIB", "/nonexistent/lib.so");
    let result = run_source_result(src);
    assert!(result.is_err(), "calling extern with nonexistent lib should fail: {:?}", result.ok());
    let err = result.unwrap_err();
    assert!(err.contains("failed to load") || err.contains("cannot find") || err.contains("not found") || err.contains("not set"),
        "error should mention library issue: {}", err);
    std::env::remove_var("MIMI_FFI_LIB");
}

#[test]
fn extern_func_no_lib_env() {
    let src = r#"
extern "C" {
    func my_func(x: i32) -> i32;
}

func main() -> i32 {
    my_func(1)
}
"#;
    std::env::set_var("MIMI_FFI_LIB", "/nonexistent/ffi_test_lib.so");
    let result = run_source_result(src);
    assert!(result.is_err(), "calling extern with bad lib should fail");
    let err = result.unwrap_err();
    assert!(err.contains("failed to load") || err.contains("cannot find") || err.contains("not found"),
        "error should mention library issue: {}", err);
    std::env::remove_var("MIMI_FFI_LIB");
}

#[test]
fn extern_block_multiple_funcs() {
    let src = r#"
extern "C" {
    func add(a: i32, b: i32) -> i32;
    func multiply(a: i32, b: i32) -> i32;
    func void_func();
}

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "multiple extern funcs should parse: {:?}", result.err());
}

#[test]
fn extern_func_with_no_return() {
    let src = r#"
extern "C" {
    func do_nothing(x: i32);
}

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "void extern func should parse: {:?}", result.err());
}
