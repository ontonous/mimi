//! Wave-1 audit-fix regression tests — list_string.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
use super::*;

fn can_link() -> bool {
    crate::tests::can_link()
}

/// Dual-backend assertion mirroring src/tests/dual_backend.rs style:
/// checker gate, then VM stdout and codegen stdout must BOTH equal expected.
fn assert_dual(src: &str, expected: &str) {
    if !can_link() {
        return;
    }
    check_source(src).unwrap_or_else(|diags| {
        panic!(
            "checker rejected dual source:\n{}",
            diags
                .iter()
                .map(|d| format!("{}", d))
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    let interp_run = std::panic::catch_unwind(|| run_source_with_stdout(src));
    assert!(interp_run.is_ok(), "interpreter panicked");
    let (_interp_val, interp_stdout) = interp_run.unwrap();
    let codegen_stdout = compile_and_run(src).expect("codegen failed");
    assert_eq!(
        codegen_stdout.trim(),
        expected,
        "codegen mismatch\ncodegen: {}\nexpected: {}",
        codegen_stdout.trim(),
        expected
    );
    assert_eq!(
        interp_stdout.trim(),
        expected,
        "interpreter stdout mismatch\ninterp: {}\nexpected: {}",
        interp_stdout.trim(),
        expected
    );
}

// ── FIX 1: chr() full UTF-8 encoding (string.rs compile_chr) ─────────

#[test]
fn audit_chr_encodes_full_utf8_dual() {
    // chr(20320)=="你" (3-byte), chr(256)=="Ā" (2-byte), chr(65)=="A" (1-byte).
    // Old codegen truncated the code point to i8 — all three above 255 broke.
    assert_dual(
        r#"
        func main() -> i32 {
            println(chr(20320))
            println(chr(256))
            println(chr(65))
            0
        }
        "#,
        "你\nĀ\nA",
    );
}

#[test]
fn audit_chr_four_byte_encoding_dual() {
    // U+1F600 (😀) needs the 4-byte form — exercises the len==4 branch.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = chr(128512)
            println(len(s))
            println(s)
            0
        }
        "#,
        "1\n😀",
    );
}

#[test]
fn audit_chr_rejects_surrogate_both_backends() {
    // VM reference: char::from_u32(0xD800) == None → "chr: invalid code point".
    let src = r#"
        func main() -> i32 {
            let s = chr(55296)
            println(s)
            0
        }
        "#;
    let vm = run_source_bytecode_result(src);
    assert!(
        matches!(&vm, Err(e) if e.contains("invalid code point")),
        "VM should reject surrogate U+D800, got {:?}",
        vm
    );
    if !can_link() {
        return;
    }
    let cg = compile_and_run(src);
    assert!(
        matches!(&cg, Err(e) if e.contains("invalid code point")),
        "codegen should trap on surrogate U+D800, got {:?}",
        cg
    );
}

#[test]
fn audit_chr_rejects_out_of_range_both_backends() {
    // chr(-1) and chr(0x110000) → "chr: code point out of range".
    for lit in ["0 - 1", "1114112"] {
        let src = format!(
            r#"
            func main() -> i32 {{
                let s = chr({})
                println(s)
                0
            }}
            "#,
            lit
        );
        let vm = run_source_bytecode_result(&src);
        assert!(
            matches!(&vm, Err(e) if e.contains("code point out of range")),
            "VM should reject chr({}), got {:?}",
            lit,
            vm
        );
        if !can_link() {
            continue;
        }
        let cg = compile_and_run(&src);
        assert!(
            matches!(&cg, Err(e) if e.contains("code point out of range")),
            "codegen should trap on chr({}), got {:?}",
            lit,
            cg
        );
    }
}

// ── FIX 2: len(string) counts chars, not bytes (list/access.rs) ──────

#[test]
fn audit_len_counts_unicode_chars_dual() {
    // strlen returned 6 / 6; the VM returns chars().count(): 2 / 5.
    assert_dual(
        r#"
        func main() -> i32 {
            println(len("你好"))
            println(len("héllo"))
            0
        }
        "#,
        "2\n5",
    );
}

#[test]
fn audit_len_counts_unicode_chars_variable_dual() {
    // String struct path ({i8*, i64} value): field 1 is the BYTE length and
    // must not be returned as the char count.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = "你好" + "世界"
            println(len(s))
            0
        }
        "#,
        "4",
    );
}

// ── FIX 3: str_index_of returns char index (string/query.rs) ─────────

#[test]
fn audit_str_index_of_char_index_dual() {
    // Old codegen returned the BYTE offset (6 for "世界" inside "你好世界").
    // option_value_or unwraps the Option<i32> (builtin in both backends,
    // keeps this test focused on str_index_of semantics).
    assert_dual(
        r#"
        func main() -> i32 {
            println(option_value_or(str_index_of("你好世界", "世界"), 0 - 1))
            println(option_value_or(str_index_of("héllo", "llo"), 0 - 1))
            println(option_value_or(str_index_of("abc", "zz"), 0 - 1))
            0
        }
        "#,
        "2\n2\n-1",
    );
}

// ── P1-13: contains/index_of must not truncate at embedded NUL ──────

#[test]
fn audit_nul_safe_contains_index_of_dual() {
    // The C strstr implementation used by codegen stopped at the first NUL;
    // explicit-length search keeps subtraction and chained fragments intact.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = "a" + chr(0) + "b"
            let t = chr(0) + "b"
            println(str_contains(s, t))
            println(option_value_or(str_index_of(s, t), 0 - 1))
            println(str_contains(s, "b"))
            println(str_contains(s, "x"))
            println(str_starts_with(s, "a" + chr(0)))
            println(str_starts_with(s, "a" + chr(0) + "b"))
            println(str_starts_with(s, "b"))
            println(str_ends_with(s, chr(0) + "b"))
            println(str_ends_with(s, "b"))
            println(str_ends_with(s, "a"))
            0
        }
        "#,
        "true\n1\ntrue\nfalse\ntrue\ntrue\nfalse\ntrue\ntrue\nfalse",
    );
}

#[test]
fn audit_str_repeat_nul_safe_dual() {
    // Concat must preserve embedded NULs through the length-aware runtime,
    // and str_repeat must use the explicit {ptr,len} string ABI.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = "a" + chr(0) + "b"
            println(len(str_repeat(s, 2)))
            0
        }
        "#,
        "6",
    );
}

#[test]
fn audit_char_code_nul_safe_dual() {
    // char_code must use the explicit string struct length so it can see
    // characters after an embedded NUL.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = "a" + chr(0) + "b"
            println(char_code(s, 1))
            println(char_code(s, 2))
            0
        }
        "#,
        "0\n98",
    );
}

#[test]
fn audit_str_char_at_after_nul_dual() {
    // str_char_at must use the explicit string length so it can return the
    // character after an embedded NUL.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = "a" + chr(0) + "b"
            println(str_char_at(s, 2))
            0
        }
        "#,
        "b",
    );
}

#[test]
fn audit_str_replace_nul_safe_dual() {
    // str_replace must use explicit lengths so replacing an embedded NUL
    // works and the result length is preserved.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = "a" + chr(0) + "b"
            let r = str_replace(s, chr(0), "x")
            println(len(r))
            println(r)
            0
        }
        "#,
        "3\naxb",
    );
}

// ── Batch4-2 P2-3: contains must support List<f64> ─────────

#[test]
fn audit_contains_float_dual() {
    assert_dual(
        r#"
        func main() -> i32 {
            println(contains([1.5, 2.5, -0.0], 2.5))
            println(contains([1.5, 2.5, -0.0], 0.0))
            println(contains([1.5, 2.5, -0.0], 1.0))
            println(contains([1.5, 2.5, -0.0], 0.0 - 0.0))
            0
        }
        "#,
        "true
true
false
true",
    );
}

#[test]
fn audit_char_code_unicode_dual() {
    // Old codegen read raw bytes: char_code("你",0) returned 228 (first UTF-8
    // byte) instead of 20320, and OOB clamped to 0 instead of trapping.
    assert_dual(
        r#"
        func main() -> i32 {
            println(char_code("你", 0))
            println(char_code("héllo", 1))
            println(char_code("ABC", 2))
            0
        }
        "#,
        "20320\n233\n67",
    );
}

#[test]
fn audit_char_code_oob_traps_both_backends() {
    // VM reference: "char_code: index {} out of bounds" (also for negatives —
    // the VM wraps them to a huge usize which is OOB).
    for idx in ["5", "0 - 1"] {
        let src = format!(
            r#"
            func main() -> i32 {{
                let v = char_code("abc", {})
                println(v)
                0
            }}
            "#,
            idx
        );
        let vm = run_source_bytecode_result(&src);
        assert!(
            matches!(&vm, Err(e) if e.contains("char_code") && e.contains("out of bounds")),
            "VM should trap char_code(\"abc\", {}), got {:?}",
            idx,
            vm
        );
        if !can_link() {
            continue;
        }
        let cg = compile_and_run(&src);
        assert!(
            matches!(&cg, Err(e) if e.contains("char_code") && e.contains("out of bounds")),
            "codegen should trap char_code(\"abc\", {}), got {:?}",
            idx,
            cg
        );
    }
}

// ── FIX 5: str_substring function form clamps (string/transform.rs) ──

#[test]
fn audit_str_substring_fn_form_clamps_dual() {
    // VM reference (builtin_str_substring): indices clamp to the char count;
    // only start > end errors. Old codegen used the strict runtime helper
    // which aborted on end OOB.
    assert_dual(
        r#"
        func main() -> i32 {
            println(str_substring("abc", 0, 10))
            println(str_substring("你好世界", 1, 3))
            println(str_substring("abc", 2, 99))
            0
        }
        "#,
        "abc\n好世\nc",
    );
}

#[test]
fn audit_str_substring_start_gt_end_traps_both_backends() {
    let src = r#"
        func main() -> i32 {
            let s = str_substring("abc", 2, 1)
            println(s)
            0
        }
        "#;
    let vm = run_source_bytecode_result(src);
    assert!(
        matches!(&vm, Err(e) if e.contains("start > end")),
        "VM should reject start > end, got {:?}",
        vm
    );
    if !can_link() {
        return;
    }
    let cg = compile_and_run(src);
    assert!(
        matches!(&cg, Err(e) if e.contains("start > end")),
        "codegen should trap start > end, got {:?}",
        cg
    );
}

// ── FIX 6: range() sign-extends i32 bounds (list/construct.rs) ───────

#[test]
fn audit_range_negative_bounds_dual() {
    // zext of i32 bounds turned -5 into ~4e9 → empty list. VM: (start..end).
    assert_dual(
        r#"
        func main() -> i32 {
            let r = range(-5, 5)
            println(len(r))
            println(r[0])
            println(r[9])
            0
        }
        "#,
        "10\n-5\n4",
    );
}

// ── FIX 7: pop traps on empty; sort is PURE (list/mutate.rs) ─────────

#[test]
fn audit_pop_empty_traps_both_backends() {
    // Ruling: pop is in-place and traps on empty list — VM message
    // "pop from empty list". Old codegen returned a silent sentinel 0.
    let src = r#"
        func main() -> i32 {
            let xs: List<i64> = []
            let v = pop(xs)
            println(v)
            0
        }
        "#;
    let vm = run_source_bytecode_result(src);
    assert!(
        matches!(&vm, Err(e) if e.contains("pop from empty list")),
        "VM should trap pop on empty list, got {:?}",
        vm
    );
    if !can_link() {
        return;
    }
    let cg = compile_and_run(src);
    assert!(
        matches!(&cg, Err(e) if e.contains("pop from empty list")),
        "codegen should trap pop on empty list, got {:?}",
        cg
    );
}

#[test]
fn audit_pop_last_element_dual() {
    // Non-empty pop still returns the last element in both backends.
    assert_dual(
        r#"
        func main() -> i32 {
            let xs = [7, 8, 9]
            let v = pop(xs)
            println(v)
            0
        }
        "#,
        "9",
    );
}

#[test]
fn audit_sort_is_pure_dual() {
    // Ruling: sort is PURE — returns a new list, input untouched. Old codegen
    // bubble-sorted the caller's buffer in place (VM clones before sorting).
    assert_dual(
        r#"
        func main() -> i32 {
            let a = [3, 1, 2]
            let b = sort(a)
            println(a[0])
            println(a[1])
            println(a[2])
            println(b[0])
            println(b[1])
            println(b[2])
            0
        }
        "#,
        "3\n1\n2\n1\n2\n3",
    );
}

#[test]
fn audit_sort_empty_dual() {
    assert_dual(
        r#"
        func main() -> i32 {
            let e: List<i64> = []
            let s = sort(e)
            println(len(s))
            0
        }
        "#,
        "0",
    );
}

// ── FIX 8: sum checked accumulation (list/hof.rs) ────────────────────

#[test]
fn audit_sum_int_dual() {
    assert_dual(
        r#"
        func main() -> i32 {
            println(sum([1, 2, 3]))
            println(sum([10, 20, 30, 40]))
            0
        }
        "#,
        "6\n100",
    );
}

#[test]
fn audit_sum_overflow_traps_both_backends() {
    // VM reference: checked_add → "sum overflow" (no silent wrap). SD-7.
    let src = r#"
        func main() -> i32 {
            let xs: List<i64> = [9223372036854775807, 1]
            let t = sum(xs)
            println(t)
            0
        }
        "#;
    let vm = run_source_bytecode_result(src);
    assert!(
        matches!(&vm, Err(e) if e.contains("sum overflow")),
        "VM should trap sum overflow, got {:?}",
        vm
    );
    if !can_link() {
        return;
    }
    let cg = compile_and_run(src);
    assert!(
        matches!(&cg, Err(e) if e.contains("sum overflow")),
        "codegen should trap sum overflow, got {:?}",
        cg
    );
}

#[test]
fn audit_sum_float_vm_reference() {
    // VM reference semantics for float elements (checked int accumulation +
    // float accumulation + promotion). Codegen coverage is the ignored dual
    // test below, pending an element-type dispatch channel.
    let src = r#"
        func main() -> i32 {
            println(to_string(sum([1.5, 2.5])))
            0
        }
        "#;
    let (_v, stdout) = run_source_bytecode_with_stdout(src);
    assert_eq!(stdout.trim(), "4", "VM sum([1.5, 2.5]) should print 4");
}

#[test]
fn audit_sum_float_dual_pending_wave2() {
    assert_dual(
        r#"
        func main() -> i32 {
            let xs: List<f64> = [1.5, 2.5]
            println(to_string(sum(xs)))
            0
        }
        "#,
        "4",
    );
}

// ── FIX 9: to_string(f64) shortest round-trip (string/format.rs) ─────

#[test]
fn audit_to_string_f64_full_precision_dual() {
    // Old codegen used snprintf("%.15g") — 15 significant digits truncated the
    // value. mimi_to_string_f64 (Rust Display, shortest round-trip) matches
    // the VM's Value::Float Display exactly.
    assert_dual(
        r#"
        func main() -> i32 {
            println(to_string(123456789.123456789))
            println(to_string(1.5))
            println(to_string(0.1))
            0
        }
        "#,
        "123456789.12345679\n1.5\n0.1",
    );
}

// ── FIX 10: unicode trim/to_upper/to_lower (string/transform.rs) ─────

#[test]
fn audit_str_transform_unicode_dual() {
    // Runtime helpers mimi_str_trim / mimi_str_to_upper / mimi_str_to_lower
    // (audit-wave1) give VM parity with Rust str::trim/to_uppercase/
    // to_lowercase. Old codegen was ASCII-only (é survived unchanged).
    assert_dual(
        r#"
        func main() -> i32 {
            println(str_trim("  héllo  "))
            println(str_to_upper("héllo"))
            println(str_to_lower("HÉLLO"))
            0
        }
        "#,
        "héllo\nHÉLLO\nhéllo",
    );
}

#[test]
fn audit_str_transform_method_form_unicode_dual() {
    // Method forms .trim()/.to_upper()/.to_lower() funnel into the same
    // builtins (string_method_to_builtin) — must pick up the fix too.
    assert_dual(
        r#"
        func main() -> i32 {
            let s = " straße ".trim().to_upper()
            println(s)
            0
        }
        "#,
        "STRASSE",
    );
}

// ── to_int/to_float aggregate message parity (§14 leftover 2) ───────

#[test]
fn audit_to_int_aggregate_argument_fails_loud_with_vm_aligned_message() {
    // §14 leftover 2 (2026-08-06 string-guard campaign): to_int/to_float on
    // a List argument used to fail loud on BOTH backends but with divergent
    // messages — native strlen'd the list pointer and reported
    // "parse error: invalid digit", the VM reported E0800
    // "cannot convert this type". A statically known aggregate is now
    // rejected at compile time with the VM-aligned message.
    if !can_link() {
        return;
    }
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    to_int(xs)
}
"#;
    let err = match compile_and_run(src) {
        Err(e) => e,
        Ok(_) => panic!("to_int(List) must not compile on the native backend"),
    };
    assert!(
        err.contains("cannot convert this type"),
        "expected VM-aligned E0800 message, got: {err}"
    );
    // Same shape for to_float.
    let src_f = r#"
func main() -> i32 {
    let m = {"a": 1}
    to_float(m)
}
"#;
    let err_f = match compile_and_run(src_f) {
        Err(e) => e,
        Ok(_) => panic!("to_float(Map) must not compile on the native backend"),
    };
    assert!(
        err_f.contains("cannot convert this type"),
        "expected VM-aligned E0800 message, got: {err_f}"
    );
}

#[test]
fn audit_to_int_scalar_and_string_forms_still_dual() {
    // The aggregate guard must not reject the legitimate conversion forms.
    assert_dual(
        r#"
        func main() -> i32 {
            let a = to_int("42")
            let b = to_int(3.9)
            let c = to_int(true)
            let d = to_float("2.5")
            println(a + b + c + to_int(d))
            0
        }
        "#,
        "48",
    );
}

/// batch4-02 P1-1: str_join must preserve embedded NUL bytes in the
/// separator. The runtime now has a length-aware _ll variant and codegen
/// returns the explicit result length.
#[test]
fn audit_str_join_preserves_nul_in_separator_dual() {
    assert_dual(
        r#"
        func main() -> i32 {
            let parts: List<string> = ["a", "b"]
            let sep = "x" + chr(0) + "y"
            println(len(str_join(parts, sep)))
            0
        }
        "#,
        "5",
    );
}

/// AUD-2 (2026-08-20 critical audit): `to_int` on an out-of-range / non-finite
/// float used `fptosi` directly, which is UNDEFINED BEHAVIOR in LLVM and
/// miscompiled into a crash at `-O2`. The codegen now saturates via a
/// branch+phi that never calls `fptosi` out of range, matching the
/// interpreter's `f64 as i64` semantics. Dual-guarded so both backends agree.
#[test]
fn audit_to_int_out_of_range_saturates_dual() {
    assert_dual(
        r#"
        func main() {
            println(to_string(to_int(1e300)))
            println(to_string(to_int(-1e300)))
            println(to_string(to_int(3.9)))
            println(to_string(to_int(-3.9)))
        }
        "#,
        "9223372036854775807\n-9223372036854775808\n3\n-3",
    );
}
