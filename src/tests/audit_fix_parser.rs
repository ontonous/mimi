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

/// §1-#10 (audit 2026-08-05, closed 2026-08-07): braces inside QUOTED
/// literals within an f-string interpolation must not count toward the
/// interpolation depth. Pre-fix, `f"{ "}" }"` closed the interpolation at
/// the `}` inside the quotes and failed loudly at a wrong position.
#[test]
fn fstring_interpolation_brace_inside_quoted_literal() {
    let src = r#"
func main() -> i32 {
    let s = f"x{ "}" }y"
    if s == "x}y" {
        println(1)
    } else {
        println(0)
    }
    0
}
"#;
    assert!(check_source(src).is_ok(), "check: {:?}", check_source(src));
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "1", "VM mismatch");
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
    // Find the user func by name, not items[0]: progressive typestate
    // (src/progressive.rs) injects an implicit `flow Main` at index 0 for
    // script-mode files, so positional indexing lands on the synthetic flow.
    // Same pattern as v1_2_error_paths.rs / basic_let.rs recovery tests.
    let f = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("expected a function item");
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

// ── 0.35.25 C2 回归锁（audit-triage-0.35.25.md）──────────────
// 修复前：嵌套 f-string 插值每层新建 Parser 实例、recursion_depth 从 0
// 重数，深度守卫对跨实例链完全失察——6000 层嵌套（~30KB）SIGSEGV
// （CLI 8MB 栈）；libtest 2MB 栈上仅 40 层即溢出。
// 修复后：子解析器继承外层深度 + f-string 专用预算 DEPTH_MAX_FSTRING=64
// （≈32 层嵌套，~1.7MB，实测 31 层在 2MB libtest 栈安全）。
// 两个探针：深层必须返回 ParseError 而非崩溃；预算内的合法嵌套必须
// 在 2MB libtest 线程栈上解析成功。

/// Deeply nested f-string interpolation must hit the recursion limit
/// (ParseError), not SIGSEGV. Runs on a 2 MB libtest thread stack like the
/// other depth probes — the pre-fix failure mode was stack overflow abort.
#[test]
fn nested_fstring_hits_recursion_limit_not_stack_overflow() {
    let depth: usize = 6000;
    let inner = {
        let mut s = String::from("a");
        for _ in 0..depth {
            s = format!("f\"{{{}}}\"", s);
        }
        s
    };
    let src = format!("func main() -> i32 {{\n let x = {}\n 0\n}}", inner);
    let tokens = crate::lexer::Lexer::new(&src)
        .tokenize()
        .expect("lex nested fstring");
    let res = crate::parser::Parser::new(tokens).parse_file();
    let err = res.expect_err("6000-level fstring must be rejected, not crash");
    assert!(
        err.message.contains("recursion limit"),
        "expected recursion-limit error, got: {}",
        err.message
    );
}

/// f-string nesting within the budget must parse successfully on a 2 MB
/// libtest thread stack (31 levels ≈ 1.6 MB measured).
#[test]
fn nested_fstring_within_budget_parses_on_test_stack() {
    let depth: usize = 24;
    let inner = {
        let mut s = String::from("a");
        for _ in 0..depth {
            s = format!("f\"{{{}}}\"", s);
        }
        s
    };
    let src = format!("func main() -> i32 {{\n let x = {}\n 0\n}}", inner);
    let tokens = crate::lexer::Lexer::new(&src)
        .tokenize()
        .expect("lex nested fstring");
    let res = crate::parser::Parser::new(tokens).parse_file();
    assert!(
        res.is_ok(),
        "24-level fstring must parse on 2 MB test stack, got: {:?}",
        res.err().map(|e| e.message.clone())
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

// ═══════════════════════════════════════════════════════════════
// WAVE-2 stack-budget measurement harness (inert by default).
// Driven by MIMI_PROBE_DEPTH / MIMI_PROBE_SHAPE. Runs on a libtest
// thread (2 MB stack) exactly like the red-line test.
// NOTE (0.35.25, M1): the MIMI_PROBE_CAP depth override was removed
// from check_depth_with (helpers.rs) — budgets are now fixed per
// recursion path; this harness exercises the fixed caps only.
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_probe_depth_budget() {
    // Inert unless driven explicitly (normal suite runs skip it).
    let Ok(raw) = std::env::var("MIMI_PROBE_DEPTH") else {
        return;
    };
    let depth: usize = raw.parse().unwrap();
    let shape = std::env::var("MIMI_PROBE_SHAPE").unwrap_or_else(|_| "module".into());
    let src = match shape.as_str() {
        "module" => format!("{}{}", "module m { ".repeat(depth), "}".repeat(depth)),
        "session" => format!("session S = {}end", "!i32 . ".repeat(depth)),
        "pattern" => {
            let pat = format!("{}x{}", "(".repeat(depth), ")".repeat(depth));
            format!("func main() -> i32 {{ let {} = 0\n 0 }}", pat)
        }
        "paren_expr" => {
            let e = format!("{}0{}", "(".repeat(depth), ")".repeat(depth));
            format!("func main() -> i32 {{ let x = {}\n x }}", e)
        }
        "if_nest" => {
            let mut s = String::from("func main() -> i32 {\n");
            for _ in 0..depth {
                s.push_str("if true { ");
            }
            s.push_str("42");
            for _ in 0..depth {
                s.push_str(" } else { 0 }");
            }
            s.push_str("\n}");
            s
        }
        other => panic!("unknown probe shape {other}"),
    };
    let tokens = crate::lexer::Lexer::new(&src)
        .tokenize()
        .expect("lex probe source");
    let res = crate::parser::Parser::new(tokens).parse_file();
    match res {
        Ok(_) => println!("PROBE shape={shape} depth={depth} PARSE_OK"),
        Err(e) => println!("PROBE shape={shape} depth={depth} PARSE_ERR: {}", e.message),
    }
}

// ═══════════════════════════════════════════════════════════════
// Wave-2 agent PM — red line: module-path depth cap (wave1-review §1.2)
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_module_cap_fires_below_default_cap() {
    // The module path recurses through 5 frames per nesting level
    // (parse_module → parse_module_inner → parse_item_block → parse_item →
    // parse_item_kind), so it gets its own cap (helpers.rs
    // DEPTH_MAX_MODULE = 32), well below the default 128. Depths between
    // the two caps must be rejected by the MODULE cap — proving the
    // module-specific budget is wired, not just the shared one.
    let depth: usize = 40; // > 32 (module cap), < 128 (default cap)
    let src = format!("{}{}", "module m { ".repeat(depth), "}".repeat(depth));
    let msgs = parse_diag_messages(&src);
    assert!(
        msgs.iter()
            .any(|m| m.contains("recursion limit") && m.contains("> 32 nested")),
        "module nesting beyond the module cap must mention the module cap, got: {:?}",
        msgs
    );
}

#[test]
fn audit2_pm_shallow_module_nesting_still_parses() {
    // 16 nested modules (half the module cap) must parse cleanly so real
    // code keeps headroom; also validates the cap is not accidentally 0.
    let depth: usize = 16;
    let src = format!("{}{}", "module m { ".repeat(depth), "}".repeat(depth));
    let msgs = parse_diag_messages(&src);
    assert!(
        msgs.is_empty(),
        "16 nested modules must parse, got: {:?}",
        msgs
    );
}

// ═══════════════════════════════════════════════════════════════
// P-1 — sketch+recovery orphan `}` must make progress, not hang
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_sketch_recovery_orphan_brace_makes_progress() {
    // Sketch-mode blocks terminate on Dedent, so an orphan `}` at statement
    // position is not the terminator. In recovery mode recover_to_sync
    // stops ON the `}` (it is a sync token), nothing consumed it, and the
    // loop retried parse_stmt on the same token forever (infinite hang).
    // Run on a watchdog thread: a hang fails the test instead of freezing
    // the whole suite.
    let src = "func main():\n    }\n";
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let tokens = crate::lexer::Lexer::new_sketch(src)
            .tokenize()
            .expect("lex sketch source");
        let parser = crate::parser::Parser::splice(
            &tokens,
            0,
            crate::parser::ParseMode::Sketch,
            true,
            crate::span::SourceId::UNKNOWN,
        );
        let (_file, _errors) = parser.parse_file_with_recovery();
        let _ = tx.send(());
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok(),
        "sketch+recovery parse hung on an orphan `}}` (P-1 regression)"
    );
}

// ═══════════════════════════════════════════════════════════════
// P-2 — parse_expr_without_range no longer leaks recursion_depth
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_failed_slice_starts_do_not_leak_depth_under_recovery() {
    // Each `a[)..]` statement fails inside parse_expr_without_range before
    // any token is consumed. The old code skipped dec_depth on that error
    // path, leaking one depth level per statement; after 128 failures the
    // next statement got a FALSE "recursion limit exceeded". Build 140
    // failing slice statements under recovery and require (a) no
    // recursion-limit error among the collected diagnostics and (b) the
    // trailing valid statement still parsed.
    let mut body = String::new();
    for _ in 0..140 {
        body.push_str("    let v = a[)..]\n");
    }
    body.push_str("    let marker = 7\n");
    let src = format!("func main() -> i32 {{\n{}}}", body);
    let tokens = crate::lexer::Lexer::new(&src).tokenize().expect("lex");
    let (file, errors) =
        crate::parser::Parser::new_with_recovery(tokens).parse_file_with_recovery();
    assert!(
        !errors.iter().any(|e| e.message.contains("recursion limit")),
        "false recursion-limit after many failed slice starts: {:?}",
        errors
            .iter()
            .filter(|e| e.message.contains("recursion limit"))
            .collect::<Vec<_>>()
    );
    assert!(
        errors.len() >= 140,
        "the 140 malformed slices must each surface an error, got {}",
        errors.len()
    );
    // Find the user func by name: progressive typestate injects an implicit
    // `flow Main` at items[0] for script-mode files, so positional indexing
    // would land on the synthetic flow, not the func.
    let f = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("expected a function item");
    assert!(
        f.body.iter().any(|s| matches!(
            s.unlocated(),
            crate::ast::Stmt::Let { pat, .. } if pat.single_var_name() == Some("marker")
        )),
        "statement after the failed slices must still parse: {:#?}",
        f.body
    );
}

// ═══════════════════════════════════════════════════════════════
// P-3 — numeric separators in array sizes and flow annotations
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_array_size_accepts_digit_separators() {
    let src = "func f(a: [i32; 1_000]) -> i32 { 0 }\nfunc main() -> i32 { f([]) }";
    // Parse level: `[i32; 1_000]` must no longer be rejected.
    let msgs = parse_diag_messages(src);
    // Only the call-site arity/type issue is acceptable — NOT a parse error
    // about the array size. There must be no parse diagnostic at all here.
    assert!(
        msgs.is_empty(),
        "array size with separators must parse, got: {:?}",
        msgs
    );
}

#[test]
fn audit2_pm_flow_annotations_accept_digit_separators() {
    let src = r#"
type E { code: i32 }
flow F @mailbox(depth=2_048) @max_children(1_0) {
    state Idle
    fault E
}
func main() -> i32 { 0 }
"#;
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.is_empty(),
        "@mailbox/@max_children with separators must parse, got: {:?}",
        msgs
    );
}

// ═══════════════════════════════════════════════════════════════
// P-4 — range precedence relative to comparisons
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_range_binds_tighter_than_equality() {
    // Old tree for `1..2 == 2..3`: ((1..2)==2)..3 — a range over a bool.
    // New tree: (1..2) == (2..3).
    let src = "1..2 == 2..3";
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let expr = crate::parser::Parser::new(tokens)
        .parse_expr(0)
        .expect("parse range comparison");
    let crate::ast::Expr::Binary(crate::ast::BinOp::EqCmp, lhs, rhs) = expr.unlocated() else {
        panic!("top of `1..2 == 2..3` must be ==, got: {:?}", expr);
    };
    assert!(
        matches!(
            lhs.unlocated(),
            crate::ast::Expr::Binary(crate::ast::BinOp::Range, ..)
        ),
        "LHS must be the range 1..2, got: {:?}",
        lhs
    );
    assert!(
        matches!(
            rhs.unlocated(),
            crate::ast::Expr::Binary(crate::ast::BinOp::Range, ..)
        ),
        "RHS must be the range 2..3, got: {:?}",
        rhs
    );
}

#[test]
fn audit2_pm_range_rhs_extends_past_comparison() {
    // `x == 1..2` must read `x == (1..2)`, not `(x==1)..2`.
    let src = "x == 1..2";
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let expr = crate::parser::Parser::new(tokens)
        .parse_expr(0)
        .expect("parse comparison with range rhs");
    let crate::ast::Expr::Binary(crate::ast::BinOp::EqCmp, _lhs, rhs) = expr.unlocated() else {
        panic!("top of `x == 1..2` must be ==, got: {:?}", expr);
    };
    assert!(
        matches!(
            rhs.unlocated(),
            crate::ast::Expr::Binary(crate::ast::BinOp::Range, ..)
        ),
        "RHS must be the range 1..2, got: {:?}",
        rhs
    );
}

#[test]
fn audit2_pm_slice_syntax_unaffected_by_range_precedence() {
    // Slice bounds parse through parse_expr_without_range; the precedence
    // change must not disturb `a[1..2]` / `a[1..]` / `a[..2]`.
    let src = "func main() -> i32 { let a = [1, 2, 3]\n let s = a[1..2]\n len(s) }";
    let msgs = parse_diag_messages(src);
    assert!(msgs.is_empty(), "slice syntax must still parse: {:?}", msgs);
}

// ═══════════════════════════════════════════════════════════════
// P-5 — Span::contains is half-open like every other end position
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_span_contains_is_half_open_with_point_exception() {
    let span = crate::span::Span::new(1, 1, 1, 4); // covers cols 1..4 (1,2,3)
    assert!(span.contains(1, 1), "start col is inclusive");
    assert!(span.contains(1, 3), "last covered col");
    assert!(!span.contains(1, 4), "end col is exclusive (half-open)");
    assert!(!span.contains(1, 5));
    // Zero-width point spans contain exactly their point.
    let point = crate::span::Span::single(2, 7);
    assert!(point.contains(2, 7), "point span contains its own point");
    assert!(!point.contains(2, 8));
    // Multi-line: end column still exclusive on the final line.
    let multi = crate::span::Span::new(1, 3, 3, 5);
    assert!(multi.contains(2, 1), "middle line fully inside");
    assert!(multi.contains(3, 4));
    assert!(!multi.contains(3, 5), "end col exclusive on last line");
}

// ═══════════════════════════════════════════════════════════════
// P-6 — leading UTF-8 BOM is skipped
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_leading_bom_is_skipped() {
    let src = "\u{FEFF}func main() -> i32 { 0 }";
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.is_empty(),
        "a file starting with U+FEFF must lex/parse cleanly, got: {:?}",
        msgs
    );
}

#[test]
fn audit2_pm_mid_file_bom_still_rejected() {
    // Only position-0 BOMs are skipped; a U+FEFF inside the file is not
    // whitespace and must stay an error.
    let src = "func main() -> i32 { let x = \u{FEFF}1\n 0 }";
    let res = crate::lexer::Lexer::new(src).tokenize();
    assert!(
        res.is_err(),
        "mid-file U+FEFF must remain a lex error, got tokens: {:?}",
        res.map(|t| t.len())
    );
}

// ═══════════════════════════════════════════════════════════════
// P-7 — actor fields accept soft-keyword names
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_actor_soft_keyword_field_names_parse() {
    let src = r#"
actor Counter {
    view: i32
    end: i32
}
func main() -> i32 { 0 }
"#;
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.is_empty(),
        "soft-keyword actor field names must parse, got: {:?}",
        msgs
    );
}

// ═══════════════════════════════════════════════════════════════
// P-8 — attributes and `pub` in either order
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_attribute_before_pub_parses() {
    // Choice documented at parse_item_kind: BOTH orders accepted.
    let attr_first = "#[derive(Debug)]\npub type P8Rec { a: i32 }";
    let pub_first = "pub #[derive(Debug)] type P8Rec { a: i32 }";
    for src in [attr_first, pub_first] {
        let msgs = parse_diag_messages(src);
        assert!(msgs.is_empty(), "`{}` must parse, got: {:?}", src, msgs);
    }
}

// ═══════════════════════════════════════════════════════════════
// P-9 — radix prefixes reject prefix-adjacent separators
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_radix_prefix_adjacent_separator_rejected() {
    for bad in ["let x = 0x_1", "let x = 0b_1", "let x = 0o_1"] {
        let res = crate::lexer::Lexer::new(bad).tokenize();
        let err = res.expect_err(&format!("`{}` must be a lex error", bad));
        assert!(
            err.to_string().contains("invalid digit separator"),
            "`{}` must fail with the separator diagnostic, got: {}",
            bad,
            err
        );
    }
    // Separators BETWEEN digits stay legal in every base.
    for good in [
        "let x = 0x1_2",
        "let x = 0b1_0",
        "let x = 0o7_7",
        "let x = 1_0",
    ] {
        assert!(
            crate::lexer::Lexer::new(good).tokenize().is_ok(),
            "`{}` must still lex",
            good
        );
    }
}

// ═══════════════════════════════════════════════════════════════
// P-10 — negative literal patterns
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_negative_literal_pattern_matches() {
    let src = r#"
func main() -> i32 {
    let x = 0 - 1
    match x {
        -1 => 42
        _ => 0
    }
}
"#;
    assert_eq!(run_source_result(src), Ok(interp::Value::Int(42)));
}

#[test]
fn audit2_pm_negative_i64_min_pattern_parses() {
    // The positive magnitude of i64::MIN does not fit i64; the parser must
    // fold `-9223372036854775808` directly, exactly like parse_expr.
    let src = r#"
func pick(v: i64) -> i32 {
    match v {
        -9223372036854775808 => 1
        _ => 0
    }
}
func main() -> i32 { 0 }
"#;
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.is_empty(),
        "i64::MIN literal pattern must parse, got: {:?}",
        msgs
    );
}

// P-10 follow-up: i64::MIN is not decimal-only. Hex/binary/octal spellings of
// the same magnitude must also fold into `i64::MIN` in both expression and
// negative-pattern positions.
#[test]
fn audit2_pm_i64_min_radix_spellings_parse_and_match() {
    // Negative-pattern spellings are parser-level: integer literal patterns
    // are currently typed i32 by the checker, so an i64 subject is not needed
    // here. The parse assertion pins that each base form folds to `i64::MIN`
    // instead of overflowing.
    let pattern_src = r#"
func pick(v: i64) -> i32 {
    match v {
        -0x8000000000000000 => 1
        -0b1000000000000000000000000000000000000000000000000000000000000000 => 2
        -0o1000000000000000000000 => 3
        _ => 0
    }
}
func main() -> i32 { 0 }
"#;
    let msgs = parse_diag_messages(pattern_src);
    assert!(
        msgs.is_empty(),
        "radix i64::MIN patterns must parse, got: {:?}",
        msgs
    );

    // Expression spellings must also evaluate to the same i64::MIN value.
    let expr_src = r#"
func main() -> i64 {
    let a = -0x8000000000000000
    let b = -0b1000000000000000000000000000000000000000000000000000000000000000
    let c = -0o1000000000000000000000
    if a == -9223372036854775808 && b == a && c == a { 42 } else { 0 }
}
"#;
    assert_eq!(run_source_result(expr_src), Ok(interp::Value::Int(42)));
}

// ═══════════════════════════════════════════════════════════════
// P-11 — hard keywords rejected at expression position
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_hard_keyword_at_expr_position_is_parse_error() {
    for bad in [
        "func main() -> i32 { let x = return\n x }",
        "func main() -> i32 { let x = else\n x }",
        "func main() -> i32 { let x = module\n x }",
    ] {
        let msgs = parse_diag_messages(bad);
        assert!(
            msgs.iter()
                .any(|m| m.contains("cannot start an expression")),
            "`{}` must be a parse-time keyword error, got: {:?}",
            bad,
            msgs
        );
    }
}

#[test]
fn audit2_pm_soft_keyword_still_works_as_expression_ident() {
    // The ident-like soft keywords (view/mutate/end/...) remain valid
    // binding names and therefore valid expressions.
    let src = r#"
func main() -> i32 {
    let view = 3
    let end = 4
    view + end
}
"#;
    assert_eq!(run_source_result(src), Ok(interp::Value::Int(7)));
}

// ═══════════════════════════════════════════════════════════════
// P-12 — space-separated enum variants ruling (remains legal)
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_space_separated_enum_variants_remain_legal() {
    // RULING documented in parse_enum_variants: `type Color { Red Green }`
    // is de-facto syntax used across the suite; this nail prevents a future
    // "missing comma" hard error from regressing it silently.
    let src = "type Color { Red Green }\nfunc main() -> i32 { 0 }";
    let msgs = parse_diag_messages(src);
    assert!(
        msgs.is_empty(),
        "space-separated bare variants stay legal, got: {:?}",
        msgs
    );
}

// ═══════════════════════════════════════════════════════════════
// Stress — else-if chains parse iteratively (flat, no depth cost)
// ═══════════════════════════════════════════════════════════════

#[test]
fn audit2_pm_long_else_if_chain_parses_without_depth_cost() {
    // 1500-link `else if` chains exceed every depth cap, yet they are
    // semantically FLAT; the iterative chain parser must accept them
    // (scripts/stress-test.sh big-if-else-2000 shape). Genuine nesting is
    // still capped (deeply_nested_module/pattern tests).
    let n = 1500usize;
    let mut src = String::from("func main() -> i32 {\n    let x = 5\n");
    for i in 0..n {
        src.push_str(&format!("    if x == {} {{ {} }} else ", i, i));
    }
    src.push_str("{ -1 }\n}");
    let msgs = parse_diag_messages(&src);
    assert!(
        msgs.is_empty(),
        "a flat {}-link else-if chain must parse, got: {:?}",
        n,
        msgs
    );
}

#[test]
fn audit2_pm_else_if_chain_ast_shape_unchanged() {
    // The iterative parser must produce the SAME right-nested AST the
    // recursive form did: If(c0, t0, [If(c1, t1, [If(c2, t2, else)])]).
    let src = "if a { 1 } else if b { 2 } else { 3 }";
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let stmt = crate::parser::Parser::new(tokens)
        .parse_stmt()
        .expect("parse if chain");
    let crate::ast::Stmt::If {
        cond: c0,
        then_,
        else_,
    } = stmt.unlocated()
    else {
        panic!("outer must be If, got: {:?}", stmt);
    };
    assert!(matches!(c0.unlocated(), crate::ast::Expr::Ident(name) if name == "a"));
    assert_eq!(then_.len(), 1);
    let else_block = else_.as_ref().expect("else branch");
    assert_eq!(else_block.len(), 1, "else holds exactly the elif statement");
    let crate::ast::Stmt::If {
        cond: c1,
        else_: else2,
        ..
    } = else_block[0].unlocated()
    else {
        panic!("elif must be If, got: {:?}", else_block[0]);
    };
    assert!(matches!(c1.unlocated(), crate::ast::Expr::Ident(name) if name == "b"));
    let else_block2 = else2.as_ref().expect("elif else branch");
    assert_eq!(else_block2.len(), 1);
    assert!(
        matches!(else_block2[0].unlocated(), crate::ast::Stmt::Expr(_)),
        "final else is an expression statement, got: {:?}",
        else_block2[0]
    );
}

#[test]
fn audit2_pm_else_if_expr_chain_runs_correctly() {
    // End-to-end: expression-position else-if chain evaluates correctly.
    let src = r#"
func main() -> i32 {
    let x = 2
    let r = if x == 1 { 10 } else if x == 2 { 20 } else if x == 3 { 30 } else { 40 }
    r
}
"#;
    assert_eq!(run_source_result(src), Ok(interp::Value::Int(20)));
}

#[test]
fn audit_fix_parser_optional_chain_dot_assoc_left_to_right() {
    // Locks standard optional-chaining precedence: `a?.b.c` parses as
    // `(a?.b).c` == Field(OptionalChain(a, "b"), "c"), NOT
    // OptionalChain(Field(a, "b"), "c"). The latter would make `.c` part of
    // the optional chain (non-standard; matches JS/TS/C#/Swift). The audit
    // F-01 claim that the current tree is "误解析" is itself a misjudgment
    // (same class as the §0 ACT-F1 / RT-H2 overturns).
    let src = "a?.b.c";
    let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
    let expr = crate::parser::Parser::new(tokens)
        .parse_expr(0)
        .expect("parse a?.b.c");
    match expr.unlocated() {
        crate::ast::Expr::Field(inner, name) => {
            assert_eq!(name, "c", "outer accessor must be `c`");
            match inner.unlocated() {
                crate::ast::Expr::OptionalChain(base, oname) => {
                    assert_eq!(oname, "b", "optional chain field must be `b`");
                    assert!(
                        matches!(base.unlocated(), crate::ast::Expr::Ident(x) if x == "a"),
                        "optional chain base must be `a`, got: {:?}",
                        base
                    );
                }
                other => panic!(
                    "inner of a?.b.c must be OptionalChain(a,b), got: {:?}",
                    other
                ),
            }
        }
        other => panic!(
            "a?.b.c must be Field(OptionalChain(a,b), c), got: {:?}",
            other
        ),
    }
}
