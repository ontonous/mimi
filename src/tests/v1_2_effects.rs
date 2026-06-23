use super::*;

#[test]
fn effect_declaration() {
    let src = r#"
cap FileReadCap;

func load_data(path: string) with FileReadCap {
    println(path);
}

func main() -> i32 {
    load_data("test.txt");
    42
}
"#;
    // Function with effect - FileReadCap is declared but not bound to a variable
    // So calling load_data should fail because the effect is not available
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("effect") && m.contains("not available")));
}

#[test]
fn effect_not_available() {
    let src = r#"
cap FileReadCap;

func load_data(path: string) with FileReadCap {
    println(path);
}

func main() -> i32 {
    // FileReadCap is not in scope here (only declared, not bound)
    load_data("test.txt");
    42
}
"#;
    // Function with effect should fail because effect cap is declared but not bound
    let result = check_source(src);
    assert!(result.is_err(), "calling function with unbound effect should fail");
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("effect") || m.contains("cap") || m.contains("not available")));
}

#[test]
fn effect_undeclared_cap_cross_validation() {
    let src = r#"
func load_data(path: string) with FileReadCap {
    println(path);
}

func main() -> i32 {
    42
}
"#;
    // FileReadCap is not declared as a cap, so the `with FileReadCap` should fail
    let result = check_source(src);
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let err_messages: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
    assert!(err_messages.iter().any(|m| m.contains("not a declared capability")));
}

#[test]
fn effect_available_via_function_chain() {
    let src = r#"
cap FileReadCap;

func load_data(path: string) with FileReadCap {
    println(path);
}

func process(path: string) with FileReadCap {
    load_data(path);  // OK because process also has FileReadCap
}

func main() -> i32 {
    42
}
"#;
    // FileReadCap is available in process() via `with`, so calling load_data() from process() succeeds
    let result = check_source(src);
    assert!(result.is_ok(), "expected success when chaining with-cap functions: {:?}", result);
}
