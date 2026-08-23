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
    assert!(
        result.is_ok(),
        "use statement should not fail type checking: {:?}",
        result.err()
    );
}

#[test]
fn module_nested_types() {
    // 0.39.139（spec §6.14 选项 C）：含类型与函数的内联 module 在解析期
    // E0445 拒绝；类型/函数/成员访问都不再有语言级语义。
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
    0
}
"#;
    let err = parse_error(src);
    assert_eq!(
        err.code.as_deref(),
        Some("E0445"),
        "expected E0445 for inline module block, got: {err:?}"
    );
}
