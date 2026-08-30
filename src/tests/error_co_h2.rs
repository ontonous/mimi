//! CO-H2 regression locks (0.35.19, dx-backlog #7): precise error spans and
//! user-readable diagnostics for resolved-lowering failures.
//! Findings: devdocs/v0.35/error-coh2-0.35.19.md.
//!
//! Before 0.35.19, a statement-position tail `if/else` whose branches had
//! mismatched types slipped past the checker and aborted the resolved layer
//! with an internal `TOOL-RESOLUTION-001: resolved body node
//! 'function:f/generated:...' types 'rt:<hash>' and 'rt:<hash>' have no
//! admitted implicit conversion` — no E code, no source span, internal type
//! IDs leaked. The fix: the checker checks tail if/else branches
//! bidirectionally (E0214 + precise span), and the resolved lowering renders
//! canonical types by language name instead of internal IDs.
use super::*;

/// CO-H2: tail if/else branch mismatch must surface as E0214 at the if
/// statement, not as an internal TOOL-RESOLUTION-001 with no span.
#[test]
fn coh2_tail_if_branch_mismatch_is_e0214_with_exact_span() {
    let src =
        "func f(c: bool) -> i32 {\n    if c { \"a\" } else { 1 }\n}\nfunc main() -> i32 { 0 }\n";
    let diagnostics = check_source(src).expect_err("tail if/else mismatch must fail check");
    let e0214: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code.as_deref() == Some("E0214"))
        .collect();
    assert_eq!(
        e0214.len(),
        1,
        "expected exactly one E0214, got: {:?}",
        diagnostics
    );
    assert!(
        e0214[0].message.contains("string vs i32"),
        "E0214 must name the branch types: {}",
        e0214[0].message
    );
    // The span must point at the if statement (line 2), not the function.
    assert_eq!(
        e0214[0].span.start_line, 2,
        "E0214 must anchor the if statement, got {:?}",
        e0214[0].span
    );
    // No internal identifiers may leak.
    for d in &diagnostics {
        assert!(
            !d.message.contains("TOOL-RESOLUTION-001") && !d.message.contains("rt:"),
            "internal identifier leaked: {}",
            d.message
        );
    }
}

/// CO-H2: an if whose branches are both `return`s (diverging) must not
/// report a branch-type mismatch — the branches never produce a value.
#[test]
fn coh2_diverging_branches_do_not_unify() {
    let src = "func classify(x: i32) -> i32 {\n    if x > 0 { return 1 } else { return -1 }\n}\nfunc main() -> i32 { classify(1) }\n";
    check_source(src)
        .unwrap_or_else(|diags| panic!("diverging branches must type-check: {:?}", diags));
    // Flow multi-target: both branches return different states.
    let flow = "flow F {\n    state A { v: i32 }\n    state B { v: i32 }\n    transition pick(A, go: bool) -> A | B {\n        if go { return B { v: 1 } } else { return A { v: 2 } }\n    }\n}\nfunc main() -> i32 { 0 }\n";
    check_source(flow)
        .unwrap_or_else(|diags| panic!("flow diverging branches must type-check: {:?}", diags));
}

/// CO-H2: numeric widening between branches stays legal (no E0214).
#[test]
fn coh2_numeric_widening_branches_are_legal() {
    let src = "func f(c: bool) -> i64 {\n    if c { 1 } else { 2 }\n}\nfunc main() -> i32 { 0 }\n";
    check_source(src).unwrap_or_else(|diags| panic!("i32/i64 branches rejected: {:?}", diags));
    let mixed =
        "func g(c: bool) -> f64 {\n    if c { 1 } else { 2.5 }\n}\nfunc main() -> i32 { 0 }\n";
    check_source(mixed).unwrap_or_else(|diags| panic!("int/float branches rejected: {:?}", diags));
}

/// CO-H2: lowering errors render canonical types by language name, never
/// as `rt:<hash>` internal identities.
#[test]
fn coh2_lowering_error_uses_language_type_names() {
    let src =
        "func f(c: bool) -> i32 {\n    if c { \"a\" } else { 1 }\n}\nfunc main() -> i32 { 0 }\n";
    let diagnostics = check_source(src).expect_err("mismatch must fail check");
    for d in &diagnostics {
        assert!(
            !d.message.contains("rt:"),
            "internal type id leaked in message: {}",
            d.message
        );
    }
}

/// CO-H2 / §5.2-2 diagnostic self-healing: a call to a bytecode-VM-only
/// reflection builtin (`type_fields`) must surface as a structured `E0830`
/// diagnostic that names the failing callable and anchors the call site —
/// not as an internal `TOOL-RESOLUTION-001` jargon leak with no actionable
/// information (the pre-fix message was
/// `typed body lowering does not yet support closed Unknown call target`).
#[test]
fn coh2_reflection_builtin_type_fields_is_e0830_not_jargon() {
    let src = "type R { a: i32 }\nfunc main() -> i32 {\n    let r = R { a: 1 };\n    println(type_fields(r));\n    0\n}\n";
    let diagnostics = check_source(src).expect_err("type_fields must fail resolved body lowering");
    let e0830: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code.as_deref() == Some("E0830"))
        .collect();
    assert_eq!(
        e0830.len(),
        1,
        "expected exactly one E0830, got: {:?}",
        diagnostics
    );
    assert!(
        e0830[0].message.contains("type_fields"),
        "E0830 must name the failing builtin: {}",
        e0830[0].message
    );
    assert!(
        !e0830[0].message.contains("TOOL-RESOLUTION-001"),
        "internal jargon leaked: {}",
        e0830[0].message
    );
    // The span must point at the call (line 4), not the function root.
    assert_eq!(
        e0830[0].span.start_line, 4,
        "E0830 must anchor the call, got {:?}",
        e0830[0].span
    );
}
