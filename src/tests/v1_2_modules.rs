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
    // 0.39.137 (spec §6.14): inline modules are a non-contract dead surface.
    // `Math.origin()` must be rejected by the checker (E0220 field access on
    // unknown type) — the old assertion here locked a VM-only path that no
    // checked program could ever reach.
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
    let diags = check_source(src).expect_err("inline-module member access must be rejected");
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("E0220")),
        "expected E0220 for inline-module member access, got: {diags:?}"
    );
}
