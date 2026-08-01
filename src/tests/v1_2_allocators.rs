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
    assert_eq!(run_source_treewalker(src), interp::Value::Int(42));
}

#[test]
fn alloc_arena_block() {
    let src = r#"
func main() -> i32 {
    let mut result = 0;
    alloc(Arena) {
        result = 10;
    }
    result
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(10));
}

#[test]
fn alloc_bump_block() {
    let src = r#"
func main() -> string {
    let mut msg = "start";
    alloc(Bump) {
        msg = "inside bump";
    }
    msg
}
"#;
    assert_eq!(
        run_source_treewalker(src),
        interp::Value::String("inside bump".to_string())
    );
}

#[test]
fn alloc_system_block() {
    let src = r#"
func main() -> i32 {
    let mut x = 0;
    alloc(System) {
        x = 99;
    }
    x
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(99));
}

#[test]
fn alloc_arena_nested_scopes() {
    let src = r#"
func main() -> i32 {
    let mut a = 0;
    alloc(Arena) {
        a = 1;
        alloc(Arena) {
            a = 2;
        }
        a
    }
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(2));
}

#[test]
fn alloc_bump_with_list() {
    let src = r#"
func main() -> i32 {
    let mut result = 0;
    alloc(Bump) {
        let items = [10, 20, 30];
        result = items[1];
    }
    result
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(20));
}

#[test]
fn alloc_arena_with_record() {
    let src = r#"
type Point {
    x: i32
    y: i32
}

func main() -> i32 {
    let mut result = 0;
    alloc(Arena) {
        let p = Point { x: 5, y: 10 };
        result = p.x + p.y;
    }
    result
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(15));
}

#[test]
fn alloc_arena_with_enum() {
    let src = r#"
type Color { Red | Green | Blue }

func main() -> string {
    let mut result = "none";
    alloc(Arena) {
        let c = Green();
        result = match c {
            Red() => "red",
            Green() => "green",
            Blue() => "blue",
        }
    }
    result
}
"#;
    assert_eq!(
        run_source_treewalker(src),
        interp::Value::String("green".to_string())
    );
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
        run_source_treewalker(src),
        interp::Value::String("system".to_string())
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
        run_source_treewalker(src),
        interp::Value::String("arena".to_string())
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
        run_source_treewalker(src),
        interp::Value::String("bump".to_string())
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
    assert_eq!(run_source_treewalker(src), interp::Value::Int(42));
}

#[test]
fn builtin_bump_used() {
    let src = r#"
func main() -> i32 {
    bump_used()
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(0));
}

#[test]
fn alloc_bump_with_function_call() {
    let src = r#"
func add_one(x: i32) -> i32 {
    x + 1
}

func main() -> i32 {
    let mut result = 0;
    alloc(Bump) {
        result = add_one(41);
    }
    result
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(42));
}

#[test]
fn alloc_bump_with_loop() {
    let src = r#"
func main() -> i32 {
    let mut sum = 0;
    alloc(Bump) {
        for i in [1, 2, 3, 4, 5] {
            sum += i;
        }
    }
    sum
}
"#;
    assert_eq!(run_source_treewalker(src), interp::Value::Int(15));
}
