//! Wave-2 audit-fix regression tests — verifier Resolved engine
//! (resolved_expr.rs / ctx.rs / expr.rs / flow.rs; VIR engine lives in audit_fix_verifier.rs).
//! Findings: devdocs/full-audit-2026-08-05-0656.md (C-7 resolved half, H-21/H-23/H-24, V-1..V-7).
//! Discipline: false-Proven regressions must assert the verifier verdict flips
//! (Proven -> Disproven/Failed/Unknown), never just "it ran".
//!
//! Routing under test:
//! - `Verifier::verify_checked` (ctx.rs) = the RESOLVED engine — the engine
//!   behind the LSP (lsp/state.rs:619) and `mimi verify --dump-z3`.
//! - `verify_source` (mod.rs) = the Flow/AST/VIR engine behind `mimi verify`.
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
    println(id(7) - 7)
    0
}
"#;
    let out = compile_and_run(src);
    assert!(out.is_ok(), "compile_and_run failed: {:?}", out.err());
    assert_eq!(out.unwrap().trim(), "0");
}

fn z3_or_skip() -> bool {
    if crate::verifier::is_z3_available() {
        true
    } else {
        eprintln!("    └─ skipped (Z3 not available)");
        false
    }
}

fn parse_and_check(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("audit_fix_verifier_resolved: lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("audit_fix_verifier_resolved: parse");
    crate::core::check_program(&file).expect("audit_fix_verifier_resolved: check")
}

/// Run the RESOLVED engine (LSP / --dump-z3 route).
fn resolved_engine_results(source: &str) -> Vec<crate::verifier::VerificationResult> {
    let program = parse_and_check(source);
    let mut verifier =
        crate::verifier::Verifier::new().expect("audit_fix_verifier_resolved: Verifier::new");
    verifier.verify_checked(&program)
}

fn status_of(
    results: &[crate::verifier::VerificationResult],
    name: &str,
) -> Option<crate::verifier::VerifStatus> {
    results
        .iter()
        .find(|r| r.func_name == name || r.func_name.ends_with(&format!("::{name}")))
        .map(|r| r.status.clone())
}

// =========================================================================
// C-7 (CRITICAL, Resolved half): shadowed locals keyed by bare display name
// aliased back onto the shadowed parameter → fake Proven. Fixed by keying
// locals by ResolvedLocalId.
// =========================================================================

#[test]
fn audit2_verb_c7_shadowed_local_not_proven() {
    if !z3_or_skip() {
        return;
    }
    // Runtime-false contract: with c == true the body returns x - 1, not x.
    let source = r#"
func shadow(c: bool, x: i32) -> i32 {
    requires: x >= 0
    ensures: result == x
    if c { let x = x - 1; x } else { x }
}
func main() -> i32 { 0 }
"#;
    // Semantic grounding: the contract really is false at runtime.
    // The fixture must PRINT the value — `main() -> i32 { shadow(true, 5) }`
    // exits 4 with empty stdout, which compile_and_run reports as
    // Err("exit code Some(4)") before the verdict is ever checked.
    let runtime = compile_and_run(
        r#"
func shadow(c: bool, x: i32) -> i32 {
    if c { let x = x - 1; x } else { x }
}
func main() -> i32 { println(shadow(true, 5)); 0 }
"#,
    );
    assert_eq!(
        runtime.unwrap().trim(),
        "4",
        "shadow(true, 5) must be 4, not 5"
    );

    // Before the fix: Proven (the shadowed `let x` aliased onto the
    // parameter variable; the block statement was dropped entirely).
    let results = resolved_engine_results(source);
    let status = status_of(&results, "shadow");
    assert_eq!(
        status,
        Some(crate::verifier::VerifStatus::Disproven),
        "shadowed-local contract must be Disproven, got {:?}",
        status
    );
}

#[test]
fn audit2_verb_c7_let_binding_still_provable() {
    if !z3_or_skip() {
        return;
    }
    // Positive control: id-keyed let encodings must not over-reject. The
    // bound `x <= 100` discharges the i32 overflow obligation of `x + 1`.
    let source = r#"
func plus_one(x: i32) -> i32 {
    requires: x >= 0
    requires: x <= 100
    ensures: result == x + 1
    let y = x + 1
    y
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(source);
    let status = status_of(&results, "plus_one");
    assert_eq!(
        status,
        Some(crate::verifier::VerifStatus::Proven),
        "let-bound result with bounded requires must stay Proven, got {:?}",
        status
    );
}

// =========================================================================
// H-21 (HIGH): the Resolved engine encoded f64 as EXACT Z3 Reals (no IEEE
// rounding, no NaN) — float associativity verified fake Proven. Fixed:
// f64 contracts fail closed with NotInTrustedSubset.
// =========================================================================

#[test]
fn audit2_verb_h21_f64_reassociation_fail_closed() {
    if !z3_or_skip() {
        return;
    }
    // Runtime-false: with x = 1e30, y = -1e30, z = 1.0 the two associations
    // differ (1.0 vs 1e30). Exact Real arithmetic says they are equal.
    let source = r#"
func assoc(x: f64, y: f64, z: f64) -> f64 {
    ensures: (x + y) + z == x + (y + z)
    (x + y) + z
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(source);
    let result = results
        .iter()
        .find(|r| r.func_name == "assoc")
        .expect("assoc result");
    assert_eq!(
        result.status,
        crate::verifier::VerifStatus::NotInTrustedSubset,
        "f64 contract must fail closed, got {:?}",
        result.status
    );
    assert!(
        result.message.contains("IEEE"),
        "diagnostic must explain WHY f64 is rejected: {}",
        result.message
    );
}

#[test]
fn audit2_verb_h21_f64_in_requires_fail_closed() {
    if !z3_or_skip() {
        return;
    }
    // f64 involvement in a CONTRACT rejects the callable. Detection is exact
    // (typed walk): a merely-declared, never-used f64 parameter does NOT
    // trigger rejection — only expressions actually involved do.
    let source = r#"
func mixed(x: f64, n: i32) -> i32 {
    requires: x >= 0.0
    requires: n >= 0
    ensures: result == n
    n
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(source);
    assert_eq!(
        status_of(&results, "mixed"),
        Some(crate::verifier::VerifStatus::NotInTrustedSubset),
        "f64 comparison inside requires must fail closed"
    );
}

// =========================================================================
// H-23 (HIGH, AST path): is_f64_expr recognized only literal/Ident/Old
// NON-recursively, so composite f64 expressions bypassed the P0-2 rejection
// guard and were encoded as exact Reals (invariant statements force the AST
// path). Fixed: recursive recognition mirroring is_real_expr.
// =========================================================================

#[test]
fn audit2_verb_h23_composite_f64_bypass_closed() {
    if !z3_or_skip() {
        return;
    }
    // Before the fix: both sides of the comparison are Match-wrapped f64
    // loads; is_f64_expr(Match) == false, the guard never fired, and the
    // comparison was encoded as `x == x` over exact Reals → Proven (false
    // for NaN). Now the guard fires on composite f64 → fail closed.
    let source = r#"
func reflect(x: f64) -> f64 {
    invariant: true
    ensures: (match 1 { _ => x }) == (match 2 { _ => x })
    x
}
func main() -> i32 { 0 }
"#;
    let results =
        crate::verifier::verify_source(source).expect("audit_fix_verifier_resolved: verify");
    let status = status_of(&results, "reflect");
    assert_eq!(
        status,
        Some(crate::verifier::VerifStatus::NotInTrustedSubset),
        "composite f64 comparison must fail closed on the AST path, got {:?}",
        status
    );
}

// =========================================================================
// H-24 (HIGH): the Resolved engine emitted NO i32 definedness VCs —
// `ensures: result > x; x + 1` verified Proven while the runtime traps on
// overflow (SD-7/SD-8: Trap ≠ Fault). Fixed: definedness VCs mirroring the
// AST engine's collect_i32_definedness machinery.
// =========================================================================

#[test]
fn audit2_verb_h24_overflow_disproven() {
    if !z3_or_skip() {
        return;
    }
    let source = r#"
func inc(x: i32) -> i32 {
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(source);
    let result = results
        .iter()
        .find(|r| r.func_name == "inc")
        .expect("inc result");
    assert_eq!(
        result.status,
        crate::verifier::VerifStatus::Disproven,
        "overflowable body must not be Proven, got {:?}",
        result.status
    );
    assert!(
        result.message.contains("overflow"),
        "verdict message must name the definedness failure: {}",
        result.message
    );
}

#[test]
fn audit2_verb_h24_overflow_bounded_still_proven() {
    if !z3_or_skip() {
        return;
    }
    let source = r#"
func inc(x: i32) -> i32 {
    requires: x <= 2147483646
    ensures: result > x
    x + 1
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(source);
    assert_eq!(
        status_of(&results, "inc"),
        Some(crate::verifier::VerifStatus::Proven),
        "overflow discharged by requires must stay Proven"
    );
}

#[test]
fn audit2_verb_h24_div_zero_disproven() {
    if !z3_or_skip() {
        return;
    }
    let source = r#"
func self_div(x: i32) -> i32 {
    ensures: result * x == x
    x / x
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(source);
    let result = results
        .iter()
        .find(|r| r.func_name == "self_div")
        .expect("self_div result");
    assert_eq!(
        result.status,
        crate::verifier::VerifStatus::Disproven,
        "x / x without x != 0 must be Disproven (E0801 trap), got {:?}",
        result.status
    );
    assert!(
        result.message.contains("undefined"),
        "verdict message must name the definedness failure: {}",
        result.message
    );

    // Positive control: with the guard in place the same body verifies.
    let guarded = r#"
func self_div(x: i32) -> i32 {
    requires: x != 0
    ensures: result * x == x
    x / x
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(guarded);
    assert_eq!(
        status_of(&results, "self_div"),
        Some(crate::verifier::VerifStatus::Proven),
        "guarded division must stay Proven"
    );
}

// =========================================================================
// V-1 (MED, AST path): collect_i32_definedness had NO Match and NO Call arm
// — divisions inside arm bodies / call arguments generated no obligation.
// Fixed: both arms added (guard/pattern-gated for Match).
// =========================================================================

#[test]
fn audit2_verb_v1_match_arm_division_not_proven() {
    if !z3_or_skip() {
        return;
    }
    // Runtime: y == 1 selects the wildcard arm → 1 / 0 → E0801 trap.
    // Before the fix: Proven, because `match` generated no obligation and
    // the ensures holds under Z3's uninterpreted div-by-zero.
    let source = r#"
func f(y: i32) -> i32 {
    requires: y > 0
    invariant: true
    ensures: result * (y - 1) >= 0
    match y {
        2 => 0
        _ => 1 / (y - 1)
    }
}
func main() -> i32 { 0 }
"#;
    // Semantic grounding: the real program traps.
    let runtime = compile_and_run(
        r#"
func main() -> i32 {
    let y = 1
    match y {
        2 => 0
        _ => 1 / (y - 1)
    }
}
"#,
    );
    assert!(
        runtime.is_err() && runtime.unwrap_err().contains("E0801"),
        "division by zero must trap with E0801"
    );

    let results =
        crate::verifier::verify_source(source).expect("audit_fix_verifier_resolved: verify");
    let result = results
        .iter()
        .find(|r| r.func_name == "f")
        .expect("f result");
    assert_eq!(
        result.status,
        crate::verifier::VerifStatus::Disproven,
        "match-arm division by zero must be Disproven, got {:?}",
        result.status
    );

    // Positive control: y > 1 keeps every arm's division defined.
    let guarded = source.replace("requires: y > 0", "requires: y > 1");
    let results =
        crate::verifier::verify_source(&guarded).expect("audit_fix_verifier_resolved: verify");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::Proven),
        "guarded match arms must stay Proven"
    );
}

#[test]
fn audit2_verb_v1_call_argument_division_not_proven() {
    if !z3_or_skip() {
        return;
    }
    // Runtime: y == 1 evaluates the call argument → 1 / 0 → E0801 trap.
    let source = r#"
func id(x: i32) -> i32 {
    ensures: result == x
    x
}
func g(y: i32) -> i32 {
    requires: y > 0
    invariant: true
    ensures: result * (y - 1) >= 0
    id(y / (y - 1))
}
func main() -> i32 { 0 }
"#;
    let results =
        crate::verifier::verify_source(source).expect("audit_fix_verifier_resolved: verify");
    let result = results
        .iter()
        .find(|r| r.func_name == "g")
        .expect("g result");
    assert_eq!(
        result.status,
        crate::verifier::VerifStatus::Disproven,
        "call-argument division by zero must be Disproven, got {:?}",
        result.status
    );
}

// =========================================================================
// V-3 (MED): func_defs/func_status stored by BARE name — cross-module
// same-named functions polluted each other. Fixed: qualified-name keys.
// =========================================================================

#[test]
#[ignore = "inline `module` rejected at check since 0.39.138 (E0445, spec §6.14); the module machinery under test retires with pre-1.0 option-C syntax removal"]
fn audit2_verb_v3_module_same_name_isolation() {
    if !z3_or_skip() {
        return;
    }
    // Top-level `get` returns 0. `A::get` returns x + 1 and is verified.
    // Before the fix: func_defs["get"] was overwritten by A::get, so the
    // caller's `get(x)` picked up `result == x + 1` axioms → fake Proven.
    // After the fix the caller sees the top-level definition and its
    // contract (`result == x + 1`) is correctly refuted.
    let source = r#"
func get(x: i32) -> i32 {
    ensures: result == 0
    0
}
module A {
    pub func get(x: i32) -> i32 {
        requires: x >= 0
        requires: x <= 100
        ensures: result == x + 1
        x + 1
    }
}
func caller(x: i32) -> i32 {
    requires: x >= 0
    requires: x <= 100
    ensures: result == x + 1
    get(x)
}
func main() -> i32 { 0 }
"#;
    let results =
        crate::verifier::verify_source(source).expect("audit_fix_verifier_resolved: verify");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result");
    assert_eq!(
        caller.status,
        crate::verifier::VerifStatus::Disproven,
        "caller must not inherit A::get's axioms (fake Proven), got {:?}",
        caller.status
    );
    // The module function now reports under its qualified identity.
    assert!(
        results.iter().any(|r| r.func_name == "A::get"),
        "module function must be queued under its qualified name; got {:?}",
        results
            .iter()
            .map(|r| r.func_name.clone())
            .collect::<Vec<_>>()
    );
}

// =========================================================================
// V-4 (MED): preseed + single pass made verdicts depend on SOURCE ORDER —
// chain C→B→A declared [C,B,A] permanently lost C's axioms (fake failure).
// Fixed: worklist waves to fixpoint.
// =========================================================================

#[test]
fn audit2_verb_v4_source_order_independence() {
    if !z3_or_skip() {
        return;
    }
    let caller_first = r#"
func c(x: i32) -> i32 { requires: x >= 0; ensures: result == x; b(x) }
func b(x: i32) -> i32 { requires: x >= 0; ensures: result == x; a(x) }
func a(x: i32) -> i32 { requires: x >= 0; ensures: result == x; x }
func main() -> i32 { 0 }
"#;
    let callee_first = r#"
func a(x: i32) -> i32 { requires: x >= 0; ensures: result == x; x }
func b(x: i32) -> i32 { requires: x >= 0; ensures: result == x; a(x) }
func c(x: i32) -> i32 { requires: x >= 0; ensures: result == x; b(x) }
func main() -> i32 { 0 }
"#;
    let status_map =
        |source: &str| -> std::collections::BTreeMap<String, crate::verifier::VerifStatus> {
            let results = crate::verifier::verify_source(source)
                .expect("audit_fix_verifier_resolved: verify");
            results
                .into_iter()
                .map(|r| (r.func_name.clone(), r.status))
                .collect()
        };
    let by_caller_first = status_map(caller_first);
    let by_callee_first = status_map(callee_first);
    assert_eq!(
        by_caller_first, by_callee_first,
        "verdicts must not depend on source order:\ncaller-first: {:?}\ncallee-first: {:?}",
        by_caller_first, by_callee_first
    );
    assert_eq!(
        by_caller_first.get("c"),
        Some(&crate::verifier::VerifStatus::Proven),
        "chained caller must be Proven in either declaration order"
    );
}

// =========================================================================
// V-7 (LOW): (a) check_scope_multi returned (Sat, None) on EMPTY
// constraints — semantically inverted (no violation witness = Unsat).
// (b) Resolved engine ProofArtifact was always None — the P1-24 tamper
// binding was disabled on the LSP/--dump-z3 path.
// =========================================================================

#[test]
fn audit2_verb_v7_check_scope_multi_empty_is_unsat() {
    if !z3_or_skip() {
        return;
    }
    let mut session = crate::verifier::SolverSession::new(1000).expect("solver session");
    let empty: Vec<z3::ast::Bool> = Vec::new();
    let (result, model) = session.check_scope_multi(empty);
    assert_eq!(
        result,
        z3::SatResult::Unsat,
        "no constraints ⇒ no satisfiable violation ⇒ Unsat (was inverted: Sat)"
    );
    assert!(model.is_none(), "Unsat has no model");
}

#[test]
fn audit2_verb_v7_resolved_engine_artifact_bound() {
    if !z3_or_skip() {
        return;
    }
    let source = r#"
func id(x: i32) -> i32 {
    ensures: result == x
    x
}
func main() -> i32 { 0 }
"#;
    let results = resolved_engine_results(source);
    let result = results
        .iter()
        .find(|r| r.func_name == "id")
        .expect("id result");
    assert_eq!(result.status, crate::verifier::VerifStatus::Proven);
    let artifact = result
        .artifact
        .as_ref()
        .expect("Resolved engine results must carry a ProofArtifact (was always None)");
    assert_eq!(
        artifact.semantics_version,
        crate::verifier::ProofArtifact::SEMANTICS_VERSION
    );
    assert_eq!(artifact.integer_model, "checked_i32");
    assert_eq!(artifact.float_model, "f64_rejected");
    assert!(
        artifact.solver_version.starts_with("z3 "),
        "solver identity must be bound: {}",
        artifact.solver_version
    );
    // Documented residual gap: vir_hash is empty on this engine (no VIR
    // identity available) — assert the current shape so any future wiring
    // shows up as an intentional change.
    assert_eq!(artifact.vir_hash, "");
}
