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
func allocator_system_name() -> string {
    let a = allocator_system();
    match a {
        _ => "system"
    }
}
func main() -> i32 {
    println(allocator_system_name());
    0
}
"#;
    let (value, stdout) = run_source_bytecode_with_stdout(src);
    assert_eq!(value, interp::Value::Int(0));
    assert_eq!(stdout.trim(), "system");
}

#[test]
fn builtin_allocator_arena() {
    let src = r#"
func allocator_arena_name() -> string {
    let a = allocator_arena();
    match a {
        _ => "arena"
    }
}
func main() -> i32 {
    println(allocator_arena_name());
    0
}
"#;
    let (value, stdout) = run_source_bytecode_with_stdout(src);
    assert_eq!(value, interp::Value::Int(0));
    assert_eq!(stdout.trim(), "arena");
}

#[test]
fn builtin_allocator_bump() {
    let src = r#"
func allocator_bump_name() -> string {
    let a = allocator_bump();
    match a {
        _ => "bump"
    }
}
func main() -> i32 {
    println(allocator_bump_name());
    0
}
"#;
    let (value, stdout) = run_source_bytecode_with_stdout(src);
    assert_eq!(value, interp::Value::Int(0));
    assert_eq!(stdout.trim(), "bump");
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
