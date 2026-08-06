//! Wave-1 audit-fix regression tests — json_math.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit), §8.
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! FIX map (24-agent wave-1, agent scope: codegen/builtins/{json,math}.rs):
//! - FIX-1 [VERIFIED CRITICAL]: json NULL-pointer guards (json_get_string /
//!   from_json / json_get_element) — no puts(NULL)/strlen(NULL) UB.
//! - FIX-2 [VERIFIED CRITICAL]: json_get_int / json_array_length / json_has_key
//!   — no codegen-side sentinel-0 / partial-count fallbacks; malformed input
//!   fails loud with VM-style messages.
//! - FIX-3: to_json(float) — RFC 8259 (no "nan"/"inf"; shortest round-trip;
//!   non-finite → null mirroring serde).
//! - FIX-4 [CRITICAL]: pow int×int via runtime __mimi_pow_i64 (checked_pow
//!   semantics); float pow wrapped in the SD-9 finiteness trap.
//! - FIX-5 [CRITICAL]: abs(iN::MIN) traps (E0802) instead of saturating;
//!   MIN constant built at the operand's real width.
//! - FIX-6 [HIGH]: all math builtins enforced under SD-9 (ieee_depth-gated);
//!   log(x, base) traps on base <= 0 or base == 1 with the VM's message.
use super::*;

// Local copy of dual_backend.rs's module-local macro (macro_rules! scoping).
macro_rules! dual_assert {
    ($src:expr, $expected:expr) => {{
        check_source($src).unwrap_or_else(|diags| {
            panic!(
                "checker rejected dual_assert source:\n{}",
                diags
                    .iter()
                    .map(|d| format!("{}", d))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
        let __interp_run = std::panic::catch_unwind(|| run_source_with_stdout($src));
        assert!(
            __interp_run.is_ok(),
            "interpreter panicked for dual_assert source"
        );
        let (_interp_val, __interp_stdout) = __interp_run.unwrap();
        let __codegen = compile_and_run($src).expect("codegen failed");
        assert_eq!(
            __codegen.trim(),
            $expected,
            "codegen mismatch\ncodegen: {}\nexpected: {}",
            __codegen.trim(),
            $expected
        );
        if !__interp_stdout.trim().is_empty() || !$expected.trim().is_empty() {
            assert_eq!(
                __interp_stdout.trim(),
                $expected,
                "interpreter stdout mismatch\ninterp: {}\nexpected: {}",
                __interp_stdout.trim(),
                $expected
            );
            assert_eq!(
                __interp_stdout.trim(),
                __codegen.trim(),
                "dual-backend stdout diverge\ninterp: {}\ncodegen: {}",
                __interp_stdout.trim(),
                __codegen.trim()
            );
        }
    }};
}

fn can_link() -> bool {
    crate::tests::can_link()
}

/// Assert the bytecode VM fails loudly and the error contains `needle`.
fn assert_vm_traps(src: &str, needle: &str) {
    let vm = run_source_bytecode_result(src);
    let err = vm
        .err()
        .unwrap_or_else(|| panic!("VM must trap (expected {:?}), got Ok", needle));
    assert!(
        err.contains(needle),
        "VM error missing {:?}: {}",
        needle,
        err
    );
}

/// Assert codegen (native binary) fails loudly and stderr contains `needle`.
fn assert_codegen_traps(src: &str, needle: &str) {
    let cg = compile_and_run(src);
    let err = cg
        .err()
        .unwrap_or_else(|| panic!("codegen must trap (expected {:?}), got Ok", needle));
    assert!(
        err.contains(needle),
        "codegen stderr missing {:?}: {}",
        needle,
        err
    );
}

// ============================================================
// FIX-4: pow — int×int checked semantics, SD-9 on float pow
// ============================================================

#[test]
fn audit_pow_int_exact_small() {
    // FIX-4: small integer powers stay exact on both backends (regression
    // guard: the old codegen detoured through libc pow for ints).
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(pow(2, 10))
            0
        }
    "#,
        "1024"
    );
}

#[test]
fn audit_pow_int_exact_large_no_precision_loss() {
    // FIX-4 [CRITICAL]: 3^30 = 205891132094649 exceeds the 2^24-ish safety
    // zone of the old libc-pow detour on some libm implementations; the
    // integer path must be exact (VM checked_pow result, codegen
    // __mimi_pow_i64 → f64 round-trip; 3^30 < 2^53 so f64 is exact).
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(pow(3, 30))
            0
        }
    "#,
        "205891132094649"
    );
}

#[test]
fn audit_pow_2_60_vm_exact_and_codegen_no_trap() {
    // FIX-4: pow(2, 60) fits in i64 — must NOT trap. VM computes exact i64.
    // V-7 (closed 2026-08-07): the checker now types int×int pow as i64, so
    // BOTH backends print the exact integer (the old f64 static type made
    // codegen render a float — L1 display divergence).
    let src = r#"
        func main() -> i32 {
            let v = pow(2, 60)
            println(v)
            0
        }
    "#;
    let (_val, stdout) = run_source_bytecode_with_stdout(src);
    assert!(
        stdout.contains("1152921504606846976"),
        "VM integer pow must be exact, stdout: {:?}",
        stdout
    );
    if !can_link() {
        return;
    }
    let out = compile_and_run(src).expect("pow(2, 60) must not trap in codegen");
    assert!(
        out.contains("1152921504606846976"),
        "V-7: codegen must now render the exact integer (checker types int pow as i64), got: {:?}",
        out
    );
}

#[test]
fn audit_v7_pow_float_args_stay_f64() {
    // V-7: a float argument keeps the f64 contract (stdlib power() relies
    // on it; display shows the float rendering on both backends).
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(pow(2.0, 10.0))
            0
        }
    "#,
        "1024"
    );
}

#[test]
fn audit_pow_exp_above_u32_max_traps_both_backends() {
    // wave1-review §5.17: the VM rejects exponents above u32::MAX via
    // u32::try_from; the runtime's __mimi_pow_i64 mirrors the cap (abort).
    // Pre-fix, pow(1, 4294967326) returned 1 in codegen while the VM
    // trapped — an L1 divergence. Both sides must now fail loud.
    let src = r#"
        func main() -> i32 {
            let v = pow(1, 4294967326)
            println(v)
            0
        }
    "#;
    assert_vm_traps(src, "exponent");
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "exponent exceeds u32::MAX");
}

#[test]
fn audit_pow_int_overflow_traps_both_backends() {
    // FIX-4 [CRITICAL] + SD-7: pow(10, 19) overflows i64 — VM checked_pow
    // errors; runtime __mimi_pow_i64 aborts. Both sides must fail loud.
    let src = r#"
        func main() -> i32 {
            let v = pow(10, 19)
            println(v)
            0
        }
    "#;
    assert_vm_traps(src, "overflow");
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "overflow");
}

#[test]
fn audit_pow_negative_exponent_traps_both_backends() {
    // FIX-4: integer pow with a negative exponent is an error in both
    // backends (VM: "negative exponent not allowed for integers").
    let src = r#"
        func main() -> i32 {
            let v = pow(2, -1)
            println(v)
            0
        }
    "#;
    assert_vm_traps(src, "negative exponent");
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "negative exponent");
}

#[test]
fn audit_pow_float_nan_traps_outside_ieee_float() {
    // FIX-4 + SD-9: (-1.0)**0.5 = NaN must trap (E0813) outside ieee_float.
    let src = r#"
        func main() -> i32 {
            let v = pow(-1.0, 0.5)
            println(v)
            0
        }
    "#;
    let vm = run_source_bytecode_result(src);
    let vm_err = vm
        .err()
        .unwrap_or_else(|| panic!("VM: NaN pow must trap, got Ok"));
    assert!(
        vm_err.contains("E0813") || vm_err.contains("invalid floating-point"),
        "VM message: {}",
        vm_err
    );
    if !can_link() {
        return;
    }
    let cg = compile_and_run(src);
    let cg_err = cg
        .err()
        .unwrap_or_else(|| panic!("codegen: NaN pow must trap, got Ok"));
    assert!(
        cg_err.contains("E0813") || cg_err.contains("NaN/Inf"),
        "codegen message: {}",
        cg_err
    );
}

#[test]
fn audit_pow_float_nan_allowed_inside_ieee_float() {
    // FIX-4 + SD-9 escape hatch: inside ieee_float{} the NaN is legitimate.
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            let mut r = false
            ieee_float {
                r = is_nan(pow(-1.0, 0.5))
            }
            if r { println(1) } else { println(0) }
            0
        }
    "#,
        "1"
    );
}

// ============================================================
// FIX-5: abs(iN::MIN) traps (no saturation), width-correct MIN
// ============================================================

#[test]
fn audit_abs_plain_regression() {
    // Regression guard for the FIX-5 rewrite: normal abs values unchanged.
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(abs(-7))
            println(abs(5))
            0
        }
    "#,
        "7\n5"
    );
}

#[test]
fn audit_abs_i64_min_traps_both_backends() {
    // FIX-5 + SD-7: abs(i64::MIN) overflows — VM checked_abs errors; codegen
    // traps via the E0802 machinery (the old code saturated to i64::MAX).
    let src = r#"
        func main() -> i64 {
            let m = -9223372036854775808
            let v = abs(m)
            println(v)
            0
        }
    "#;
    assert_vm_traps(src, "abs");
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "E0802");
}

#[test]
fn audit_abs_i32_min_traps_codegen_width_enforced() {
    // FIX-5 + SD-7: codegen enforces the OPERAND WIDTH — abs(i32::MIN)
    // traps with E0802. KNOWN DIVERGENCE: the bytecode VM has no i32
    // representation (everything is i64), so it returns 2147483648 without
    // trapping. Both sides asserted explicitly per the L1-discipline for
    // width-model differences.
    //
    // Width enforcement lives on the RESOLVED codegen path (the same path the
    // `mimi build` CLI takes, via compile_checked). The legacy compile_file
    // harness widens builtin integer args to i64 before compile_abs, so its
    // MIN check compares against i64::MIN and cannot trap — that is the
    // documented legacy-vs-resolved width gap (V-6, A1 residual). Asserting
    // the trap via checked_codegen_compile_and_run pins the resolved behavior
    // that CLI builds actually ship.
    let src = r#"
        func main() -> i32 {
            let m = 0 - 2147483647 - 1
            let v = abs(m)
            println(v)
            0
        }
    "#;
    // VM side: i64 semantics — -(-2147483648) = 2147483648, no trap.
    let (_val, vm_stdout) = run_source_bytecode_with_stdout(src);
    assert!(
        vm_stdout.contains("2147483648"),
        "VM (i64 model) yields 2147483648, got {:?}",
        vm_stdout
    );
    // Codegen (resolved/checked path): i32 width enforced — SD-7 E0802 trap.
    if !can_link() {
        return;
    }
    let cg = checked_codegen_compile_and_run(src);
    let err = cg
        .err()
        .unwrap_or_else(|| panic!("codegen must trap (expected E0802), got Ok"));
    assert!(
        err.contains("E0802"),
        "codegen stderr missing E0802: {}",
        err
    );
}

// ============================================================
// FIX-6: SD-9 finiteness for math builtins + log base check
// ============================================================

#[test]
fn audit_sqrt_negative_traps_both_backends() {
    // FIX-6: sqrt(-1.0) = NaN traps outside ieee_float (VM builtin_sqrt runs
    // check_float). Makes float_finiteness_trap_active_outside_ieee_float
    // dual-backend.
    let src = r#"
        func main() -> i32 {
            let x = sqrt(-1.0)
            println(x)
            0
        }
    "#;
    let vm = run_source_bytecode_result(src);
    assert!(vm.is_err(), "VM: sqrt(-1) must trap, got {:?}", vm);
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "E0813");
}

#[test]
fn audit_sqrt_positive_regression() {
    // Regression guard for the FIX-6 rewrite: finite sqrt unaffected.
    if !can_link() {
        return;
    }
    dual_assert!("func main() -> i32 { println(to_int(sqrt(9.0))); 0 }", "3");
}

#[test]
fn audit_log_negative_traps_both_backends() {
    // FIX-6: ln(-1) = NaN → SD-9 trap on the result (VM check_float).
    let src = r#"
        func main() -> i32 {
            let x = log(-1.0)
            println(x)
            0
        }
    "#;
    let vm = run_source_bytecode_result(src);
    assert!(vm.is_err(), "VM: log(-1) must trap, got {:?}", vm);
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "E0813");
}

#[test]
fn audit_log_base_one_traps_both_backends() {
    // FIX-6 [HIGH]: log(x, 1.0) used to produce Inf silently. VM message:
    // "log: base must be positive and not 1" — codegen mirrors it verbatim.
    let src = r#"
        func main() -> i32 {
            let x = log(9.0, 1.0)
            println(x)
            0
        }
    "#;
    assert_vm_traps(src, "base must be positive");
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "base must be positive");
}

#[test]
fn audit_log_base_valid_regression() {
    // Regression guard for the FIX-6 rewrite: valid base-log unaffected.
    // (to_int() keeps the assertion robust against a last-ulp ln(9)/ln(3)
    // quotient that prints as 2.0000000000000004 on some libm builds.)
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(to_int(log(9.0, 3.0)))
            println(to_int(exp(0.0)))
            0
        }
    "#,
        "2\n1"
    );
}

// ============================================================
// FIX-3: to_json(float) — RFC 8259
// ============================================================

#[test]
fn audit_to_json_float_shortest_round_trip() {
    // FIX-3: no more "%f" padding — 1.5 serializes as "1.5" (serde shortest
    // in the VM, Rust Display in codegen). RFC 8259 §6 numbers.
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(to_json(1.5))
            println(to_json(-2.25))
            0
        }
    "#,
        "1.5\n-2.25"
    );
}

#[test]
fn audit_to_json_float_non_finite_is_null() {
    // FIX-3 + RFC 8259: JSON has no NaN/Inf literals. VM: serde from_f64 →
    // Null → "null"; codegen: explicit "null" branch. The NaN is produced
    // inside ieee_float{} so its CREATION does not trip SD-9.
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            let mut s = ""
            ieee_float {
                s = to_json(0.0 / 0.0)
            }
            println(s)
            0
        }
    "#,
        "null"
    );
}

#[test]
fn audit_to_json_int_bool_regression() {
    // Regression guard: the non-float to_json arms are untouched.
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(to_json(42))
            println(to_json(true))
            0
        }
    "#,
        "42\ntrue"
    );
}

// ============================================================
// FIX-1/FIX-2: json accessors fail loud; NULL never escapes
// ============================================================

#[test]
fn audit_json_get_string_present_key_dual() {
    // FIX-1 regression guard: the happy path is untouched by the guards.
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(json_get_string("{\"name\":\"Alice\"}", "name"))
            0
        }
    "#,
        "Alice"
    );
}

#[test]
fn audit_json_get_string_missing_key_vm_empty_string() {
    // VM reference: json_get_string with a MISSING key returns ""
    // (bytecode builtin_json_get_string: None → String::new()). The codegen
    // NULL→"" guard (build_empty_heap_string) mirrors exactly this.
    // CONTRADICTION NOTE (2026-08-05): agent H's runtime rewrite aborts
    // json_get_string on a missing key ("key 'k' not found"), which DIVERGES
    // from the VM's "" — flagged to the parent for reconciliation; the VM's
    // behavior is pinned here as the reference semantics.
    let v = run_source(r#"func main() -> string { json_get_string("{\"a\":1}", "nonexistent") }"#);
    assert_eq!(
        v,
        interp::Value::String("".into()),
        "VM json_get_string missing key must return empty string"
    );
}

#[test]
fn audit_json_get_string_malformed_traps_both_backends() {
    // FIX-1/FIX-2: malformed JSON fails loud on both backends. Codegen's
    // require_valid_json_input guard fires independently of agent H.
    let src = r#"
        func main() -> i32 {
            let s = json_get_string("{invalid", "a")
            println(s)
            0
        }
    "#;
    assert_vm_traps(src, "parse error");
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "json_get_string parse error");
}

#[test]
fn audit_json_get_int_present_key_dual() {
    // FIX-2 regression guard: the happy path is untouched.
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(json_get_int("{\"count\":42}", "count"))
            0
        }
    "#,
        "42"
    );
}

#[test]
fn audit_json_get_int_missing_key_fails_loud_both_backends() {
    // FIX-2: no sentinel-0 acceptance — missing key errors in the VM and
    // aborts in the runtime (agent H) with a VM-matching message.
    // Requires agent H's runtime fail-loud on the codegen side.
    let src = r#"
        func main() -> i64 {
            json_get_int("{\"a\":1}", "nonexistent")
        }
    "#;
    assert_vm_traps(src, "not found");
    if !can_link() {
        return;
    }
    assert_codegen_traps(src, "not found");
}

#[test]
fn audit_json_get_int_wrong_type_fails_loud_both_backends() {
    // FIX-2: non-numeric value ("is not a number") and non-integral number
    // ("is not an integer") both fail loud — VM reference, runtime abort.
    let not_number = r#"
        func main() -> i64 {
            json_get_int("{\"a\":\"x\"}", "a")
        }
    "#;
    assert_vm_traps(not_number, "not a number");
    let not_integer = r#"
        func main() -> i64 {
            json_get_int("{\"a\":1.5}", "a")
        }
    "#;
    assert_vm_traps(not_integer, "not an integer");
    if !can_link() {
        return;
    }
    // Requires agent H's runtime fail-loud (codegen passes the i64 through
    // with zero fallback; the runtime abort is the enforcement point).
    assert_codegen_traps(not_number, "not a number");
    assert_codegen_traps(not_integer, "not an integer");
}

#[test]
fn audit_json_array_length_dual_and_loud_failures() {
    // FIX-2: happy path dual; malformed input loud on both; non-array loud
    // (the latter requires agent H's runtime on the codegen side).
    // VM reference first — runs unconditionally.
    let malformed = r#"
        func main() -> i32 {
            println(json_array_length("{bad"))
            0
        }
    "#;
    assert_vm_traps(malformed, "parse error");
    let not_array = r#"
        func main() -> i32 {
            println(json_array_length("42"))
            0
        }
    "#;
    assert_vm_traps(not_array, "not an array");
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(json_array_length("[1, 2, 3]"))
            println(json_array_length("[]"))
            0
        }
    "#,
        "3\n0"
    );
    assert_codegen_traps(malformed, "json_array_length parse error");
    assert_codegen_traps(not_array, "not an array");
}

#[test]
fn audit_json_get_element_guards() {
    // FIX-1: happy path dual; malformed and out-of-bounds both fail loud on
    // BOTH backends and both are independent of agent H (codegen's validity
    // guard + NULL guard cover the pre-H runtime as well).
    // VM reference first — runs unconditionally.
    let malformed = r#"
        func main() -> i32 {
            println(json_get_element("not json", 0))
            0
        }
    "#;
    let vm = run_source_bytecode_result(malformed);
    assert!(vm.is_err(), "VM must reject malformed json");
    let oob = r#"
        func main() -> i32 {
            println(json_get_element("[10]", 5))
            0
        }
    "#;
    assert_vm_traps(oob, "out of bounds");
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(json_get_element("[10, 20, 30]", 1))
            0
        }
    "#,
        "20"
    );
    assert_codegen_traps(malformed, "json_get_element parse error");
    assert_codegen_traps(oob, "out of bounds");
}

#[test]
fn audit_from_json_null_guard_and_valid_dual() {
    // FIX-1 [VERIFIED CRITICAL]: mimi_from_json returns NULL on malformed
    // input; codegen must trap instead of passing NULL downstream. Valid
    // input round-trips on both backends (runtime returns the original
    // document text for objects/arrays).
    // VM reference first — runs unconditionally.
    let malformed = r#"
        func main() -> i32 {
            println(from_json("{invalid}"))
            0
        }
    "#;
    assert_vm_traps(malformed, "parse error");
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(from_json("{\"a\":1}"))
            0
        }
    "#,
        "{\"a\":1}"
    );
    assert_codegen_traps(malformed, "from_json parse error");
}

// ============================================================
// stdlib JSON parser 与 serde 语义统一（audit 2026-08-07）
// ============================================================
// 台账 Wave-3：runtime JsonParser 的 strict_number 只做 RFC 8259 语法扫描，
// 不查数值范围；而 bytecode VM 用 serde_json 验证。serde_json 拒绝 f64 溢出
// 的浮点字面量（"number out of range"：1e999），导致双后端分歧：
//   json_is_valid("{\"a\":1e999}") → VM false / codegen true
// 修复：strict_number 对含 '.'/'e' 的字面量补上 f64 有限性检查；大整数
// （任意精度）保持合法。前导零（01）双端本已一致拒绝。

#[test]
fn audit_json_is_valid_serde_float_range_parity() {
    // 1e999 / -1e999 溢出 f64 → 双端都必须 false（serde "number out of range"）。
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(json_is_valid("{\"a\":1e999}"))
            println(json_is_valid("{\"a\":-1e999}"))
            println(json_is_valid("[1e999]"))
            0
        }
    "#,
        "false\nfalse\nfalse"
    );
}

#[test]
fn audit_json_is_valid_serde_finite_and_bigint_parity() {
    // 有限浮点、下溢（1e-999→0.0 有限）、大整数（任意精度）双端都 true。
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(json_is_valid("{\"a\":1e-999}"))
            println(json_is_valid("{\"a\":99999999999999999999999999}"))
            println(json_is_valid("{\"a\":1.5e3}"))
            0
        }
    "#,
        "true\ntrue\ntrue"
    );
}

#[test]
fn audit_json_is_valid_leading_zero_still_rejected() {
    // 前导零（01）修复前后双端都拒绝——守护既有行为不回退。
    if !can_link() {
        return;
    }
    dual_assert!(
        r#"
        func main() -> i32 {
            println(json_is_valid("{\"a\":01}"))
            println(json_is_valid("{\"a\":0}"))
            0
        }
    "#,
        "false\ntrue"
    );
}
