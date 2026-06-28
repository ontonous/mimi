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
    assert!(
        result.is_ok(),
        "extern block should parse and type-check: {:?}",
        result.err()
    );
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
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/nonexistent/lib.so");
    let result = run_source_result(src);
    assert!(
        result.is_err(),
        "calling extern with nonexistent lib should fail: {:?}",
        result.ok()
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to load")
            || err.contains("cannot find")
            || err.contains("not found")
            || err.contains("not set"),
        "error should mention library issue: {}",
        err
    );
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
    let _guard = FfiEnvLock::lock();
    std::env::set_var("MIMI_FFI_LIB", "/nonexistent/ffi_test_lib.so");
    let result = run_source_result(src);
    assert!(result.is_err(), "calling extern with bad lib should fail");
    let err = result.unwrap_err();
    assert!(
        err.contains("failed to load") || err.contains("cannot find") || err.contains("not found"),
        "error should mention library issue: {}",
        err
    );
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
    assert!(
        result.is_ok(),
        "multiple extern funcs should parse: {:?}",
        result.err()
    );
}

#[test]
fn extern_block_no_panic_attribute_parses() {
    let src = r#"
#[no_panic]
extern "C" {
    func safe_add(a: i32, b: i32) -> i32;
    func safe_greet(name: string);
}

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "#[no_panic] extern block should parse and type-check: {:?}",
        result.err()
    );
}

#[test]
fn extern_block_no_panic_attribute_preserved() {
    let src = r#"
#[no_panic]
extern "C" {
    func safe_add(a: i32, b: i32) -> i32;
    func safe_greet(name: string);
}

func main() -> i32 {
    42
}
"#;
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("tokenize ok");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse ok");
    let has_no_panic = file.items.iter().any(|item| {
        if let crate::ast::Item::ExternBlock(block) = item {
            block.no_panic && block.funcs.iter().all(|f| f.no_panic)
        } else {
            false
        }
    });
    assert!(
        has_no_panic,
        "#[no_panic] attribute should be preserved on ExternBlock and all ExternFuncs"
    );
}

#[allow(dead_code)]
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
    assert!(
        result.is_ok(),
        "void extern func should parse: {:?}",
        result.err()
    );
}
