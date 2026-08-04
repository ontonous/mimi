//! Wave-1 audit-fix regression tests — parser.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
use super::*;


/// Parse-level diagnostics (message list) without panicking. Mirrors the
/// helper in audit_regression.rs so parse/lex errors are observable.
fn parse_diag_messages(src: &str) -> Vec<String> {
    match crate::lexer::Lexer::new(src).tokenize() {
        Err(e) => vec![e.to_string()],
        Ok(tokens) => match crate::parser::Parser::new(tokens).parse_file() {
            Ok(_) => Vec::new(),
            Err(e) => vec![e.message.clone()],
        },
    }
}

// ═══════════════════════════════════════════════════════════════
// Fix 1 — f-string \xNN / \uXXXX / \u{...} decoding (parse_stmt.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn fstring_decodes_hex_and_unicode_escapes_like_normal_strings() {
    // Bug: the lexer validated \xNN / \uXXXX / \u{...} inside f-strings but
    // parse_fstring_parts never decoded them, so f"\x41" stayed the literal
    // 4 characters "\x41" while "\x41" == "A". Now both decode identically.
    // Both backends consume the same parser output, so assert stdout parity.
    let src = r#"
func main() -> i32 {
    let a = f"\x41"
    let b = f"\u0042"
    let c = f"\u{43}"
    if a == "A" {
        if b == "B" {
            if c == "C" {
                println(1)
                return 0
            }
        }
    }
    println(0)
    return 0
}
"#;
    assert!(check_source(src).is_ok(), "check: {:?}", check_source(src));
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "1",
        "VM: f-string escapes must decode to A/B/C"
    );
    if crate::tests::can_link() {
        if let Ok(out) = compile_and_run(src) {
            assert_eq!(
                out.trim(),
                "1",
                "codegen: f-string escapes must decode identically to the VM"
            );
        }
    }
}

#[test]
fn fstring_decode_matches_plain_string_and_interpolation_survives() {
    // f"\x41" must equal the plain-string decode "\x41", and interpolation
    // must keep working alongside decoded escapes.
    let src = r#"
func main() -> i32 {
    let x = 7
    let hex = f"\x41"
    let plain = "\x41"
    let mixed = f"[{x}]\u0041"
    if hex == plain {
        if plain == "A" {
            if mixed == "[7]A" {
                println(1)
                return 0
            }
        }
    }
    println(0)
    return 0
}
"#;
    assert!(check_source(src).is_ok(), "check: {:?}", check_source(src));
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "1", "VM mismatch");
    if crate::tests::can_link() {
        if let Ok(out) = compile_and_run(src) {
            assert_eq!(out.trim(), "1", "codegen mismatch");
        }
    }
}

#[test]
fn fstring_invalid_unicode_scalar_is_rejected() {
    // \uD800 is a UTF-16 surrogate — passes the lexer's 4-hex-digit check but
    // is not a valid Unicode scalar value; char::from_u32 fails, so the parser
    // must reject it instead of decoding garbage.
    let src = r#"func main() -> i32 { let s = f"\uD800"; 0 }"#;
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.iter().any(|m| m.contains("invalid \\u escape")),
        "surrogate escape must be rejected, got: {:?}",
        msgs
    );
}

// ═══════════════════════════════════════════════════════════════
// Fix 2 — float literal finiteness (SD-9, E0813) (parse_expr.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn nonfinite_float_literal_rejected_at_parse_time() {
    // Bug: `1e999` overflowed parse::<f64>() into +Inf and bypassed the SD-9
    // finiteness trap (the literal never goes through an operation). The
    // integer path is already fail-closed; now floats are too.
    for lit in ["1e999", "1e400", "9e999", "1E999"] {
        let src = format!("func main() -> i32 {{ let x = {}\n 0 }}", lit);
        let msgs = parse_diag_messages(&src);
        assert!(
            msgs.iter()
                .any(|m| m.contains("E0813") && m.contains("not finite")),
            "literal {} must be rejected with E0813, got: {:?}",
            lit,
            msgs
        );
    }
}

#[test]
fn finite_float_literals_still_parse() {
    // Regression guard: the finiteness check must not reject valid finite
    // literals (1e308 is the largest finite f64 power-of-ten literal).
    for lit in ["1.5", "1e10", "1e308", "0.0", "2.5e-3"] {
        let src = format!("func main() -> i32 {{ let x = {}\n 0 }}", lit);
        assert!(
            parse_diag_messages(&src).is_empty(),
            "finite literal {} must parse, got errors",
            lit
        );
    }
}

// ═══════════════════════════════════════════════════════════════
// Fix 3 — enum variants with record payloads (parse_type.rs lookahead)
// ═══════════════════════════════════════════════════════════════

#[test]
fn enum_variant_with_record_payload_parses_and_checks() {
    // Bug: lookahead_is_record scanned PAST `{`, so an enum whose first
    // variant carries a record payload was misclassified as a record and
    // failed with `expected \`}\`, found :`. The scan now stops at the first
    // non-ident token (`{`, `(`, `,`, ...).
    let src = r#"
type Shape { Circle { r: f64 }, Point }
func main() -> i32 { 0 }
"#;
    assert!(
        parse_diag_messages(src).is_empty(),
        "record-payload enum must parse, got: {:?}",
        parse_diag_messages(src)
    );
    assert!(
        check_source(src).is_ok(),
        "record-payload enum must type-check: {:?}",
        check_source(src)
    );
}

#[test]
fn enum_record_payload_multiline_and_mixed_variants_parse() {
    let src = r#"
type Tree {
    Leaf
    Node { value: i32 }
    Pair(i32, i32)
}
func main() -> i32 { 0 }
"#;
    assert!(
        parse_diag_messages(src).is_empty(),
        "mixed enum variants must parse, got: {:?}",
        parse_diag_messages(src)
    );
    assert!(check_source(src).is_ok(), "check: {:?}", check_source(src));
}

#[test]
fn records_still_classified_as_records_after_lookahead_fix() {
    // Regression guard: tightening the lookahead must not demote real records.
    // Covers plain, multiline, and M4 soft-keyword-first-field records.
    let src = r#"
type Rec { a: i32, b: i32 }
type Rec2 {
    x: i32
    y: i32
}
type Rec3 { and: i32 }
func main() -> i32 {
    let r = Rec { a: 1, b: 2 }
    let r2 = Rec2 { x: 3, y: 4 }
    let r3 = Rec3 { and: 5 }
    r.a + r.b + r2.x + r2.y + r3.and
}
"#;
    assert!(check_source(src).is_ok(), "check: {:?}", check_source(src));
    assert_eq!(run_source(src).as_int().unwrap_or(-1), 15);
}

// ═══════════════════════════════════════════════════════════════
// Fix 4 — math:{} recovery loop no longer eats its `}` (parse_stmt.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn recovery_math_block_does_not_swallow_following_statements() {
    // Bug: in recovery mode, if parse_expr failed with the cursor ON the math
    // block's own `}`, the blind advance() consumed that terminator and pulled
    // every following statement into the math node. The fix breaks instead of
    // advancing when the cursor sits on RBrace/Eof.
    let src = r#"
func main() -> i32 {
    let bad = ;
    math: { (1 + }
    let marker = 7
    return marker
}
"#;
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let (file, _errors) =
        crate::parser::Parser::new_with_recovery(tokens).parse_file_with_recovery();
    let crate::ast::Item::Func(f) = &file.items[0] else {
        panic!("expected a function item");
    };
    let has_marker_let = f.body.iter().any(|s| {
        matches!(
            s.unlocated(),
            crate::ast::Stmt::Let { pat, .. } if pat.single_var_name() == Some("marker")
        )
    });
    assert!(
        has_marker_let,
        "statement after the math block was swallowed into it: {:#?}",
        f.body
    );
}

// ═══════════════════════════════════════════════════════════════
// Fix 5 — depth guards on pattern / session-type / module recursion
// ═══════════════════════════════════════════════════════════════

#[test]
fn deeply_nested_pattern_hits_recursion_limit_not_stack_overflow() {
    let depth: usize = 400;
    let pat = format!("{}x{}", "(".repeat(depth), ")".repeat(depth));
    let src = format!("func main() -> i32 {{ let {} = 0\n 0 }}", pat);
    let res = run_source_result(&src);
    let err = res.expect_err("deeply nested pattern must be rejected");
    assert!(
        err.contains("recursion limit"),
        "expected recursion-limit error, got: {}",
        err
    );
}

#[test]
fn deeply_nested_module_hits_recursion_limit_not_stack_overflow() {
    let depth: usize = 400;
    let src = format!("{}{}", "module m { ".repeat(depth), "}".repeat(depth));
    let res = run_source_result(&src);
    let err = res.expect_err("deeply nested modules must be rejected");
    assert!(
        err.contains("recursion limit"),
        "expected recursion-limit error, got: {}",
        err
    );
}

#[test]
fn deeply_chained_session_type_hits_recursion_limit_not_stack_overflow() {
    let depth: usize = 400;
    let src = format!("session S = {}end", "!i32 . ".repeat(depth));
    let res = run_source_result(&src);
    let err = res.expect_err("deeply chained session type must be rejected");
    assert!(
        err.contains("recursion limit"),
        "expected recursion-limit error, got: {}",
        err
    );
}

// ═══════════════════════════════════════════════════════════════
// Fix 6 — digit separator violations are lex errors (lexer/flow.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn digit_separator_violations_are_lex_errors_not_silent_retokenization() {
    // Bug: `1__2` / `1e_5` / trailing `_` silently re-tokenized as number +
    // identifier (changing meaning); octal even accepted `__`. All separator
    // violations are now lex errors.
    for bad in [
        "let x = 1__2",
        "let x = 1_",
        "let x = 1e_5",
        "let x = 1_e5",
        "let x = 0x1__f",
        "let x = 0b1__0",
        "let x = 0o1__7",
        "let x = 1.5__5",
        "let x = 1e5_",
    ] {
        let res = crate::lexer::Lexer::new(bad).tokenize();
        assert!(
            res.is_err(),
            "separator violation must be a lex error: {:?}",
            bad
        );
        let msg = res.unwrap_err().to_string();
        assert!(
            msg.contains("digit separator"),
            "error must mention the digit separator for {:?}, got: {}",
            bad,
            msg
        );
    }
}

#[test]
fn valid_digit_separators_and_bare_underscore_still_lex() {
    // Regression guard: legal separators between digits still tokenize, and a
    // standalone `_` remains a valid identifier/wildcard.
    for good in [
        "let x = 1_000",
        "let x = 1_000_000",
        "let x = 0xFF_FF",
        "let x = 0b1010_0101",
        "let x = 0o7_7",
        "let x = 1e1_0",
        "let x = 1.5_5",
    ] {
        assert!(
            crate::lexer::Lexer::new(good).tokenize().is_ok(),
            "valid separator must lex: {:?}",
            good
        );
    }
    // Standalone `_` is an identifier/wildcard, not a number-adjacent separator.
    let wild = "func main() -> i32 { let _ = 1\n 0 }";
    assert!(crate::lexer::Lexer::new(wild).tokenize().is_ok());
    assert_eq!(run_source(wild).as_int().unwrap_or(-1), 0);
}

// ═══════════════════════════════════════════════════════════════
// Fix 7 — pinned binder requires its closing `|` (ADR-002) (parse_stmt.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn pinned_binder_without_closing_pipe_is_rejected() {
    // Bug: `pinned(x) |name { }` (missing the closing `|`) was silently
    // accepted. ADR-002 binder form is `pinned(expr) | name | { ... }`.
    let src = r#"
flow Buffer {
    state Active { data: i32 }
    transition use_pinned(Active) -> Active {
        pinned(self.data) |ptr {
            let _ = ptr
        }
        return Active { data: self.data }
    }
}
func main() -> i32 { 0 }
"#;
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.iter()
            .any(|m| m.contains("pinned binder") && m.contains("ADR-002")),
        "missing closing `|` must be rejected with an ADR-002 message, got: {:?}",
        msgs
    );
}

#[test]
fn pinned_binder_with_closing_pipe_still_parses_and_runs() {
    // Regression guard: the correct `|name|` form is unaffected.
    let src = r#"
flow Buffer {
    state Active { data: i32 }
    transition use_pinned(Active) -> Active {
        pinned(self.data) |ptr| {
            let _ = ptr
        }
        return Active { data: self.data + 1 }
    }
}
func main() -> i32 {
    let s = Active { data: 100 }
    let r = Buffer::use_pinned(s)
    println(r.data)
    0
}
"#;
    assert!(check_source(src).is_ok(), "check: {:?}", check_source(src));
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "101", "VM pinned binder output");
    if crate::tests::can_link() {
        if let Ok(out) = compile_and_run(src) {
            assert_eq!(out.trim(), "101", "codegen pinned binder output");
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Fix 8 — speculative map/set rewind truncates ghost errors (parse_expr.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn speculative_map_set_rewind_leaves_no_ghost_diagnostics() {
    // Speculative map/set literal parsing rewinds self.pos but used to leave
    // recovery diagnostics in self.errors. The inner `let x = ;` failure is
    // then reported once by the map attempt, once by the set attempt, and once
    // by the real block parse (3×). After truncating on rewind only the real
    // parse reports it (1×).
    let src = "func f() -> i32 {\n    let v = { { let x = ; 1 } }\n    v\n}\n";
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("tokenize");
    let (_file, errors) =
        crate::parser::Parser::new_with_recovery(tokens).parse_file_with_recovery();
    let ghost_count = errors
        .iter()
        .filter(|e| e.message.contains("expected expression after `=`"))
        .count();
    assert_eq!(
        ghost_count,
        1,
        "speculative rewinds left ghost diagnostics (expected exactly 1 report): {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ═══════════════════════════════════════════════════════════════
// Fix 9 — duplicate `fault T` in a flow (E0402) (top_level.rs)
// ═══════════════════════════════════════════════════════════════

#[test]
fn duplicate_fault_declaration_in_flow_is_rejected() {
    // Bug: a second `fault T` silently overwrote the first (last-wins). A flow
    // has exactly one fault type; the duplicate is now E0402.
    let src = r#"
type E1 { code: i32 }
type E2 { code: i32 }
flow F {
    state Idle
    fault E1
    fault E2
}
func main() -> i32 { 0 }
"#;
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.iter()
            .any(|m| m.contains("E0402") && m.contains("duplicate")),
        "duplicate fault must be rejected with E0402, got: {:?}",
        msgs
    );
}

#[test]
fn single_fault_declaration_still_parses_and_checks() {
    // Regression guard: exactly one `fault T` remains valid (the new duplicate
    // check must not over-trigger on the first/only declaration).
    let src = r#"
type MyErr { code: i32 }
flow F {
    state Idle { n: i32 }
    fault MyErr
    transition step(Idle, d: i32) -> Idle { return Idle { n: self.n + d } }
}
func main() -> i32 { 0 }
"#;
    assert!(
        parse_diag_messages(src).is_empty(),
        "single fault must parse, got: {:?}",
        parse_diag_messages(src)
    );
    assert!(check_source(src).is_ok(), "check: {:?}", check_source(src));
}

