//! Wave-2 audit-fix regression tests — resolved native emitter
//! (codegen/resolved/mod.rs exclusive territory; legacy-core fixes live in
//! audit_fix_codegen_expr2.rs / infra.rs).
//! Findings: devdocs/full-audit-2026-08-05-0656.md (C-4, H-15, H-16, K-4/K-5).
//! Discipline: emitter findings are silent-miscompile class — tests must force
//! the resolved path (eligible scalar/control-flow bodies) and assert observable
//! behavior through compile_and_run; do NOT rely on legacy fallback.
use super::*;

/// Registration smoke: an eligible scalar function (resolved path) compiles and
/// behaves; guards module wiring for Wave-2 additions.
#[test]
fn audit2_codegen_resolved_registration_smoke() {
    let src = r#"
func add(a: i32, b: i32) -> i32 {
    a + b
}
func main() -> i32 {
    println(add(20, 22) - 42)
    0
}
"#;
    let out = compile_and_run(src);
    assert!(out.is_ok(), "compile_and_run failed: {:?}", out.err());
    assert_eq!(out.unwrap().trim(), "0");
}

/// AUD-1 (2026-08-20 critical audit): `continue` inside a resolved `for` loop
/// branched straight to the loop header, skipping the counter increment, so the
/// counter froze and the loop ran forever (compile_and_run would hang). Now
/// `continue` routes through a latch block that increments first.
#[test]
fn audit_resolved_for_range_continue_terminates() {
    let src = r#"
func main() {
    for i in range(0, 3) {
        if i == 1 { continue; }
        println("x");
    }
}
"#;
    let out = compile_and_run(src);
    assert!(
        out.is_ok(),
        "compile_and_run failed (AUD-1 regression would hang): {:?}",
        out.err()
    );
    assert_eq!(out.unwrap().trim(), "x\nx");
}

/// Same AUD-1 class for the for-in-list path: `continue` must still advance the
/// index. 1+2+4+5 = 12 (3 is skipped by `continue`).
#[test]
fn audit_resolved_for_list_continue_skips_element() {
    let src = r#"
func main() {
    let items = [1, 2, 3, 4, 5];
    let mut sum = 0;
    for it in items {
        if it == 3 { continue; }
        sum = sum + it;
    }
    println(to_string(sum));
}
"#;
    let out = compile_and_run(src);
    assert!(out.is_ok(), "compile_and_run failed: {:?}", out.err());
    assert_eq!(out.unwrap().trim(), "12");
}
