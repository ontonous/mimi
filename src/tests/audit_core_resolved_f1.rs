//! 0.1.8 audit-fix regression — core_resolved F1 (`from_flow_acc` suffix
//! fallback misbinding).
//!
//! Finding: `devdocs/audit0820/core_resolved.md` F1 — the old code resolved a
//! resolved function's checker-finalized signature with
//! `find_map` over progressively shorter suffixes (`m::A::run` → `A::run` →
//! `run`) guarded *only* by a parameter-count check. A same-suffix,
//! same-arity, different-typed signature could therefore be silently bound
//! (the wrong function's signature flows into the resolved IR and downstream
//! codegen/interp) — a soundness hole. The rework makes the fallback
//! defensive: exact / node-id hits stay authoritative; a suffix hit is taken
//! only when it is unambiguous (a single candidate) and type-consistent,
//! otherwise resolution fails closed (TOOL-RESOLUTION-001) instead of
//! last-wins misbinding.
//!
//! The misbind is latent in well-formed programs (the checker and resolved
//! catalogs now agree on module-qualified keys, so the exact match always
//! wins), so the PoC drives the resolution helper directly to prove the
//! bug class is closed, and a dual-backend test locks the real contract
//! (an actor method must bind to its own transition signature, never a
//! same-named top-level function).

use super::*;
use crate::ast::Type;
use crate::core::phase::ZonkedTy;
use crate::core::resolved::{resolve_zonked_signature, ZonkedResolution};
use crate::core::NodeId;
use std::collections::HashMap;

fn zt(t: Type) -> ZonkedTy {
    ZonkedTy::from_resolved(t).expect("resolved type must zonk cleanly")
}

/// Build a bare named type (`Name(String, [])`) for tests.
fn tn(name: &str) -> Type {
    Type::Name(name.to_string(), vec![])
}

/// PoC (core_resolved F1): the OLD `find_map` over suffixes would silently
/// pick the FIRST present suffix key when two distinct same-suffix functions
/// exist for one qualified name (e.g. `y::run` and `run`, both candidates for
/// `x::y::run`). That is exactly the "same-suffix, same-arity, different-typed"
/// misbind the audit flags. The reworked resolver fails closed (Err) instead
/// of last-wins binding the wrong signature.
#[test]
fn f1_ambiguous_suffix_fails_closed() {
    let mut zonked: HashMap<String, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();
    // "y::run" belongs to one function (ret i32), "run" to a different one
    // (ret string). Both are suffix candidates for qualified name "x::y::run".
    zonked.insert(
        "y::run".to_string(),
        (vec![zt(tn("i32"))], zt(tn("i32"))),
    );
    zonked.insert(
        "run".to_string(),
        (
            vec![zt(tn("string"))],
            zt(tn("string")),
        ),
    );
    let nested: HashMap<NodeId, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();

    let res = resolve_zonked_signature("x::y::run", &NodeId("n".into()), &zonked, &nested);
    assert!(
        res.is_err(),
        "ambiguous suffix must fail closed, not last-wins misbind (old find_map would bind here)"
    );
    let msg = res.unwrap_err();
    assert!(
        msg.contains("ambiguous"),
        "error must call out ambiguity: {}",
        msg
    );
}

/// Exact qualified-name hit is authoritative and applied silently — the common,
/// correct path that must keep working.
#[test]
fn f1_exact_hit_is_authoritative() {
    let mut zonked: HashMap<String, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();
    zonked.insert(
        "x::y::run".to_string(),
        (
            vec![zt(tn("i32"))],
            zt(tn("string")),
        ),
    );
    let nested: HashMap<NodeId, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();
    let res = resolve_zonked_signature("x::y::run", &NodeId("n".into()), &zonked, &nested)
        .expect("exact hit resolves");
    match res {
        ZonkedResolution::Exact(_) => {}
        other => panic!("expected exact hit, got {:?}", other),
    }
}

/// A single unambiguous suffix candidate is still accepted (legacy
/// compatibility shim for bare-key type constructors).
#[test]
fn f1_single_suffix_accepted() {
    let mut zonked: HashMap<String, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();
    zonked.insert(
        "run".to_string(),
        (vec![], zt(tn("i32"))),
    );
    let nested: HashMap<NodeId, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();
    let res = resolve_zonked_signature("x::y::run", &NodeId("n".into()), &zonked, &nested)
        .expect("single suffix resolves");
    match res {
        ZonkedResolution::Suffix(_) => {}
        other => panic!("expected suffix hit, got {:?}", other),
    }
}

/// Nested callables resolve via node-id (authoritative), independent of names.
#[test]
fn f1_nested_node_id_hit() {
    let zonked: HashMap<String, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();
    let mut nested: HashMap<NodeId, (Vec<ZonkedTy>, ZonkedTy)> = HashMap::new();
    nested.insert(
        NodeId("n".into()),
        (vec![], zt(tn("i32"))),
    );
    let res = resolve_zonked_signature("whatever", &NodeId("n".into()), &zonked, &nested)
        .expect("node-id hit resolves");
    match res {
        ZonkedResolution::Exact(_) => {}
        other => panic!("expected node-id (exact) hit, got {:?}", other),
    }
}

/// Dual-backend lock: an actor method `A::run` (a flow transition) must bind to
/// its OWN transition signature, never to a same-named top-level `func run()`.
///
/// The F1 fix guarantees `from_flow_acc` overrides a resolved function's
/// signature only with the *authoritative* (exact / node-id) zonked signature,
/// so the actor method keeps its transition return type (`T`) instead of being
/// misbound to the top-level `run() -> i32`. We assert this directly on the
/// resolved IR (backend-independent) and also confirm the program runs
/// correctly on the bytecode VM.
///
/// NOTE: a full codegen dual run is intentionally omitted here — actor
/// `runs Flow` transition dispatch is a separate, pre-existing codegen gap
/// ("method 'run' not compiled for type 'A'"), unrelated to F1. The VM run and
/// the resolved-IR assertion below lock the F1 binding contract.
#[test]
fn core_resolved_f1_actor_method_vs_same_named_top_level_dual() {
    let src = r#"
    func run() -> i32 { 99 }

    flow Counter {
        state S { v: i32 }
        state T { v: i32 }
        transition run(S, x: i32) -> T { { return T { v: x + 1 } } }
        transition step(T) -> S { { return S { v: self.v } } }
    }

    actor A runs Counter {
    }

    func main() -> i32 {
        let a = A.spawn();
        let s1 = a.run(10);
        let s2 = a.step();
        let t = run();
        let r = t + s2.v;
        println(to_string(r));
        r
    }
    "#;
    check_source(src).expect("source should check cleanly");

    // Backend-independent: the resolved function catalog must reflect the
    // CORRECT signatures, proving no silent misbind.
    let file = parse(src);
    let program = core::check_program(&file).expect("check_program must succeed");
    let actor_run = program
        .functions()
        .values()
        .find(|f| f.qualified_name == "A::run")
        .expect("synthetic transition method A::run should exist");
    let top_run = program
        .functions()
        .values()
        .find(|f| f.qualified_name == "run")
        .expect("top-level run should exist");
    assert_eq!(
        crate::core::fmt_type(&actor_run.ret),
        "T",
        "actor method A::run must bind to its transition return type T, not the top-level run() -> i32"
    );
    assert_eq!(
        crate::core::fmt_type(&top_run.ret),
        "i32",
        "top-level run() must keep its own return type"
    );

    // VM runtime sanity: the program computes 99 + 11 = 110.
    let (_vm_val, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "110", "vm runtime output mismatch");
}
