use super::*;

#[test]
fn module_use_basic() {
    let src = r#"
use std::collections;

func main() -> i32 {
    42
}
"#;
    let result = check_source(src);
    // use of non-existent module is accepted by parser; main() returns 42
    assert!(result.is_ok(), "use statement should not fail type checking: {:?}", result.err());
}

#[test]
fn module_nested_types() {
    let src = r#"
module Math {
    type Point {
        x: i32,
        y: i32
    }

    func origin() -> Point {
        Point { x: 0, y: 0 }
    }
}

func main() -> i32 {
    let p = Math.origin();
    p.x
}
"#;
    let result = run_source_result(src);
    assert!(result.is_ok(), "module with type and method should work: {:?}", result.err());
}
