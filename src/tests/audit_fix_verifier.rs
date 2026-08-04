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
        return 1;
    }
    0
}
"#;
    let results = crate::verifier::verify_ffi_source(src).expect("verify_ffi_source");
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
        return 0;
    }
    s
}
"#;
    let results = crate::verifier::verify_ffi_source(src).expect("verify_ffi_source");
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
