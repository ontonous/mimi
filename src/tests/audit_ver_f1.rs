//! VER-F1 (audit0820, CRITICAL) — regression guard for `old()` contract soundness.
//!
//! Audit claim: the verifier asserted `old(param) == param` globally and ignored
//! reassignments, so `ensures: x == old(x)` on a reassigned `mutate` param could
//! be falsely proven. Investigation (0.1.8 audit0820) found the verifier NOW
//! (a) symbolically models scalar assignments and (b) fails closed
//! (`NotInTrustedSubset`) on constructs it cannot model (record field access,
//! checked arithmetic, unary minus). The concrete false-`Proven` is therefore
//! NOT reproducible in the current tree — the hole appears already mitigated by a
//! later verifier upgrade.
//!
//! This test LOCKS IN that soundness: for every CHANGING reassignment paired with
//! an `old()`-referencing `ensures`, the verdict must never be `Proven`. A
//! regression that re-introduces "assignments are ignored" would be caught here.
//! The identity control (`x = x`) is the only case where `Proven` is correct
//! (the value genuinely does not change).

#[cfg(test)]
mod audit_ver_f1 {
    use super::*;

    fn verify(src: &str) -> Vec<crate::verifier::VerificationResult> {
        crate::verifier::verify_source(src).expect("verify_source")
    }

    fn status_of(src: &str, contains: &str) -> crate::verifier::VerifStatus {
        assert!(
            crate::verifier::is_z3_available(),
            "Z3 unavailable; VER-F1 regression requires the verifier engine"
        );
        verify(src)
            .iter()
            .find(|r| r.func_name.contains(contains))
            .unwrap_or_else(|| panic!("no verification result for '{}'", contains))
            .status
            .clone()
    }

    #[test]
    fn ver_f1_no_false_proof_on_changing_reassignment() {
        // scalar bitwise: x=3 -> x&1 = 1, so `x >= old(x)` is FALSE.
        // If the verifier ignored the assignment it would falsely prove.
        let ge = r#"
func rmw_ge(x: mutate i32) -> i32 {
    ensures: x >= old(x)
    x = x & 1
    x
}
"#;
        // reversed old() ordering
        let rev = r#"
func rmw_rev(x: mutate i32) -> i32 {
    ensures: old(x) == x
    x = x & 1
    x
}
"#;
        // record field swap; p.a becomes old(p).b, so p.a == old(p.a) is FALSE.
        let fld = r#"
type Rec { a: i32, b: i32 }
func rmw_fld(p: mutate Rec) -> Rec {
    ensures: p.a == old(p.a)
    p = Rec { a: p.b, b: p.a }
    p
}
"#;
        // checked arithmetic reassignment (gate should catch -> NotInTrustedSubset)
        let chk = r#"
func rmw_chk(x: mutate i32) -> i32 {
    ensures: x == old(x)
    x = x + 1
    x
}
"#;
        for (name, src) in [("rmw_ge", ge), ("rmw_rev", rev), ("rmw_fld", fld), ("rmw_chk", chk)] {
            let st = status_of(src, name);
            assert_ne!(
                st,
                crate::verifier::VerifStatus::Proven,
                "VER-F1 regression: changing reassignment '{}' must NOT be falsely \
                 proven against an old()-referencing contract (status={:?})",
                name,
                st
            );
        }
    }

    #[test]
    fn ver_f1_identity_reassignment_is_correctly_proven() {
        // control: `x = x` does not change x, so `ensures: x == old(x)` is TRUE
        // and Proven is the correct verdict.
        let id = r#"
func rmw_id(x: mutate i32) -> i32 {
    ensures: x == old(x)
    x = x
    x
}
"#;
        let st = status_of(id, "rmw_id");
        assert_eq!(
            st,
            crate::verifier::VerifStatus::Proven,
            "VER-F1 control: identity reassignment should be Provable (status={:?})",
            st
        );
    }
}
