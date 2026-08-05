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
    // Wave-2 C-group fix: fields keep the declared i64 layout (the test pins
    // declared-order GEP at 64-bit width); pin the literals to i64 explicitly
    // (record fields unify strictly, no literal widening → E0247 otherwise).
    dual_expected(
        r#"
        type Rec3 { a: i64, b: i64, c: i64 }
        func main() -> i32 {
            let r = Rec3 { c: 30 as i64, a: 10 as i64, b: 20 as i64 }
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
    // Wave-2 C-group fix: pin the i64 field literal explicitly (strict field
    // unification; literals infer i32).
    dual_expected(
        r#"
        type Mixed { n: i64, s: string }
        func main() -> i32 {
            let m = Mixed { s: "hi", n: 7 as i64 }
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

// ══════════════════════════════════════════════════════════════
// Wave-2 CGCORE (full-audit-2026-08-05-0656.md §3.6 + §2.6)
// K-1 operator.rs div/mod MIN constant; K-2 match.rs literal-arm
// width; H-22 control.rs if-expr i64→i1 normalize; control.rs
// list/string slice VM-parity (clamp→trap, negative wrap, string
// char-slice, copy-not-alias).
// ══════════════════════════════════════════════════════════════

/// K-1 regression: the rewritten MIN-constant construction must not change
/// behavior for the reachable widths (≤ i64). i64::MIN / -1 still traps on
/// both backends (the SD-8 guard is intact after switching the constant to
/// LLVM-domain const_shl).
#[test]
fn audit2_cgc_div_min_neg1_still_traps_i64() {
    if !can_link() {
        return;
    }
    // Construct i64::MIN without an overflowing literal, then divide by -1.
    let src = r#"
        func main() -> i32 {
            let m: i64 = 0 - 9223372036854775807 - 1
            println(m / -1)
            0
        }
    "#;
    let vm = std::panic::catch_unwind(|| run_source(src));
    assert!(vm.is_err(), "bytecode VM must trap on i64 MIN / -1");
    let cg = compile_and_run(src).expect_err("codegen must trap MIN / -1, not emit poison sdiv");
    assert!(
        cg.contains("overflow") || cg.contains("E0802"),
        "codegen error must surface the MIN/-1 overflow trap, got: {}",
        cg
    );
}

/// K-1 companion: div-by-zero and MIN/-1 guards for the modulo operator too.
#[test]
fn audit2_cgc_mod_min_neg1_still_traps_i64() {
    if !can_link() {
        return;
    }
    let src = r#"
        func main() -> i32 {
            let m: i64 = 0 - 9223372036854775807 - 1
            println(m % -1)
            0
        }
    "#;
    let vm = std::panic::catch_unwind(|| run_source(src));
    assert!(vm.is_err(), "bytecode VM must trap on i64 MIN % -1");
    let cg = compile_and_run(src).expect_err("codegen must trap MIN % -1, not emit poison srem");
    assert!(
        cg.contains("overflow") || cg.contains("E0802"),
        "codegen error must surface the MIN/-1 overflow trap, got: {}",
        cg
    );
}

/// K-1 companion: ordinary division still folds correctly for the common
/// widths (the constant-construction rewrite must be value-identical).
#[test]
fn audit2_cgc_div_mod_still_correct_dual() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let a: i64 = 0 - 8
            println(a / 2)
            println(a % 3)
            println(7 / 2)
            println(7 % 3)
            0
        }
    "#,
        "-4\n-2\n3\n1",
    );
}

/// K-2 regression: matching a bool (i1) scrutinee against literal arms must
/// not emit `icmp i1, i32` (invalid IR → ICE). The arm constants now take the
/// scrutinee's own width.
#[test]
fn audit2_cgc_match_bool_scrutinee_literal_arms() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let b = true
            let r = match b {
                true => 1
                false => 0
            }
            println(r)
            let c = false
            let s = match c {
                true => 1
                false => 0
            }
            println(s)
            0
        }
    "#,
        "1\n0",
    );
}

/// H-22 regression: an if-EXPRESSION whose condition is a builtin i64
/// 0/1 predicate must normalize to i1 before `br` (previously `br i64`
/// = invalid IR). Dual-backend: both sides pick the same branch.
#[test]
fn audit2_cgc_if_expr_i64_predicate_normalized() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let s = "banana"
            let hit = if str_contains(s, "nan") { 1 } else { 2 }
            println(hit)
            let miss = if str_contains(s, "xyz") { 1 } else { 2 }
            println(miss)
            0
        }
    "#,
        "1\n2",
    );
}

// ── Slice VM-parity (control.rs compile_slice_expr rewrite) ─────

/// Slice axis 1+4: in-bounds slice returns a COPY (mutating the source after
/// slicing must not show through) and the element values are correct. Old
/// codegen aliased the source buffer (s[0] would read 99 after xs[1]=99).
#[test]
fn audit2_cgc_list_slice_is_a_copy_dual() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let xs = [10, 20, 30, 40]
            let s = xs[1..3]
            xs[1] = 99
            println(len(s))
            println(s[0])
            println(s[1])
            println(xs[1])
            0
        }
    "#,
        "2\n20\n30\n99",
    );
}

/// Slice axis 2: negative indices wrap Python-style (idx < 0 → len + idx).
/// Old codegen clamped negatives to 0.
#[test]
fn audit2_cgc_list_slice_negative_wrap_dual() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let xs = [10, 20, 30, 40]
            let s = xs[-2..]
            println(len(s))
            println(s[0])
            println(s[1])
            let t = xs[..-1]
            println(len(t))
            println(t[3 - 1])
            0
        }
    "#,
        "2\n30\n40\n3\n30",
    );
}

/// Slice axis 3 (string leg): strings are sliced by CHARACTER index and the
/// result is a new string. Multibyte input proves char- (not byte-) indexing.
#[test]
fn audit2_cgc_string_slice_char_based_dual() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let s = "hello"
            let t = s[1..4]
            println(t)
            println(len(t))
            let u = "你好世界"
            let v = u[1..3]
            println(v)
            println(len(v))
            0
        }
    "#,
        "ell\n3\n好世\n2",
    );
}

/// String slice negative-index wrap (VM parity: chars, negatives resolve
/// against the char length).
#[test]
fn audit2_cgc_string_slice_negative_wrap_dual() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let s = "hello"
            let t = s[-3..]
            println(t)
            0
        }
    "#,
        "llo",
    );
}

/// Slice axis 1 (error leg): end beyond len must TRAP (VM E0814), not clamp.
/// Both backends fail.
#[test]
fn audit2_cgc_list_slice_end_oob_traps_dual() {
    if !can_link() {
        return;
    }
    let src = r#"
        func main() -> i32 {
            let xs = [1, 2, 3]
            let s = xs[1..10]
            println(len(s))
            0
        }
    "#;
    let vm = run_source_result(src);
    assert!(vm.is_err(), "VM must trap on slice end out of bounds");
    assert!(
        vm.unwrap_err().contains("slice"),
        "VM error must mention slice bounds"
    );
    let cg = compile_and_run(src).expect_err("codegen must trap slice end OOB, not clamp");
    assert!(
        cg.contains("slice"),
        "codegen abort must mention slice bounds, got: {}",
        cg
    );
}

/// Slice axis 1 (error leg): start > end must TRAP (VM E0814), not yield empty.
#[test]
fn audit2_cgc_list_slice_start_gt_end_traps_dual() {
    if !can_link() {
        return;
    }
    let src = r#"
        func main() -> i32 {
            let xs = [1, 2, 3]
            let s = xs[2..1]
            println(len(s))
            0
        }
    "#;
    let vm = run_source_result(src);
    assert!(vm.is_err(), "VM must trap on slice start > end");
    let cg =
        compile_and_run(src).expect_err("codegen must trap slice start > end, not yield empty");
    assert!(
        cg.contains("slice"),
        "codegen abort must mention slice bounds, got: {}",
        cg
    );
}

/// String slice OOB traps both backends (VM E0814 parity).
#[test]
fn audit2_cgc_string_slice_oob_traps_dual() {
    if !can_link() {
        return;
    }
    let src = r#"
        func main() -> i32 {
            let s = "abc"
            let t = s[0..9]
            println(t)
            0
        }
    "#;
    let vm = run_source_result(src);
    assert!(
        vm.is_err(),
        "VM must trap on string slice end out of bounds"
    );
    let cg = compile_and_run(src).expect_err("codegen must trap string slice OOB, not clamp");
    assert!(
        cg.contains("slice") || cg.contains("substring"),
        "codegen abort must mention the string slice bound, got: {}",
        cg
    );
}

/// Empty-range slice (start == end) yields an empty list on both backends.
#[test]
fn audit2_cgc_list_slice_empty_range_dual() {
    if !can_link() {
        return;
    }
    dual_expected(
        r#"
        func main() -> i32 {
            let xs = [1, 2, 3]
            let s = xs[1..1]
            println(len(s))
            0
        }
    "#,
        "0",
    );
}
