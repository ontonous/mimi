use super::*;

// ============================================================
// T604: Custom Allocators
// ============================================================

#[test]
fn alloc_system_basic() {
    let src = r#"
func main() -> i32 {
    let x = 42;
    x
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn builtin_allocator_system() {
    let src = r#"
func main() -> string {
    let a = allocator_system();
    match a {
        _ => "system"
    }
}
"#;
    assert_eq!(
        run_source_bytecode_result(src),
        Ok(interp::Value::String(Arc::new("system".to_string())))
    );
}

#[test]
fn builtin_allocator_arena() {
    let src = r#"
func main() -> string {
    let a = allocator_arena();
    match a {
        _ => "arena"
    }
}
"#;
    assert_eq!(
        run_source_bytecode_result(src),
        Ok(interp::Value::String(Arc::new("arena".to_string())))
    );
}

#[test]
fn builtin_allocator_bump() {
    let src = r#"
func main() -> string {
    let a = allocator_bump();
    match a {
        _ => "bump"
    }
}
"#;
    assert_eq!(
        run_source_bytecode_result(src),
        Ok(interp::Value::String(Arc::new("bump".to_string())))
    );
}

#[test]
fn builtin_alloc_with_system() {
    let src = r#"
func main() -> i32 {
    let a = allocator_system();
    let r = alloc(a, 42);
    r
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn builtin_bump_used() {
    let src = r#"
func main() -> i32 {
    bump_used()
}
"#;
    assert_eq!(run_source_bytecode_result(src), Ok(interp::Value::Int(0)));
}
