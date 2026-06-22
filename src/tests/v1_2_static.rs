use super::*;

// ── T203: --strict mode tests ──

#[test]
fn strict_mode_locked_function_passes() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}

func main() -> i32 {
    add(1, 2)
}
"#;
    // Non-locked functions should pass strict mode
    let result = check_source_strict(src);
    assert!(result.is_ok(), "non-locked function should pass strict mode");
}

#[test]
fn strict_mode_normal_check_still_works() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}

func main() -> i32 {
    add(1, 2)
}
"#;
    // Normal check should also pass
    let result = check_source(src);
    assert!(result.is_ok());
}

// ── T204: 类型检查器增强（静态分析）tests ──

#[test]
fn static_check_missing_return_path() {
    let src = r#"
func maybe_return(x: i32) -> i32 {
    if x > 0 {
        return x;
    }
}

func main() -> i32 {
    maybe_return(5)
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "missing return path should be an error");
    let errors = result.unwrap_err();
    let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("does not return on all paths")), "Expected return path error, got: {:?}", msgs);
}

#[test]
fn static_check_all_return_paths_ok() {
    let src = r#"
func maybe_return(x: i32) -> i32 {
    if x > 0 {
        return x;
    } else {
        return 0;
    }
}

func main() -> i32 {
    maybe_return(5)
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "all return paths should pass: {:?}", result.err());
}

#[test]
fn static_check_unreachable_after_return() {
    let src = r#"
func test() -> i32 {
    return 42;
    let x = 1;
}

func main() -> i32 {
    test()
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "unreachable code after return should be an error");
    let errors = result.unwrap_err();
    let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("unreachable statement")), "Expected unreachable error, got: {:?}", msgs);
}

#[test]
fn static_check_mut_enforcement() {
    let src = r#"
func main() -> i32 {
    let x = 5;
    x = 10;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "assigning to immutable variable should be an error");
    let errors = result.unwrap_err();
    let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("cannot assign to immutable")), "Expected mut error, got: {:?}", msgs);
}

#[test]
fn static_check_mut_allowed() {
    let src = r#"
func main() -> i32 {
    let mut x = 5;
    x = 10;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_ok(), "assigning to mutable variable should pass: {:?}", result.err());
}

#[test]
fn static_check_shadowing_warning() {
    let src = r#"
func main() -> i32 {
    let x = 1;
    let x = 2;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "variable shadowing should produce an error");
    let errors = result.unwrap_err();
    let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("shadows")), "Expected shadowing error, got: {:?}", msgs);
}

#[test]
fn static_check_divide_by_zero() {
    let src = r#"
func main() -> i32 {
    let x = 10 / 0;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "division by zero literal should be an error");
    let errors = result.unwrap_err();
    let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("division by zero")), "Expected divide-by-zero error, got: {:?}", msgs);
}

#[test]
fn static_check_modulo_by_zero() {
    let src = r#"
func main() -> i32 {
    let x = 10 % 0;
    x
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "modulo by zero literal should be an error");
    let errors = result.unwrap_err();
    let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("modulo by zero")), "Expected modulo-by-zero error, got: {:?}", msgs);
}

#[test]
fn static_check_alias_cycle() {
    let src = r#"
type A = B;
type B = A;

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    assert!(result.is_err(), "type alias cycle should be an error");
    let errors = result.unwrap_err();
    let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("type alias cycle")), "Expected alias cycle error, got: {:?}", msgs);
}

#[test]
fn strict_locked_func_no_mms_ok() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
    let result = check_source_strict(src);
    assert!(result.is_ok());
}

#[test]
fn verify_rules_valid() {
    let src = r#"
func main() -> i32 {
    desc "this is a description";
    return 0;
}
"#;
    let file = parse(src);
    let errors = core::verify_rules(&file);
    assert!(errors.is_empty());
}
