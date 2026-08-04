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
    add(20, 22) - 42
}
"#;
    let out = compile_and_run(src);
    assert!(out.is_ok(), "compile_and_run failed: {:?}", out.err());
    assert_eq!(out.unwrap().trim(), "0");
}
