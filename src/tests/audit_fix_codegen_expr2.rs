//! Wave-1 audit-fix regression tests — codegen_expr2.
//! Findings: devdocs/full-audit-2026-08-05.md §7 (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via
//! compile_and_run) — same discipline as the `dual_assert!` macro in
//! src/tests/dual_backend.rs (checker gate as applicable, both backends
//! asserted against the expected output and against each other).
//!
//! Fixes covered:
//! - §7 CRITICAL match.rs array/slice pattern length check + subject test
//! - §7 CRITICAL match.rs slice `..rest` actual remainder binding
//! - §7 HIGH     record.rs declared-order field GEP
//! - §7 HIGH     record.rs tuple_type_stack symmetric push/pop
//! - §7 MEDIUM   literal.rs f-string length-based assembly + float Display
use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

/// Run BOTH backends and assert the trimmed outputs equal `expected` and
/// each other (mirrors `dual_assert!`).
fn run_and_compare(src: &str, expected: &str) {
    let interp_run = std::panic::catch_unwind(|| run_source_with_stdout(src));
    assert!(
        interp_run.is_ok(),
        "bytecode VM panicked for audit_fix_codegen_expr2 source"
    );
    let (_vm_val, vm_out) = interp_run.unwrap();
    let cg_out = compile_and_run(src).expect("codegen failed");
    assert_eq!(
        cg_out.trim(),
        expected,
        "codegen mismatch\ncodegen: {}\nexpected: {}",
        cg_out.trim(),
        expected
    );
    assert_eq!(
        vm_out.trim(),
        expected,
        "bytecode VM stdout mismatch\nvm: {}\nexpected: {}",
        vm_out.trim(),
        expected
    );
    assert_eq!(
        vm_out.trim(),
        cg_out.trim(),
        "dual-backend stdout diverge\nvm: {}\ncodegen: {}",
        vm_out.trim(),
        cg_out.trim()
    );
}

/// Hard checker gate (mirrors `dual_assert!`).
fn dual_expected(src: &str, expected: &str) {
    check_source(src).unwrap_or_else(|diags| {
        panic!(
            "checker rejected audit_fix_codegen_expr2 source:\n{}",
            diags
                .iter()
                .map(|d| format!("{}", d))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    run_and_compare(src, expected);
}

/// Soft checker gate — for sources that intentionally exercise behavior the
/// checker rejects (each call site carries a CHECKER-GAP comment, per
/// 0.31.29 止血线 §7 discipline).
fn dual_expected_soft(src: &str, expected: &str) {
    let _ = check_source(src);
    run_and_compare(src, expected);
}

// ══════════════════════════════════════════════════════════════
// Fix 1 — match.rs array/slice patterns: length test + subject test
// (VM reference: interp/bytecode/compiler.rs:3830-3879 EqInt len == N,
// 3928-3977 GeInt len >= N; element tests guarded by the length check).
// ══════════════════════════════════════════════════════════════

#[test]
fn cg_expr2_array_pat_no_over_match() {
    if !can_link() {
        return;
    }
    // [1,2,3] vs pattern [1, 2]: VM requires exact length → fallthrough.
    // Pre-fix codegen compared only prefix elements → over-matched arm 1.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1, 2, 3] {
                [1, 2] => 1
                _ => 2
            }
            println(r)
            0
        }
    "#,
        "2",
    );
}

#[test]
fn cg_expr2_array_pat_short_subject() {
    if !can_link() {
        return;
    }
    // Subject shorter than pattern: VM len 1 != 2 → fallthrough. Pre-fix
    // codegen loaded data[1] OOB before comparing → UB read.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1] {
                [1, 2] => 1
                _ => 2
            }
            println(r)
            0
        }
    "#,
        "2",
    );
}

#[test]
fn cg_expr2_array_pat_exact_match_ok() {
    if !can_link() {
        return;
    }
    // Sanity: exact-length match still works after the fix.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1, 2] {
                [1, 2] => 10
                _ => 20
            }
            println(r)
            0
        }
    "#,
        "10",
    );
}

#[test]
fn cg_expr2_array_pat_empty_matches_empty() {
    if !can_link() {
        return;
    }
    // Empty pattern vs empty list: len 0 == 0 → matches (both backends).
    dual_expected(
        r#"
        func main() -> i32 {
            let xs: List<i64> = []
            let r = match xs {
                [] => 10
                _ => 20
            }
            println(r)
            0
        }
    "#,
        "10",
    );
}

#[test]
fn cg_expr2_array_pat_empty_rejects_nonempty() {
    if !can_link() {
        return;
    }
    // Empty pattern vs non-empty list: VM len 2 != 0 → fallthrough.
    // Pre-fix codegen: `[]` compiled to NO test → the dispatcher took the
    // unconditional br and matched everything.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1, 2] {
                [] => 10
                _ => 20
            }
            println(r)
            0
        }
    "#,
        "20",
    );
}

#[test]
fn cg_expr2_array_pat_string_subject_fallthrough() {
    if !can_link() {
        return;
    }
    // CHECKER-GAP: E0251 rejects array patterns against non-array subjects;
    // this exercises the checker-less legacy path (`compile_file`/VM run
    // without CheckedProgram). The VM treats the string subject as non-list
    // (TypeOf != "list") and falls through; pre-fix codegen GEP-ed the
    // string pointer as a list struct ({i64 len, i8* data}) — UB garbage
    // reads that could match anything.
    dual_expected_soft(
        r#"
        func main() -> i32 {
            let r = match "hi" {
                [h, i] => 1
                [] => 2
                _ => 3
            }
            println(r)
            0
        }
    "#,
        "3",
    );
}

#[test]
fn cg_expr2_array_pat_nonlist_subject_codegen_fallthrough() {
    if !can_link() {
        return;
    }
    // CHECKER-GAP: E0251 rejects array patterns against non-list subjects;
    // exercises the checker-less legacy path with an integer subject.
    //
    // Backend asymmetry is EXPECTED here and asserted explicitly:
    // - VM: the bytecode compiler emits Op::Len BEFORE the length-test jump
    //   is patched (interp/bytecode/compiler.rs:3853-3889), so Op::Len runs
    //   on the int subject and errors ("len: unsupported type") — the same
    //   trap-instead-of-fallthrough hazard audited in §9 for tuple patterns.
    // - Codegen: must fall through to `_` instead of the pre-fix
    //   unconditional match of `[]` on a non-list subject.
    let src = r#"
        func main() -> i32 {
            let r = match 5 {
                [] => 1
                [1, 2] => 2
                _ => 3
            }
            println(r)
            0
        }
    "#;
    assert!(
        run_source_result(src).is_err(),
        "bytecode VM unexpectedly accepted list patterns on a non-list subject"
    );
    let cg_out = compile_and_run(src).expect("codegen failed");
    assert_eq!(
        cg_out.trim(),
        "3",
        "codegen over-matched a list pattern on a non-list subject"
    );
}

#[test]
fn cg_expr2_slice_pat_min_len_reject() {
    if !can_link() {
        return;
    }
    // Slice pattern with prefix longer than the subject: VM GeInt 1 >= 2
    // fails → fallthrough. Pre-fix codegen delegated to the array path with
    // NO length test and read data[1] OOB.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1] {
                [1, 2, ..rest] => len(rest)
                _ => 7
            }
            println(r)
            0
        }
    "#,
        "7",
    );
}

// ══════════════════════════════════════════════════════════════
// Fix 2 — match.rs slice `..rest` binds the actual remainder
// (VM reference: __slice(subject, pats.len(), len) —
// interp/bytecode/compiler.rs:4021-4056). Pre-fix: hardcoded empty i64 0.
// ══════════════════════════════════════════════════════════════

#[test]
fn cg_expr2_slice_rest_len() {
    if !can_link() {
        return;
    }
    // Audit's dual example: match [1,2,3,4] { [a, ..rest] => len(rest) } == 3.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1, 2, 3, 4] {
                [a, ..rest] => len(rest)
                _ => 0
            }
            println(r)
            0
        }
    "#,
        "3",
    );
}

#[test]
fn cg_expr2_slice_rest_elems() {
    if !can_link() {
        return;
    }
    // Remainder must carry the actual ELEMENTS, not just the length.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [10, 20, 30] {
                [a, ..rest] => rest[0] + rest[1]
                _ => 0
            }
            println(r)
            0
        }
    "#,
        "50",
    );
}

#[test]
fn cg_expr2_slice_rest_empty() {
    if !can_link() {
        return;
    }
    // Prefix consumes the whole list: rest is an EMPTY list (len 0), not
    // garbage and not the old hardcoded 0-int.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1] {
                [a, ..rest] => a + len(rest)
                _ => 9
            }
            println(r)
            0
        }
    "#,
        "1",
    );
}

#[test]
fn cg_expr2_slice_rest_whole() {
    if !can_link() {
        return;
    }
    // `[..rest]` with empty prefix: rest is the whole subject.
    dual_expected(
        r#"
        func main() -> i32 {
            let r = match [1, 2, 3] {
                [..rest] => len(rest)
                _ => 0
            }
            println(r)
            0
        }
    "#,
        "3",
    );
}

// ══════════════════════════════════════════════════════════════
// Fix 3 — record.rs literals store fields at DECLARED positions
// (checker validates by name, core/infer/record.rs; Resolved IR
// canonicalizes to declaration order, core/ir/lower.rs:1839-1846).
// Pre-fix: write-order GEP swapped out-of-order literals.
// ══════════════════════════════════════════════════════════════

#[test]
fn cg_expr2_record_out_of_order_two_fields() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        type Point { x: i32, y: i32 }
        func main() -> i32 {
            let p = Point { y: 4, x: 3 }
            println(p.x)
            println(p.y)
            0
        }
    "#,
        "3\n4",
    );
}

#[test]
fn cg_expr2_record_out_of_order_three_fields() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        type Rec3 { a: i64, b: i64, c: i64 }
        func main() -> i32 {
            let r = Rec3 { c: 30, a: 10, b: 20 }
            println(r.a)
            println(r.b)
            println(r.c)
            println(r.a + r.b + r.c)
            0
        }
    "#,
        "10\n20\n30\n60",
    );
}

#[test]
fn cg_expr2_record_out_of_order_mixed_types() {
    if !can_link() {
        return;
    }
    // Different-typed fields: a positional swap not only changes values but
    // crosses types (i64 slot vs string slot).
    dual_expected(
        r#"
        type Mixed { n: i64, s: string }
        func main() -> i32 {
            let m = Mixed { s: "hi", n: 7 }
            println(m.s)
            println(m.n)
            0
        }
    "#,
        "hi\n7",
    );
}

// ══════════════════════════════════════════════════════════════
// Fix 4 — record.rs tuple_type_stack symmetric push/pop
// (stale-layout hazard for .last() consumers). These tests pin that the
// pop does NOT break literal/index/match/destructuring flows.
// ══════════════════════════════════════════════════════════════

#[test]
fn cg_expr2_tuple_stack_symmetry_basic() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let t = (1, 2)
            println(t.0)
            println(t.1)
            let r = match t {
                (1, 2) => 20
                _ => 30
            }
            println(r)
            0
        }
    "#,
        "1\n2\n20",
    );
}

#[test]
fn cg_expr2_tuple_stack_two_widths() {
    if !can_link() {
        return;
    }
    // Two tuple literals of DIFFERENT widths: with a never-popped stack the
    // top entry is the last literal compiled, so any layout lookup for an
    // earlier tuple could observe the wrong arity.
    dual_expected(
        r#"
        func main() -> i32 {
            let a = (1, 2)
            let b = (7, 8, 9)
            let ra = match a {
                (1, 2) => 100
                _ => 101
            }
            let rb = match b {
                (7, 8, 9) => 200
                _ => 201
            }
            println(ra)
            println(rb)
            println(a.0 + b.2)
            0
        }
    "#,
        "100\n200\n10",
    );
}

#[test]
fn cg_expr2_tuple_stack_destructure_after_literal() {
    if !can_link() {
        return;
    }
    // `let (x, y) = ...` destructuring after tuple literals: relies on the
    // paired push/pop around compile_pattern_bind (codegen/func.rs), not on
    // leaked literal entries.
    dual_expected(
        r#"
        func main() -> i32 {
            let (x, y) = (5, 6)
            println(x + y)
            let t = (9, 10)
            let (u, v) = t
            println(u + v)
            0
        }
    "#,
        "11\n19",
    );
}

// ══════════════════════════════════════════════════════════════
// Fix 5 — literal.rs f-string: length-based assembly (NUL survival)
// + mimi_to_string_f64 float Display (VM: Value::Float → `{}`).
// ══════════════════════════════════════════════════════════════

#[test]
fn cg_expr2_fstring_nul_preserved_len() {
    if !can_link() {
        return;
    }
    // chr(0) is a 1-byte string containing NUL. Pre-fix strcat assembly
    // stopped at the NUL (len 2); the VM's length-based ConcatStr yields
    // a 3-byte string. Assert via len(): printing raw NUL bytes is not
    // comparable (codegen's single-string println fast path uses puts).
    dual_expected(
        r#"
        func main() -> i32 {
            let s = f"a{chr(0)}b"
            println(len(s))
            0
        }
    "#,
        "3",
    );
}

#[test]
fn cg_expr2_fstring_float_shortest() {
    if !can_link() {
        return;
    }
    // %f printed "1.500000"; the VM uses Rust shortest round-trip Display.
    dual_expected(
        r#"
        func main() -> i32 {
            let s = f"x={1.5}"
            println(s)
            0
        }
    "#,
        "x=1.5",
    );
}

#[test]
fn cg_expr2_fstring_float_whole() {
    if !can_link() {
        return;
    }
    // Whole float: rust Display renders 2.0 as "2" (%f: "2.000000").
    dual_expected(
        r#"
        func main() -> i32 {
            let s = f"v={2.0}"
            println(s)
            0
        }
    "#,
        "v=2",
    );
}

#[test]
fn cg_expr2_fstring_interp_string_and_int() {
    if !can_link() {
        return;
    }
    // Regression: the memcpy rewrite must keep ordinary interpolation intact
    // (string part length from the struct len field, int via snprintf %ld).
    dual_expected(
        r#"
        func main() -> i32 {
            let name = "World"
            let n = 42
            let s = f"Hello, {name}! n={n}"
            println(s)
            println(len(s))
            0
        }
    "#,
        // "Hello, " (7) + "World" (5) + "! n=" (4) + "42" (2) = 18 bytes.
        "Hello, World! n=42\n18",
    );
}
