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
fn module_internal_use_parses() {
    let src = r#"
module Math {
    use std::collections;

    func answer() -> i32 {
        42
    }
}

func main() -> i32 {
    Math.answer()
}
"#;
    let file = parse(src);
    let module = file
        .items
        .iter()
        .find_map(|i| {
            if let crate::ast::Item::Module(m) = i {
                Some(m)
            } else {
                None
            }
        })
        .expect("Math module should be present");
    assert_eq!(
        module.imports.len(),
        1,
        "module should have one internal use"
    );
    assert_eq!(module.imports[0].path, vec!["std", "collections"]);
}

#[test]
fn module_nested_types() {
    // 0.39.138（spec §6.14）：含类型与函数的内联 module 整体 E0445 拒绝；
    // 类型/函数/成员访问都不再有语言级语义。
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
    let diags = check_source(src).expect_err("inline module with types must be rejected");
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("E0445")),
        "expected E0445 for inline module block, got: {diags:?}"
    );
}
