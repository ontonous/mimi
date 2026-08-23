//! Wave-1 audit-fix regression tests — verifier.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! AU-V1 (VERIFIED CRITICAL): ctx.rs `verify_checked_contracts` ran every
//! callable on ONE shared SolverSession with no reset; base assertions
//! (requires, result == body, i32 bounds) leaked across callables that reuse
//! the same Z3 const names (`result`, parameter display names) → function B
//! proved under function A's assumptions (spurious Proven). Affects
//! `mimi verify --dump-z3` (main/verify.rs) and the LSP persistent Verifier
//! (lsp/state.rs:602-612).
//!
//! AU-V2 (HIGH): ffi.rs extern call-site discovery skipped If/While
//! conditions, For iterables, WhileLet/IfLet, Match, Alloc bodies, Defer
//! bodies — extern calls in those positions were never checked by
//! `--verify-ffi`. Fix: exhaustive recursive walker (no catch-all arm).
//!
//! Wave-2 (VER-A) — full-audit-2026-08-05-0656 §1/§2.7/§3.8:
//! C-5 (VIR Let definedness), C-6 (extract_body_return tail swallowing),
//! C-7 VIR half (scoped name_map / shadowed locals + the AST-path
//! self-referential let-substitution stack overflow), H-25 (callee-requires
//! walker missing arms), V-2 (old() parity for bool/string), V-6 (i64
//! div/mod/MIN÷-1 definedness), FFI walker module descent. Audit axiom:
//! Trap ≠ Fault — a body that can trap (E0801 etc.) must never be Proven.

/// Mirrors the `require_z3!` guard in src/verifier/tests.rs: verification
/// regressions are meaningless without a solver, so skip loudly instead of
/// failing on Z3-less machines.
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
        .expect("audit_fix_verifier: lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("audit_fix_verifier: parse");
    crate::core::check_program(&file).expect("audit_fix_verifier: check")
}

/// Find a result by bare or qualified function name.
fn status_of(
    results: &[crate::verifier::VerificationResult],
    name: &str,
) -> Option<crate::verifier::VerifStatus> {
    let qualified = format!("::{name}");
    results
        .iter()
        .find(|r| r.func_name == name || r.func_name.ends_with(&qualified))
        .map(|r| r.status.clone())
}

/// AU-V1 regression: two callables, ONE shared session, one
/// `Verifier::verify_checked` call. Before the fix, `a` asserted
/// `result == 7` into the shared solver; `b` reuses the same Z3 const name
/// `result`, so `b` was spuriously Proven under `a`'s leaked assumption.
#[test]
fn audit_fix_ctx_no_solver_leak_between_callables() {
    if !z3_or_skip() {
        return;
    }
    let source = r#"
func a() -> i32 {
    requires: true
    ensures: result == 7
    7
}
func b(y: i32) -> i32 {
    ensures: result == 7
    y
}
func main() -> i32 { 0 }
"#;
    let program = parse_and_check(source);
    let mut verifier = crate::verifier::Verifier::new().expect("z3 verifier");
    let results = verifier.verify_checked(&program);

    assert_eq!(
        status_of(&results, "a"),
        Some(crate::verifier::VerifStatus::Proven),
        "a() legitimately proves result == 7: {:?}",
        results
    );
    assert_eq!(
        status_of(&results, "b"),
        Some(crate::verifier::VerifStatus::Disproven),
        "b() body is the unconstrained parameter y — ensures result == 7 \
         must be Disproven. Proven means `a`'s `result == 7` assertion \
         leaked into `b`'s solver scope: {:?}",
        results
    );
}

/// AU-V1 regression (requires leakage): parameters collide by Z3 const name
/// across callables. Before the fix, `guard_a`'s `requires: n > 100` leaked
/// into `guard_b` (same param name `n`) and spuriously proved
/// `ensures: result > 100` for body `n`.
#[test]
fn audit_fix_ctx_no_requires_leak_across_callables() {
    if !z3_or_skip() {
        return;
    }
    let source = r#"
func guard_a(n: i32) -> i32 {
    requires: n > 100
    ensures: result == n
    n
}
func guard_b(n: i32) -> i32 {
    ensures: result > 100
    n
}
func main() -> i32 { 0 }
"#;
    let program = parse_and_check(source);
    let mut verifier = crate::verifier::Verifier::new().expect("z3 verifier");
    let results = verifier.verify_checked(&program);

    assert_eq!(
        status_of(&results, "guard_a"),
        Some(crate::verifier::VerifStatus::Proven),
        "guard_a is legitimate: {:?}",
        results
    );
    assert_eq!(
        status_of(&results, "guard_b"),
        Some(crate::verifier::VerifStatus::Disproven),
        "guard_b has no requires — n = 0 is a counterexample to \
         result > 100. Proven means guard_a's requires leaked: {:?}",
        results
    );
}

/// AU-V1 / LSP angle: lsp/state.rs keeps one Verifier across requests
/// (state.rs:602-612). The per-callable reset inside
/// `verify_checked_contracts` must also clear prior-request state, so a
/// second `verify_checked` on the SAME verifier instance yields identical
/// verdicts — no separate per-request reset hook is required.
#[test]
fn audit_fix_ctx_persistent_verifier_reuse_is_stable() {
    if !z3_or_skip() {
        return;
    }
    let source = r#"
func a() -> i32 {
    requires: true
    ensures: result == 7
    7
}
func b(y: i32) -> i32 {
    ensures: result == 7
    y
}
func main() -> i32 { 0 }
"#;
    let program = parse_and_check(source);
    let mut verifier = crate::verifier::Verifier::new().expect("z3 verifier");

    let first = verifier.verify_checked(&program);
    let second = verifier.verify_checked(&program);

    let summarize = |results: &[crate::verifier::VerificationResult]| {
        let mut v: Vec<(String, crate::verifier::VerifStatus)> = results
            .iter()
            .map(|r| (r.func_name.clone(), r.status.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    assert_eq!(
        summarize(&first),
        summarize(&second),
        "re-running verification on the same persistent Verifier must not \
         accumulate solver state (LSP reuse scenario)"
    );
    assert_eq!(
        status_of(&second, "b"),
        Some(crate::verifier::VerifStatus::Disproven),
        "b must stay Disproven on the second request: {:?}",
        second
    );
}

/// AU-V2 regression: extern call inside an if-condition. Before the walker
/// fix the call was never discovered → zero results → silent pass.
///
/// Wave-2 fixture fix (wave1-review §2-B): the original fixture used
/// `return 1;` / tail `0` in an `-> i64` function — bare literals are i32,
/// so `check_program` rejected the fixture with "return type mismatch:
/// expected i64, found i32" and `verify_ffi_source` returned Err before the
/// walker ever ran. The walker fix was thus never actually tested.
#[test]
fn audit_fix_ffi_extern_call_in_if_condition_discovered() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
extern "C" {
    func danger(p: i64) -> i64
        requires: p > 0;
}
func caller(x: i64) -> i64 {
    if danger(x) > 0 {
        return x;
    }
    x
}
"#;
    let results = crate::verifier::verify_ffi_source(src)
        .expect("fixture must type-check (wave1-review §2-B fix)");
    assert!(
        results.iter().any(|r| r.func_name.contains("calls danger")),
        "extern call inside an if-condition must be discovered: {:?}",
        results
    );
    // No caller-side guard → precondition may be violated (fail closed).
    assert!(
        results.iter().any(|r| r.func_name.contains("calls danger")
            && r.status == crate::verifier::VerifStatus::Failed),
        "unguarded danger(x) in condition should be Disproven: {:?}",
        results
    );
}

/// AU-V2 regression: extern call inside a while-condition — the headline
/// hole (`while dangerous(ptr) { ... }`).
///
/// Wave-2 fixture fix (wave1-review §2-B): same i64/i32 literal mismatch as
/// the if-condition test above; the fixture now type-checks so the walker is
/// genuinely exercised.
#[test]
fn audit_fix_ffi_extern_call_in_while_condition_discovered() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
extern "C" {
    func step(s: i64) -> i64
        requires: s >= 0;
}
func poller(s: i64) -> i64 {
    while step(s) > 0 {
        return s;
    }
    s
}
"#;
    let results = crate::verifier::verify_ffi_source(src)
        .expect("fixture must type-check (wave1-review §2-B fix)");
    assert!(
        results.iter().any(|r| r.func_name.contains("calls step")),
        "extern call inside a while-condition must be discovered: {:?}",
        results
    );
    assert!(
        results.iter().any(|r| r.func_name.contains("calls step")
            && r.status == crate::verifier::VerifStatus::Failed),
        "unguarded step(s) in while-condition should be Disproven: {:?}",
        results
    );
}

/// AU-V2 regression: extern call inside a defer body.
#[test]
fn audit_fix_ffi_extern_call_in_defer_body_discovered() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
extern "C" {
    func release(h: i64) -> i64
        requires: h >= 0;
}
func cleaner(h: i64) -> i64 {
    defer {
        release(h);
    }
    h
}
"#;
    let results = crate::verifier::verify_ffi_source(src).expect("verify_ffi_source");
    assert!(
        results
            .iter()
            .any(|r| r.func_name.contains("calls release")),
        "extern call inside a defer body must be discovered: {:?}",
        results
    );
    assert!(
        results.iter().any(|r| r.func_name.contains("calls release")
            && r.status == crate::verifier::VerifStatus::Failed),
        "unguarded release(h) in defer body should be Disproven: {:?}",
        results
    );
}

// ── Wave-2 VER-A: VIR engine false-Proven cluster ────────────────────────
// full-audit-2026-08-05-0656. Every test asserts the VERDICT. Arithmetic
// axiom: Trap ≠ Fault — any body that can runtime-trap (E0801 div-zero,
// E08xx overflow/MIN÷-1, callee requires violation) must NOT be Proven.

/// C-5 (CRITICAL): VIR-path `let y = x / z` skipped definedness — the z≠0
/// obligation was collected ONLY in the Return arm, and `Return(Var(y))`
/// carries no CheckedArith. With z possibly 0 the body traps E0801 at
/// runtime yet verified Proven. Fix: VStmt::Let checks definedness too.
#[test]
fn audit2_vera_c5_vir_let_div_zero_disproven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(x: i32, z: i32) -> i32 {
    ensures: result == result
    let y = x / z
    y
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::Disproven),
        "let-bound division with an unconstrained divisor must Disprove \
         (E0801 trap at z == 0): {:?}",
        results
    );
}

/// C-5 control: with a strict-positive divisor the same body is fully
/// defined and the postcondition must still prove (no over-correction).
#[test]
fn audit2_vera_c5_vir_let_div_guarded_proven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(x: i32, z: i32) -> i32 {
    requires: z > 0
    ensures: result == result
    let y = x / z
    y
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::Proven),
        "z > 0 excludes zero-divisor and MIN/-1, so the let division is \
         defined and the trivial postcondition proves: {:?}",
        results
    );
}

/// C-6 (CRITICAL): `extract_body_return` met the first `Stmt::If`, and when
/// no return-expression was extractable from its branches the `None`
/// propagated as the OVERALL result — subsequent tail statements were never
/// examined and func.rs bound result to 0. `ensures: result == 0` became a
/// fake Proven although the runtime always returns the tail `y`.
#[test]
fn audit2_vera_c6_tail_not_swallowed_by_valueless_if() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(c: bool, y: i32) -> i32 {
    ensures: result == 0
    if c { let y2 = y }
    y
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::Disproven),
        "the tail `y` must survive the value-less if; y = 1 violates \
         result == 0. Proven means extract_body_return swallowed the tail: {:?}",
        results
    );
}

/// C-6 control: once extraction reaches the tail, `result == y` proves.
#[test]
fn audit2_vera_c6_tail_survives_and_proves() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(c: bool, y: i32) -> i32 {
    ensures: result == y
    if c { let y2 = y }
    y
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::Proven),
        "runtime returns y on every path; ensures result == y must prove: {:?}",
        results
    );
}

/// C-7 (CRITICAL, VIR half): block-level shadowing aliased the shadow to the
/// PARAMETER's Z3 variable through the flat name map.
/// `if c { let x = x + 1; x } else { x }` returns x+1 when c holds, yet
/// `ensures: result == x` verified Proven. Fix: scoped name map — the shadow
/// gets a fresh VarId and branch lets are lowered (substituted) instead of
/// discarded by block_tail_expr.
#[test]
fn audit2_vera_c7_shadowed_let_is_not_the_parameter() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(c: bool, x: i32) -> i32 {
    ensures: result == x
    if c { let x = x + 1
        x } else { x }
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::Disproven),
        "c = true returns x + 1, violating result == x. Proven means the \
         shadow `let x` was aliased to the parameter's Z3 variable: {:?}",
        results
    );
}

/// C-7 control: under `!c` only the else branch runs, so result == x holds.
#[test]
fn audit2_vera_c7_shadow_else_branch_proves() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(c: bool, x: i32) -> i32 {
    requires: !c
    ensures: result == x
    if c { let x = x + 1
        x } else { x }
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::Proven),
        "with !c the else branch (plain x) always runs: {:?}",
        results
    );
}

/// C-7 family (VERIFIED crash on the 90ac9bdc binary): the shadowing binding
/// `let x = x + 1` makes the AST-path let-substitution self-referential and
/// `expand_lets_in_expr` recursed until stack overflow — `mimi verify`
/// aborted on user source. This test pins the crash fix; any verdict other
/// than an explicit solver outcome would have aborted the process before.
#[test]
fn audit2_vera_c7_self_referential_let_no_stack_overflow() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func pos(v: i32) -> i32 {
    requires: v > 0
    ensures: result > 0
    v
}
func f(c: bool, x: i32) -> i32 {
    requires: x > 0
    ensures: result == pos(x)
    if c { let x = pos(x)
        x } else { pos(x) }
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    // The point is reaching a verdict at all (pre-fix: stack overflow abort).
    assert!(
        status_of(&results, "f").is_some(),
        "self-referential let substitution must terminate: {:?}",
        results
    );
}

/// H-25 (HIGH): the callee-requires walker lacked Assign/SharedLet/For/
/// WhileLet arms while callee ENSURES axioms were asserted at those
/// positions. `z = pos(y)` assumed pos(y) > 0 yet never discharged pos's
/// `requires: y > 0` — a guaranteed-violation trap proving Proven.
#[test]
fn audit2_vera_h25_assign_requires_checked() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func pos(v: i32) -> i32 {
    requires: v > 0
    ensures: result > 0
    v
}
func caller(y: i32) -> i32 {
    ensures: result == 42
    let mut z = 0
    z = pos(y)
    42
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    let caller = results
        .iter()
        .find(|r| r.func_name == "caller")
        .expect("caller result present");
    assert_eq!(
        caller.status,
        crate::verifier::VerifStatus::Disproven,
        "z = pos(y) must discharge pos's requires: y > 0. Proven means the \
         Assign arm is still missing from the requires walker: {:?}",
        results
    );
    assert!(
        caller.message.contains("may violate precondition"),
        "expected the callee-requires failure message, got: {}",
        caller.message
    );
}

/// H-25: For-body calls must be checked too (walker arm was missing).
#[test]
fn audit2_vera_h25_for_body_requires_checked() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func pos(v: i32) -> i32 {
    requires: v > 0
    ensures: result > 0
    v
}
func caller(y: i32) -> i32 {
    ensures: result == 0
    for v in [y] {
        pos(y)
    }
    0
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "caller"),
        Some(crate::verifier::VerifStatus::Disproven),
        "pos(y) inside a for body must discharge requires: y > 0: {:?}",
        results
    );
}

/// H-25: SharedLet initializers must be checked too (walker arm missing).
#[test]
fn audit2_vera_h25_sharedlet_requires_checked() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func pos(v: i32) -> i32 {
    requires: v > 0
    ensures: result > 0
    v
}
func caller(y: i32) -> i32 {
    ensures: result == 0
    shared s = pos(y)
    0
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "caller"),
        Some(crate::verifier::VerifStatus::Disproven),
        "shared s = pos(y) must discharge requires: y > 0: {:?}",
        results
    );
}

/// V-2 (MED): bool/string params' old() was never asserted old == current in
/// the AST path (int/real only), while the Resolved engine completed all
/// three — engine inconsistency. `ensures: old(s) == s` was a fake Disproven
/// (old_s unconstrained → counterexample s = "A").
#[test]
fn audit2_vera_v2_old_string_equality_proven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func keeps(s: string) -> i32 {
    ensures: old(s) == s
    0
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "keeps"),
        Some(crate::verifier::VerifStatus::Proven),
        "old(s) == s is an identity on immutable parameters; Disproven means \
         old_s was never equated with s: {:?}",
        results
    );
}

/// V-2 (VIR half): bool params get old snapshots now. Before the fix,
/// `ensures: old(b) == b` on a pure-bool (trusted-subset) function was
/// unencodable in VIR → NotInTrustedSubset.
#[test]
fn audit2_vera_v2_old_bool_vir_proven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func keep(b: bool) -> bool {
    ensures: old(b) == b
    b
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "keep"),
        Some(crate::verifier::VerifStatus::Proven),
        "old(b) == b must prove on the VIR path: {:?}",
        results
    );
}

/// V-6 (LOW→fail-closed): i64 was modeled as unbounded Int with NO
/// definedness, contradicting SD-7/SD-8 trap semantics. Minimal fix mirrors
/// the i32 machinery for div/mod (zero divisor + MIN÷-1) and neg (MIN).
#[test]
fn audit2_vera_v6_i64_div_zero_disproven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func g(x: i64, z: i64) -> i64 {
    ensures: result == result
    x / z
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "g"),
        Some(crate::verifier::VerifStatus::Disproven),
        "i64 division by an unconstrained divisor traps E0801 at z == 0; \
         Proven means i64 definedness is still missing: {:?}",
        results
    );
}

/// V-6 control: a strict-positive divisor discharges both the zero-divisor
/// and the MIN÷-1 obligations.
#[test]
fn audit2_vera_v6_i64_div_guarded_proven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func g(x: i64, z: i64) -> i64 {
    requires: z > 0
    ensures: result == result
    x / z
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "g"),
        Some(crate::verifier::VerifStatus::Proven),
        "z > 0 makes the i64 division defined everywhere: {:?}",
        results
    );
}

/// V-6: negation of i64::MIN traps — must Disprove without an excludes-MIN
/// precondition.
#[test]
fn audit2_vera_v6_i64_neg_min_disproven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func n(x: i64) -> i64 {
    ensures: result == result
    -x
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "n"),
        Some(crate::verifier::VerifStatus::Disproven),
        "negating an unconstrained i64 can hit MIN (runtime trap): {:?}",
        results
    );
}

/// Wave-2 item 7 (wave1-review §5.8): ffi.rs call-site discovery must cover
/// every top-level function — some declaration forms were a blind spot for
/// `--verify-ffi` even after Wave-1 exhausted the statement/expr positions.
/// (0.39.139: inline `module` nesting no longer exists; the walker is flat.)
/// §11-#46/V-6 (audit 2026-08-05, closed 2026-08-07): i64 add/sub/mul 现与
/// i32 同等携带溢出义务（SD-7 trap 对齐）。无界操作数且前置条件不约束
/// 范围 → fail-closed Disproven（此前静默 Proven，披露语掩盖假设）。
#[test]
fn audit2_vera_46_i64_add_unbounded_disproven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func add(a: i64, b: i64) -> i64 {
    ensures: result == a + b
    a + b
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "add"),
        Some(crate::verifier::VerifStatus::Disproven),
        "unbounded i64 add may overflow (SD-7 trap); Proven means the \
         overflow obligation is still missing: {:?}",
        results
    );
}

/// §11-#46 对侧：前置条件约束操作数范围 → 溢出义务可解除，Proven。
#[test]
fn audit2_vera_46_i64_add_bounded_proven() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func add(a: i64, b: i64) -> i64 {
    requires: a > -100 && a < 100
    requires: b > -100 && b < 100
    ensures: result == a + b
    a + b
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "add"),
        Some(crate::verifier::VerifStatus::Proven),
        "bounded operands discharge the i64 overflow obligation: {:?}",
        results
    );
}

/// §11-#47 (audit 2026-08-05, closed 2026-08-07): f64 let 绑定此前静默跳过
/// （变量不断言到初始化表达式）。现经 encode_f64 断言恒等——可编码形状
/// （变量/常量/Result）登记为约束；不可编码形状诚实 NotInTrustedSubset。
/// 回归钉死：f64 let 不再静默丢失（constraint_count 含绑定项），且可证
/// 契约不被绑定改动破坏。
#[test]
fn audit2_vera_47_f64_let_binding_registered() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(x: f64) -> f64 {
    math: { 2 + 2 == 4 }
    let y = x
    y
}
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    let f = results.iter().find(|r| r.func_name == "f");
    assert!(f.is_some(), "f should be verified: {:?}", results);
    let f = f.unwrap();
    assert_eq!(
        f.status,
        crate::verifier::VerifStatus::Proven,
        "math obligation must still prove with the f64 let bound: {:?}",
        f.message
    );
    // math(1) + let-binding(1) — pre-§11-#47 the let contributed nothing.
    assert!(
        f.constraint_count >= 2,
        "f64 let binding must be registered as a constraint, got {} ({})",
        f.constraint_count,
        f.message
    );
}

/// §11-#48 (audit 2026-08-05, closed 2026-08-07): AST 引擎 encode_match_bool
/// 的非穷尽落空分支此前硬编 `false`——scrutinee 编码为无界 Int 时落空分支
/// 可达，verifier 凭空假设结果为 false（双向假证）。镜像 int 路径 E2 修复：
/// 落空用无约束变量。回归钉死：非穷尽 bool match 编码后 fallback 变量已
/// 登记（而非硬编常量）。
#[test]
fn audit2_vera_48_match_bool_fallback_unconstrained() {
    if !z3_or_skip() {
        return;
    }
    let file = crate::tests::parse(
        r#"
func f(x: i32) -> bool {
    match x {
        1 => true
        2 => false
    }
}
"#,
    );
    let function = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(f) if f.name == "f" => Some(f),
            _ => None,
        })
        .expect("func f parsed");
    let arms = function
        .body
        .iter()
        .find_map(|stmt| match stmt.unlocated() {
            crate::ast::Stmt::Expr(e) => match e.unlocated() {
                crate::ast::Expr::Match(_, arms) => Some(arms.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("match expression parsed");
    let mut vars = crate::verifier::Z3VarMap::new();
    let matched = vars.get_or_create_int("x");
    let encoded = crate::verifier::encode_match_bool(&matched, &arms, &mut vars)
        .expect("non-exhaustive bool match must encode");
    let _ = encoded;
    assert!(
        vars.get_bool("_match_fallback_bool").is_some(),
        "non-exhaustive bool match fallback must be an unconstrained \
         variable, not a hardcoded `false` (pre-§11-#48)"
    );
}

/// batch4-08 P0-1: the AST fallback must not return Proven for a function
/// whose body contains checked arithmetic in a non-tail statement. The old
/// flow_ast path only proved definedness for the extracted tail expression,
/// so a crashing `x / y` inside an `if` branch could be verified as safe.
#[test]
fn audit2_vera_p01_non_tail_arith_fails_closed() {
    if !z3_or_skip() {
        return;
    }
    let src = r#"
func f(x: i32, y: i32) -> i32 {
    requires: true
    ensures: result == 0
    if x > 0 { x / y; 0 } else { 0 }
}
func main() -> i32 { 0 }
"#;
    let results = crate::verifier::verify_source(src).expect("verify_source");
    assert_eq!(
        status_of(&results, "f"),
        Some(crate::verifier::VerifStatus::NotInTrustedSubset),
        "non-tail checked arithmetic must fail closed: {:?}",
        results
    );
}
