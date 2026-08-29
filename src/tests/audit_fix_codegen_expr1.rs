//! Wave-1 audit-fix regression tests — codegen_expr1.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! Coverage:
//! - §7 CRITICAL `and`/`or` eager evaluation → short-circuit (operator.rs)
//! - §7 HIGH float `**` bypasses SD-9 finiteness (operator.rs)
//! - §7 HIGH float→int bare fptosi poison (expr.rs)
//! - §7 HIGH values(record) fptoui on float fields (call/helpers.rs)
//! - §7 MEDIUM `?` on Field resolves Result as Option (try_expr.rs)
//! - §7 MEDIUM Deref pointee guessing (operator.rs)
use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

/// L1 dual assertion (mirrors dual_backend!dual_assert): checker gates the
/// source, then VM stdout and codegen stdout must both equal `expected`.
macro_rules! dual_eq {
    ($src:expr, $expected:expr) => {{
        check_source($src).unwrap_or_else(|diags| {
            panic!(
                "checker rejected dual_eq source:\n{}",
                diags
                    .iter()
                    .map(|d| format!("{}", d))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
        let (_vm_val, vm_stdout) = run_source_with_stdout($src);
        let cg_stdout = compile_and_run($src).expect("codegen failed");
        assert_eq!(
            cg_stdout.trim(),
            $expected,
            "codegen mismatch\ncodegen: {}\nexpected: {}",
            cg_stdout.trim(),
            $expected
        );
        assert_eq!(
            vm_stdout.trim(),
            $expected,
            "VM stdout mismatch\nvm: {}\nexpected: {}",
            vm_stdout.trim(),
            $expected
        );
    }};
}

// ============================================================
// Fix 1 (§7 CRITICAL): `and`/`or` must short-circuit — the VM
// does (compile_short_circuit); eager codegen evaluated BOTH
// sides and core-dumped on the skipped div-by-zero.
// ============================================================

#[test]
fn audit_expr1_and_short_circuits_div_zero_never_runs() {
    if !can_link() {
        return;
    }
    // `boom()` divides by zero: it must NEVER execute when the LHS is
    // false. Pre-fix codegen evaluated both sides → E0801 trap/core dump
    // while the VM printed "short" (VERIFIED#1 in the audit).
    dual_eq!(
        r#"
        func boom() -> bool {
            let z = 0
            let w = 1 / z
            w > 0
        }
        func main() -> i32 {
            if false and boom() { println("boom") } else { println("short") }
            0
        }
    "#,
        "short"
    );
}

#[test]
fn audit_expr1_or_short_circuits_div_zero_never_runs() {
    if !can_link() {
        return;
    }
    dual_eq!(
        r#"
        func boom() -> bool {
            let z = 0
            let w = 1 / z
            w > 0
        }
        func main() -> i32 {
            if true or boom() { println("short") } else { println("boom") }
            0
        }
    "#,
        "short"
    );
}

#[test]
fn audit_expr1_logical_truth_table_dual() {
    if !can_link() {
        return;
    }
    dual_eq!(
        r#"
        func main() -> i32 {
            println(true and true)
            println(true and false)
            println(false and true)
            println(false and false)
            println(true or true)
            println(true or false)
            println(false or true)
            println(false or false)
            0
        }
    "#,
        "true\nfalse\nfalse\nfalse\ntrue\ntrue\ntrue\nfalse"
    );
}

#[test]
fn audit_expr1_logical_value_flow_dual() {
    if !can_link() {
        return;
    }
    // Non-literal operands: the RHS is evaluated exactly when needed and
    // its value flows through the merge phi.
    dual_eq!(
        r#"
        func is_pos(x: i64) -> bool { x > 0 }
        func main() -> i32 {
            let a = is_pos(3) and is_pos(4)
            let b = is_pos(-1) or is_pos(2)
            let c = is_pos(-1) and is_pos(2)
            let d = is_pos(3) or is_pos(-2)
            println(a)
            println(b)
            println(c)
            println(d)
            0
        }
    "#,
        "true\ntrue\nfalse\ntrue"
    );
}

// ============================================================
// Fix 2 (§7 HIGH): float `**` bypassed the SD-9 finiteness
// guard — (-1.0)**0.5 silently produced NaN instead of E0813.
// ============================================================

#[test]
fn audit_expr1_pow_float_nonfinite_traps_e0813_both_backends() {
    if !can_link() {
        return;
    }
    let src = r#"
        func main() -> i32 {
            let x = -1.0
            let y = x ** 0.5
            println(y)
            0
        }
    "#;
    check_source(src).expect("pow source must type-check");
    let vm_err =
        run_source_bytecode_result(src).expect_err("VM: (-1.0)**0.5 is NaN and must trap E0813");
    assert!(
        vm_err.contains("E0813") || vm_err.contains("invalid floating-point"),
        "VM pow trap message: {}",
        vm_err
    );
    let cg_err = compile_and_run(src).expect_err("codegen: (-1.0)**0.5 must trap E0813");
    assert!(
        cg_err.contains("E0813") || cg_err.contains("NaN/Inf"),
        "codegen pow trap message: {}",
        cg_err
    );
}

#[test]
fn audit_expr1_pow_float_inside_ieee_block_codegen_does_not_trap() {
    if !can_link() {
        return;
    }
    // SD-9: inside `ieee_float { }` the finiteness invariant is suspended,
    // so `**` must NOT trap — the NaN result is a legitimate IEEE value.
    //
    // NOTE (L1 asymmetry, out of this group's ownership): the bytecode VM's
    // Op::PowFloat (vm.rs:1146-1151) hard-traps NaN WITHOUT honoring the
    // per-frame ieee_depth — the same bug class the audit flagged for the
    // `*Int` float fallback (audit §9, vm.rs:570-696). Codegen follows the
    // SD-9 ruling here; the VM-side fix is a Wave-2 follow-up for the VM
    // group, so this test asserts the codegen side only.
    let src = r#"
        func main() -> i32 {
            ieee_float {
                let y = (-1.0) ** 0.5
                println("reached")
            }
            0
        }
    "#;
    check_source(src).expect("ieee pow source must type-check");
    let stdout = compile_and_run(src)
        .expect("codegen: pow inside ieee_float { } must not trap (SD-9 suspended)");
    assert_eq!(stdout.trim(), "reached");
}

// ============================================================
// Fix 4 (§7 HIGH): float→int casts used bare fptosi (poison on
// NaN/overflow). The VM uses Rust `as` (vm.rs Op::Cast):
// saturate to MIN/MAX, NaN → 0.
// ============================================================

#[test]
fn audit_expr1_float_to_int_cast_saturates_dual() {
    if !can_link() {
        return;
    }
    dual_eq!(
        r#"
        func main() -> i32 {
            let big = 1.0e100
            println(big as i64)
            let small = -1.0e100
            println(small as i64)
            println((2.5) as i64)
            println((-2.5) as i64)
            println((3.0e9) as i32)
            println((-3.0e9) as i32)
            0
        }
    "#,
        "9223372036854775807\n-9223372036854775808\n2\n-2\n2147483647\n-2147483648"
    );
}

#[test]
fn audit_expr1_float_nan_cast_to_zero_dual() {
    if !can_link() {
        return;
    }
    // NaN produced legitimately inside ieee_float { } and then cast: the VM
    // (Rust `as`) maps NaN → 0; codegen must match instead of poisoning.
    dual_eq!(
        r#"
        func main() -> i32 {
            ieee_float {
                let z = 0.0
                let n = z / z
                println(n as i64)
            }
            0
        }
    "#,
        "0"
    );
}

// ============================================================
// Fix 5 (§7 MEDIUM): `?` on a non-Ident/non-Call inner (record
// FIELD) defaulted to the Option layout — a 3-field Result was
// misread and the Err path passed the OK slot to mimi_try_exit.
// ============================================================

#[test]
fn audit_expr1_try_on_result_field_ok_payload_dual() {
    if !can_link() {
        return;
    }
    dual_eq!(
        r#"
        type Wallet { res: Result<i64, i64> }
        func make_wallet() -> Wallet {
            Wallet { res: Ok(9) }
        }
        func main() -> i32 {
            let w = make_wallet()
            let v = w.res?
            println(v)
            0
        }
    "#,
        "9"
    );
}

#[test]
fn audit_expr1_try_on_result_field_err_exits_with_err_slot() {
    if !can_link() {
        return;
    }
    // Err path: the propagated error value must be the ERR slot (77), not
    // the OK slot. Pre-fix codegen loaded the field through the 2-field
    // Option layout and exited with the ok-slot payload.
    let src = r#"
        type Wallet { res: Result<i64, i64> }
        func make_wallet() -> Wallet {
            Wallet { res: Err(77) }
        }
        func main() -> i64 {
            let w = make_wallet()
            let v = w.res?
            v
        }
    "#;
    check_source(src).expect("field-try source must type-check");
    // VM: `?` propagates the Err variant as main's value (RetEarly).
    let vm_val = run_source_with_stdout(src).0;
    match &vm_val {
        crate::interp::Value::Variant(name, payload) => {
            assert_eq!(name, "Err", "VM must propagate the Err variant");
            assert_eq!(payload.len(), 1, "Err payload arity");
            match &payload[0] {
                crate::interp::Value::Int(e) => {
                    assert_eq!(*e, 77, "VM must propagate the ERR payload, not the ok slot")
                }
                other => panic!("expected Int err payload, got {}", other),
            }
        }
        other => panic!("expected Err variant from VM, got {}", other),
    }
    // Codegen: `?` outside a fails-transition exits the process with the
    // propagated error — the exit message must carry the ERR slot (77).
    let cg_err =
        compile_and_run(src).expect_err("codegen: `?` on Err must exit non-zero via mimi_try_exit");
    assert!(
        cg_err.contains("Result::Err(77)"),
        "codegen must exit with the ERR slot payload, got: {}",
        cg_err
    );
}

// ============================================================
// Fix 6 (§7 HIGH): values(record) converted float fields with
// fptoui (poison for negatives). The VM keeps field Values as-is
// (heterogeneous list); codegen's i64-slot ABI stores floats by
// the bitcast convention shared with List<f64> literals.
// ============================================================

// CHECKER-GAP: the checker does not model `values(record) -> List<T>` field
// packing, so these two tests soft-typecheck (dual_backend!dual_assert_soft
// discipline) while still asserting strict stdout equality on both backends.

/// Compare the two backends' `values()` output as multisets: the VM keeps
/// record fields in a std HashMap (vm.rs Op::NewRecord), so element ORDER is
/// nondeterministic run-to-run, while codegen emits declaration order. The
/// audit fix under test is the element ENCODING (bitcast, not fptoui).
fn sorted_lines(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.lines().map(|l| l.trim().to_string()).collect();
    v.sort();
    v
}

#[test]
fn audit_expr1_values_record_float_fields_bitcast_dual() {
    if !can_link() {
        return;
    }
    let src = r#"
        type Stats { a: f64, b: f64 }
        func main() -> i32 {
            let s = Stats { a: -1.5, b: 2.25 }
            let vs: List<f64> = values(s)
            println(vs[0])
            println(vs[1])
            0
        }
    "#;
    let _ = check_source(src); // CHECKER-GAP: values(record) typing untracked.
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src)
        .expect("codegen: values(record) with NEGATIVE float fields must not poison");
    let expected = sorted_lines("-1.5\n2.25");
    assert_eq!(
        sorted_lines(&cg_stdout),
        expected,
        "codegen values(): {}",
        cg_stdout
    );
    assert_eq!(
        sorted_lines(&vm_stdout),
        expected,
        "VM values(): {}",
        vm_stdout
    );
}

#[test]
fn audit_expr1_values_record_i32_fields_sext_dual() {
    if !can_link() {
        return;
    }
    // Narrow int fields must be sign-extended into the i64 list slot (the
    // previous direct i32 store was type-mismatched IR).
    let src = r#"
        type Pair32 { x: i32, y: i32 }
        func main() -> i32 {
            let p = Pair32 { x: -5, y: 7 }
            let vs: List<i64> = values(p)
            println(vs[0])
            println(vs[1])
            0
        }
    "#;
    let _ = check_source(src); // CHECKER-GAP: values(record) typing untracked.
    let (_vm_val, vm_stdout) = run_source_with_stdout(src);
    let cg_stdout = compile_and_run(src).expect("codegen: values(record) with i32 fields");
    let expected = sorted_lines("-5\n7");
    assert_eq!(
        sorted_lines(&cg_stdout),
        expected,
        "codegen values(): {}",
        cg_stdout
    );
    assert_eq!(
        sorted_lines(&vm_stdout),
        expected,
        "VM values(): {}",
        vm_stdout
    );
}

// ============================================================
// Fix 3 (§7 MEDIUM): Deref pointee guessing — derive from
// tracked types, fail closed on unknown, never guess.
// ============================================================

#[test]
fn audit_expr1_deref_borrow_param_derives_pointee_dual() {
    if !can_link() {
        return;
    }
    // Borrow parameters register the POINTED-TO type (func.rs); the load
    // must be derived from it (i32 here — an i64 guess would over-read).
    dual_eq!(
        r#"
        func read(p: &i32) -> i32 { *p }
        func main() -> i32 {
            let x = 41 as i32
            println(read(&x))
            0
        }
    "#,
        "41"
    );
}

#[test]
fn audit_expr1_deref_list_slot_borrow_dual() {
    if !can_link() {
        return;
    }
    // Let-bound borrow of a list element slot (i64-width): the corpus
    // convention preserved by the fail-closed rewrite
    // (tests/real_world/ownership_cfg.mimi borrow_indexes_sequentially).
    dual_eq!(
        r#"
        func main() -> i32 {
            let mut values = [10, 20, 30]
            let first = &values[0]
            let got = *first
            println(got)
            0
        }
    "#,
        "10"
    );
}

#[test]
fn audit_expr1_deref_unknown_pointee_fails_closed() {
    if !can_link() {
        return;
    }
    // Deref of a NON-Ident inner expression has no derivable pointee type:
    // codegen must fail closed instead of guessing an i64 load.
    let src = r#"
        func main() -> i32 {
            let a = 1
            let b = 2
            println(*(if true { &a } else { &b }))
            0
        }
    "#;
    check_source(src).expect("checker must accept deref of an if-expr borrow");
    let cg_err =
        compile_and_run(src).expect_err("codegen must fail closed on underivable pointee types");
    assert!(
        cg_err.contains("unsupported in codegen") || cg_err.contains("pointee"),
        "fail-closed deref message: {}",
        cg_err
    );
}

/// 0.40.1.3 (A3, `devdocs/v0.40/blind-spots-evaluation-2026-08-29.md` §1.3-3/4):
/// the native (LLVM) backend must fail closed with E0723 when a function
/// returns a heap-owning aggregate whose ownership cannot be transferred
/// across the return boundary — the BUG P silent pass-through hole in the
/// legacy `func.rs` `deep_copy_returned_value` / `type_owns_heap` path.
///
/// Concrete nested non-string lists (`List<List<i32>>`) are one such hole: the
/// outer list's data array is claimed but the inner list's is not, aliasing
/// freed heap. The VM backend (`mimi run`) is unaffected and remains supported.
#[test]
fn audit_expr1_native_heap_aggregate_return_fails_closed_e0723() {
    let src = r#"
        func nested() -> List<List<i32>> {
            [[1, 2], [3, 4]]
        }
        func main() -> i32 {
            let _ = nested();
            0
        }
    "#;
    check_source(src).expect("checker must accept the nested-list return");

    // Native (LLVM) codegen must fail closed.
    let cg_err = checked_codegen_compile_and_run(src)
        .expect_err("native codegen must fail closed (E0723) on heap-owning aggregate returns");
    assert!(
        cg_err.contains("E0723"),
        "fail-closed heap-return message must cite E0723: {}",
        cg_err
    );

    // The VM backend (`mimi run`) must still accept and run the same program —
    // the fail-closed is native-only, never a language-level rejection.
    let (_val, vm_out) = checked_run_source_with_stdout(src);
    assert_eq!(vm_out, "", "VM path must run the heap-aggregate return");
}
