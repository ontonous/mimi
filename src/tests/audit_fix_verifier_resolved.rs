//! Wave-2 audit-fix regression tests — verifier Resolved engine
//! (resolved_expr.rs / ctx.rs / expr.rs / flow.rs; VIR engine lives in audit_fix_verifier.rs).
//! Findings: devdocs/full-audit-2026-08-05-0656.md (C-7 resolved half, H-21/H-23/H-24, V-2..V-7).
//! Discipline: false-Proven regressions must assert the verifier verdict flips
//! (Proven -> Disproven/Failed/Unknown), never just "it ran".
use super::*;

/// Registration smoke: a trivially correct contract still checks green through
/// the compile path, guarding module wiring for Wave-2 additions.
#[test]
fn audit2_verifier_resolved_registration_smoke() {
    let src = r#"
func id(x: i32) -> i32 {
    ensures: result == x
    x
}
func main() -> i32 {
    id(7) - 7
}
"#;
    let out = compile_and_run(src);
    assert!(out.is_ok(), "compile_and_run failed: {:?}", out.err());
    assert_eq!(out.unwrap().trim(), "0");
}
