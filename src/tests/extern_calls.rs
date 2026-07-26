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

// === SD-3: #[errno] attribute tests ===

#[test]
fn sd3_errno_block_attribute_parses() {
    let src = r#"
#[errno]
extern "C" {
    func open(path: string, flags: i32) -> i32;
    func close(fd: i32) -> i32;
}

func main() -> i32 { 0 }
"#;
    let result = check_source(src);
    assert!(
        result.is_ok(),
        "#[errno] extern block should parse and type-check: {:?}",
        result.err()
    );
}

#[test]
fn sd3_errno_block_attribute_preserved() {
    let src = r#"
#[errno]
extern "C" {
    func open(path: string, flags: i32) -> i32;
    func close(fd: i32) -> i32;
}

func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("tokenize ok");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse ok");
    let has_errno = file.items.iter().any(|item| {
        if let crate::ast::Item::ExternBlock(block) = item {
            block.returns_errno && block.funcs.iter().all(|f| f.returns_errno)
        } else {
            false
        }
    });
    assert!(
        has_errno,
        "#[errno] attribute should be preserved on ExternBlock and all ExternFuncs"
    );
}

#[test]
fn sd3_errno_per_function_attribute() {
    let src = r#"
extern "C" {
    #[errno]
    func open(path: string, flags: i32) -> i32;
    func printf(fmt: string) -> i32;
}

func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("tokenize ok");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse ok");
    let extern_block = file.items.iter().find_map(|item| {
        if let crate::ast::Item::ExternBlock(block) = item {
            Some(block)
        } else {
            None
        }
    });
    let block = extern_block.expect("extern block");
    assert!(!block.returns_errno, "block-level errno should be false");
    let open_func = block.funcs.iter().find(|f| f.name == "open").expect("open");
    let printf_func = block
        .funcs
        .iter()
        .find(|f| f.name == "printf")
        .expect("printf");
    assert!(open_func.returns_errno, "open should have errno=true");
    assert!(!printf_func.returns_errno, "printf should have errno=false");
}

#[test]
fn sd3_errno_contract_uses_flag() {
    use crate::ffi::contract::FfiContract;
    let src = r#"
extern "C" {
    #[errno]
    func my_custom_open(path: string) -> i32;
    func my_custom_read(fd: i32) -> i32;
}

func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("tokenize ok");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse ok");
    let extern_block = file.items.iter().find_map(|item| {
        if let crate::ast::Item::ExternBlock(block) = item {
            Some(block)
        } else {
            None
        }
    });
    let block = extern_block.expect("extern block");
    let custom_open = block
        .funcs
        .iter()
        .find(|f| f.name == "my_custom_open")
        .expect("my_custom_open");
    let custom_read = block
        .funcs
        .iter()
        .find(|f| f.name == "my_custom_read")
        .expect("my_custom_read");

    // #[errno] flag → check_errno=true (even though name is not in ERRNO_CHECK_FUNC_NAMES)
    let contract_open = FfiContract::from_extern(custom_open);
    assert!(
        contract_open.check_errno,
        "#[errno] attribute must enable errno checking for non-standard names"
    );

    // No #[errno] and name not in ERRNO_CHECK_FUNC_NAMES → check_errno=false
    let contract_read = FfiContract::from_extern(custom_read);
    assert!(
        !contract_read.check_errno,
        "unannotated non-standard name must not enable errno checking"
    );
}

#[test]
fn sd3_errno_backward_compat_name_guessing() {
    use crate::ffi::contract::FfiContract;
    // Legacy: "open" is in ERRNO_CHECK_FUNC_NAMES, so even without #[errno],
    // check_errno should be true (transition period fallback).
    let src = r#"
extern "C" {
    func open(path: string, flags: i32) -> i32;
}

func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("tokenize ok");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse ok");
    let extern_block = file.items.iter().find_map(|item| {
        if let crate::ast::Item::ExternBlock(block) = item {
            Some(block)
        } else {
            None
        }
    });
    let block = extern_block.expect("extern block");
    let open_func = block.funcs.iter().find(|f| f.name == "open").expect("open");
    let contract = FfiContract::from_extern(open_func);
    assert!(
        contract.check_errno,
        "legacy name-guessing must still work during transition period"
    );
}

#[test]
fn sd3_errno_attribute_rejected_on_non_extern() {
    let src = r#"
#[errno]
func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(src)
        .tokenize()
        .expect("tokenize ok");
    let result = crate::parser::Parser::new(tokens).parse_file();
    assert!(result.is_err(), "#[errno] on non-extern should be rejected");
}
