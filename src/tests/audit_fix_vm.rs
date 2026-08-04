//! Wave-1 audit-fix regression tests — vm.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
use super::*;


/// Local harness: run bytecode with stdout capture, returning the Result too
/// (the shared helpers either panic on error or drop the captured stdout).
fn vm_result_with_stdout(src: &str) -> (Result<interp::Value, String>, String) {
    let tokens = lexer::Lexer::new(src).tokenize().expect("tokenize");
    let file = parser::Parser::new(tokens).parse_file().expect("parse");
    let mut compiler = interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed");
    let mut vm = interp::bytecode::BytecodeVM::new(&prog);
    vm.enable_stdout_capture();
    let res = vm.run_value().map_err(|e| e.message().to_string());
    let stdout = vm.take_stdout();
    (res, stdout)
}

// ─────────────────────────────────────────────────────────────
// Fix #1 (HIGH): builtin Err inside `on failure` — the original
// InterpError used to be lost ("FaultRetEarly: no fault_reg set").
// It must now be stashed, the compensation must run, and the
// original E0800 text must propagate.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_builtin_fault_runs_cleanup_and_preserves_error() {
    let src = r#"
func main() -> i64 {
    on failure { println("CLEANUP_RAN") }
    let c = char_at("abc", 5)
    println(c)
    0
}
"#;
    let (res, stdout) = vm_result_with_stdout(src);
    let err = res.expect_err("char_at out-of-bounds must surface as an error");
    assert!(
        err.contains("char_at") && err.contains("out of bounds"),
        "the ORIGINAL builtin error text must survive the fault handler, got: {err}"
    );
    assert!(
        !err.contains("no fault_reg set"),
        "pre-fix lost-error diagnostic must be gone, got: {err}"
    );
    assert!(
        stdout.contains("CLEANUP_RAN"),
        "compensation must run before re-raise, stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("unreachable_after_fault"),
        "code after the failing call must not run"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #2 (HIGH): per-frame handler STACK + statement-position
// activation for `on failure`.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_nested_fault_handlers_cascade_lifo() {
    // Fault inside the inner scope: inner handler first, then outer (LIFO).
    let src = r#"
func main() -> i64 {
    on failure { println("OUTER") }
    {
        on failure { println("INNER") }
        let c = char_at("abc", 5)
        println(c)
    }
    0
}
"#;
    let (res, stdout) = vm_result_with_stdout(src);
    assert!(
        res.is_err(),
        "fault must still propagate after compensations"
    );
    let inner = stdout.find("INNER");
    let outer = stdout.find("OUTER");
    assert!(
        inner.is_some() && outer.is_some(),
        "both enclosing handlers must run, stdout: {stdout:?}"
    );
    assert!(
        inner.unwrap() < outer.unwrap(),
        "LIFO order: inner compensation before outer, stdout: {stdout:?}"
    );
}

#[test]
fn audit_vm_outer_handler_compensates_after_inner_normal_exit() {
    // Inner block exits NORMALLY (its handler is popped by the paired
    // ClearFaultPc); the subsequent fault belongs to the outer handler only.
    let src = r#"
func main() -> i64 {
    on failure { println("OUTER") }
    {
        on failure { println("INNER") }
        println("INNER_BLOCK_OK")
    }
    let c = char_at("abc", 5)
    println(c)
    0
}
"#;
    let (res, stdout) = vm_result_with_stdout(src);
    assert!(res.is_err(), "fault must propagate");
    assert!(
        stdout.contains("INNER_BLOCK_OK"),
        "inner block body must run, stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("OUTER"),
        "outer handler compensates the later fault, stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("INNER\n"),
        "inner handler must NOT fire after its scope exited normally, stdout: {stdout:?}"
    );
}

#[test]
fn audit_vm_fault_before_on_failure_declaration_not_compensated() {
    // Ruling (c): the handler is active FROM its statement's execution
    // point — code ABOVE the declaration is not covered.
    let src = r#"
func main() -> i64 {
    let c = char_at("abc", 5)
    println(c)
    on failure { println("SHOULD_NOT_RUN") }
    0
}
"#;
    let (res, stdout) = vm_result_with_stdout(src);
    assert!(res.is_err(), "fault must propagate");
    assert!(
        !stdout.contains("SHOULD_NOT_RUN"),
        "handler declared after the fault must not compensate it, stdout: {stdout:?}"
    );
}

#[test]
fn audit_vm_same_scope_multipe_handlers_all_run_lifo() {
    // Multiple `on failure` statements in one block: all activate in order
    // and all fire in reverse on fault (handler stack, not a single slot).
    let src = r#"
func main() -> i64 {
    on failure { println("A") }
    on failure { println("B") }
    on failure { println("C") }
    let c = char_at("abc", 5)
    println(c)
    0
}
"#;
    let (res, stdout) = vm_result_with_stdout(src);
    assert!(res.is_err(), "fault must propagate");
    let (pa, pb, pc) = (stdout.find('A'), stdout.find('B'), stdout.find('C'));
    assert!(
        pa.is_some() && pb.is_some() && pc.is_some(),
        "all three compensations must run, stdout: {stdout:?}"
    );
    assert!(
        pc.unwrap() < pb.unwrap() && pb.unwrap() < pa.unwrap(),
        "LIFO: C then B then A, stdout: {stdout:?}"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #3 (HIGH): impl-method mut_param_indices missing +1 for the
// implicit self; method calls also emitted no MutateSetup at all.
// Checker state: core/checker has no rejection of `mut`/`mutate`
// params on impl methods (grepped items.rs/func.rs), so the path
// is reachable and tested at runtime directly.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_impl_method_mut_param_writeback() {
    let src = r#"
type C { v: i64 }

impl C {
    func addk(mut k: i64) -> i64 {
        k = k + 10
        k
    }
}

func main() -> i64 {
    let c = C { v: 0 }
    let mut n = 5
    let r = c.addk(n)
    r * 1000 + n
}
"#;
    let v = run_source_bytecode_result(src);
    // r = final k = 15; write-back must put 15 into n. Pre-fix the write-back
    // read register 0 (self) instead of k, corrupting n.
    assert_eq!(
        v,
        Ok(interp::Value::Int(15_015)),
        "impl-method mut param must write back to the caller's variable"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #4 (HIGH): field-assignment chains depth ≥ 4 wrote into a
// dead clone — the write was lost. Full chain write-back now.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_deep_field_chain_assignment_persists() {
    let src = r#"
type Inner { value: i64 }
type L3 { inner: Inner }
type L2 { l3: L3 }
type L1 { l2: L2 }

func main() -> i64 {
    let x = L1 { l2: L2 { l3: L3 { inner: Inner { value: 1 } } } }
    x.l2.l3.inner.value = 99
    x.l2.l3.inner.value
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(
        v,
        Ok(interp::Value::Int(99)),
        "4-deep record chain assignment must persist"
    );
}

#[test]
fn audit_vm_deep_field_chain_assignment_dual_backend_parity() {
    let src = r#"
type Inner { value: i64 }
type L3 { inner: Inner }
type L2 { l3: L3 }
type L1 { l2: L2 }

func main() -> i32 {
    let x = L1 { l2: L2 { l3: L3 { inner: Inner { value: 1 } } } }
    x.l2.l3.inner.value = 99
    println(x.l2.l3.inner.value)
    0
}
"#;
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "99", "VM deep-chain write-back");
    if !can_link() {
        return;
    }
    let cg_out = compile_and_run(src).expect("codegen deep-chain write-back");
    assert_eq!(cg_out.trim(), "99", "codegen deep-chain write-back (L1)");
}

// ─────────────────────────────────────────────────────────────
// Fix #5 (MEDIUM): quote block `n` counted skipped statements →
// quote-stack underflow at runtime.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_quote_block_with_skipped_defer_no_underflow() {
    // `defer` inside quote! pushes nothing; pre-fix QuoteBlock{n: block.len()}
    // popped more nodes than were pushed → runtime stack underflow (E0800).
    let src = r#"
func main() -> i64 {
    let q = quote! {
        let a = 1
        defer { println("never") }
        a + 1
    }
    7
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(
        v,
        Ok(interp::Value::Int(7)),
        "quote! with a skipped statement kind must evaluate without underflow"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #6 (MEDIUM): tuple pattern emitted TupleGet BEFORE the type
// test — a non-tuple subject trapped instead of falling through.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_tuple_pattern_falls_through_on_non_tuple() {
    let src = r#"
func main() -> i64 {
    let t = (1, 2)
    let n = 5
    let r1 = match t {
        (a, b) => a + b
        _ => 100
    }
    let r2 = match n {
        (a, b) => a + b
        _ => 200
    }
    r1 + r2
}
"#;
    let v = run_source_bytecode_result(src);
    // r1 = 3 (tuple matches); r2 = 200 (wildcard after guarded fall-through).
    // Pre-fix the (a,b) arm against the Int subject trapped in TupleGet.
    assert_eq!(
        v,
        Ok(interp::Value::Int(203)),
        "tuple pattern on a non-tuple must fall through to the next arm"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #7 (MEDIUM): push_frame leaked recursion depth on early-Err
// paths (~768 recoverable failures → spurious "recursion limit").
// Each iteration below absorbs a requires-violation as a flow Fault;
// pre-fix every absorbed failure leaked one depth unit.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_push_frame_depth_not_leaked_by_failed_requires() {
    let src = r#"
func guarded(x: i64) -> i64 {
    requires: x > 0
    x
}

flow F {
    state S { v: i64 }

    transition go(S) -> S {
        let y = guarded(-1)
        return S { v: y }
    }
}

func main() -> i64 {
    let s = S { v: 0 }
    let mut i = 0
    while i < 800 {
        let r = F::go(s)
        i = i + 1
    }
    i
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(
        v,
        Ok(interp::Value::Int(800)),
        "800 recovered contract violations must not exhaust the recursion \
         budget (pre-fix: spurious 'recursion limit exceeded' near ~768)"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #8 (MEDIUM): pow() exponent i64→u32 via `as` truncated
// (pow(2, 4294967326) computed 2**30). Must error instead.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_pow_exponent_above_u32_max_errors() {
    // 4294967326 == u32::MAX + 31 → `as u32` wrapped to 30 (2**30).
    let src = r#"
func main() -> i64 {
    pow(2, 4294967326)
}
"#;
    let v = run_source_bytecode_result(src);
    let err = v.expect_err("exponent above u32::MAX must be rejected");
    assert!(
        err.contains("exponent") && err.contains("4294967326"),
        "expected an exponent-range error, got: {err}"
    );
}

#[test]
fn audit_vm_pow_negative_exponent_message_unchanged() {
    let src = r#"
func main() -> i64 {
    pow(2, -1)
}
"#;
    let v = run_source_bytecode_result(src);
    let err = v.expect_err("negative exponent must stay rejected");
    assert!(
        err.contains("negative exponent"),
        "preserve the existing negative-exponent error, got: {err}"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #9 (MEDIUM): literal constant folding skipped the i32 width
// policy — folded 2**31 in an i32 place trapped where codegen
// wraps (operator.rs: i32 pow computes in i64 then narrows with
// wrap; shifts mask the amount then wrap).
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_i32_literal_pow_folds_with_wrap_not_trap() {
    // `let x: i32 = 2 ** 31` — codegen wraps to i32::MIN (no trap); the VM
    // pre-fix folded to 2^31 and the let-level CheckI32 trapped.
    let src = r#"
func main() -> i32 {
    let x: i32 = 2 ** 31
    println(x)
    0
}
"#;
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "-2147483648",
        "VM must wrap folded literal i32 pow like codegen"
    );
    if !can_link() {
        return;
    }
    let cg_out = compile_and_run(src).expect("codegen i32 literal pow");
    assert_eq!(
        cg_out.trim(),
        "-2147483648",
        "codegen i32 literal pow (L1 parity)"
    );
}

#[test]
fn audit_vm_i32_literal_shl_masks_and_wraps() {
    // 1 << 40 in an i32 place: amount masked mod 32 (→ 1 << 8 = 256), result
    // wrapped — codegen semantics; pre-fix the VM folded 2^40 then trapped.
    let src = r#"
func main() -> i32 {
    let x: i32 = 1 << 40
    println(x)
    let y: i32 = 1 << 31
    println(y)
    0
}
"#;
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "256\n-2147483648",
        "VM literal i32 shl must mask the amount and wrap the result"
    );
    if !can_link() {
        return;
    }
    let cg_out = compile_and_run(src).expect("codegen i32 literal shl");
    assert_eq!(
        cg_out.trim(),
        "256\n-2147483648",
        "codegen i32 literal shl (L1 parity)"
    );
}

#[test]
fn audit_vm_i32_literal_add_fold_still_traps() {
    // The non-Pow/Shl folds keep the checked policy: folded i32 add overflow
    // traps (codegen's checked i32 add traps too — existing parity test
    // dual_i32_const_fold_let_overflow_trap_parity).
    let src = r#"
func main() -> i32 {
    let x: i32 = 2147483646 + 2
    println(x)
    0
}
"#;
    let v = run_source_bytecode_result(src);
    let err = v.expect_err("folded i32 add overflow must still trap");
    assert!(
        err.contains("overflow"),
        "expected integer overflow trap, got: {err}"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #10 (MEDIUM): nested named functions could not recurse —
// the name was bound only after the body compiled.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_nested_named_function_self_recursion() {
    let src = r#"
func main() -> i64 {
    func fact(n: i64) -> i64 {
        if n <= 1 {
            1
        } else {
            n * fact(n - 1)
        }
    }
    fact(5)
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(
        v,
        Ok(interp::Value::Int(120)),
        "nested named function must be able to call itself"
    );
}

#[test]
fn audit_vm_nested_named_function_still_callable_as_value() {
    // Pre-binding must not break the closure-value usage after the def.
    let src = r#"
func main() -> i64 {
    func twice(n: i64) -> i64 {
        n * 2
    }
    let f = twice
    f(21)
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(v, Ok(interp::Value::Int(42)));
}

// ─────────────────────────────────────────────────────────────
// Fix #11 (MEDIUM): the *Int float fallbacks hard-checked NaN/Inf
// ignoring ieee_depth, and DivInt's float zero-trap ignored
// ieee_float. SD-9: inside ieee_float{} these must not trap; the
// SD-9 rules outside are preserved.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_int_op_float_fallback_ieee_float_no_trap() {
    // Unknown-typed locals hold Floats → `a / b` lowers to DivInt and hits
    // the float fallback. Inside ieee_float{} the 0.0 divisor (→ Inf) and
    // the non-finite result must be accepted.
    let src = r#"
func one() -> f64 { 1.0 }
func zero() -> f64 { 0.0 }

func main() -> i64 {
    ieee_float {
        let a = one()
        let b = zero()
        let c = a / b
        let _ = c
        0
    }
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(
        v,
        Ok(interp::Value::Int(0)),
        "float ops through the *Int fallback inside ieee_float{{}} must not trap"
    );
}

#[test]
fn audit_vm_int_op_float_fallback_still_traps_outside_ieee() {
    // SD-9 preserved: the same computation OUTSIDE ieee_float{} traps.
    let src = r#"
func one() -> f64 { 1.0 }
func zero() -> f64 { 0.0 }

func main() -> i64 {
    let a = one()
    let b = zero()
    let c = a / b
    let _ = c
    0
}
"#;
    let v = run_source_bytecode_result(src);
    assert!(
        v.is_err(),
        "div-by-zero / non-finite result must still trap outside ieee_float"
    );
}

// ─────────────────────────────────────────────────────────────
// Fix #12 (MEDIUM): ensures bound plain param names to PRE-call
// snapshots — spurious E0808 for mutated `mut` params. Plain names
// now bind POST-call values; old(x) keeps the snapshot.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_ensures_binds_post_call_value_for_mut_param() {
    let src = r#"
func bump(mut x: i32) -> i32 {
    ensures: x == old(x) + 1
    x = x + 1
    x
}

func main() -> i32 {
    bump(5)
}
"#;
    let v = checked_run_source_bytecode_result(src);
    assert_eq!(
        v,
        Ok(interp::Value::Int(6)),
        "ensures must see the POST-call x (old(x) stays pre-call); \
         pre-fix raised spurious E0808"
    );
}

#[test]
fn audit_vm_ensures_old_snapshot_still_enforced() {
    // Negative control: a wrong postcondition must still fail.
    let src = r#"
func bad(mut x: i32) -> i32 {
    ensures: x == old(x) + 2
    x = x + 1
    x
}

func main() -> i32 {
    bad(5)
}
"#;
    let v = checked_run_source_bytecode_result(src);
    let err = v.expect_err("violated ensures must trap");
    assert!(
        err.contains("ensures condition failed"),
        "expected E0808 ensures violation, got: {err}"
    );
}

#[test]
fn audit_vm_ensures_non_mut_params_unchanged() {
    // Regression guard for the existing (non-mut) ensures semantics.
    let src = r#"
func add_to(x: i32, y: i32) -> i32 {
    ensures: result == old(x) + y
    x + y
}

func main() -> i32 {
    add_to(40, 2)
}
"#;
    let v = checked_run_source_bytecode_result(src);
    assert_eq!(v, Ok(interp::Value::Int(42)));
}

// ─────────────────────────────────────────────────────────────
// Fix #13 (LOW): to_float("NaN")/("inf") injected non-finite
// values past SD-9. Non-finite parses are now rejected (parity
// with parse_float).
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_to_float_rejects_non_finite_parses() {
    for bad in ["NaN", "inf", "-inf", "infinity", "1e999"] {
        let src = format!(
            r#"
func main() -> f64 {{
    to_float("{bad}")
}}
"#
        );
        let v = run_source_bytecode_result(&src);
        let err = v.expect_err("to_float must reject non-finite parse '{bad}'");
        assert!(
            err.contains("to_float") && err.contains("non-finite"),
            "expected SD-9 rejection for '{bad}', got: {err}"
        );
    }
}

#[test]
fn audit_vm_to_float_finite_parse_still_works() {
    let src = r#"
func main() -> i64 {
    let x = to_float("3.5")
    if x > 3.0 { 1 } else { 0 }
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(v, Ok(interp::Value::Int(1)));
}

// ─────────────────────────────────────────────────────────────
// Fix #14 / ruling (a): pop is IN-PLACE with write-back + error
// on empty. The builtin clones (value semantics) so `pop(var)` on
// a local compiles to Op::ListPop, which mutates the binding's
// register. Dual-backend: codegen agent D2 implements the
// in-place + trap codegen side — parity asserted here.
// ─────────────────────────────────────────────────────────────

#[test]
fn audit_vm_pop_mutates_caller_list_in_place() {
    let src = r#"
func main() -> i64 {
    let l = [1, 2, 3]
    let x = pop(l)
    (len(l) * 1000) + (l[0] * 100) + (l[1] * 10) + x
}
"#;
    let v = run_source_bytecode_result(src);
    // len(l) == 2, l == [1, 2], x == 3 → 2123. Pre-fix pop cloned and the
    // caller's list stayed length 3 (2313 pattern broken).
    assert_eq!(
        v,
        Ok(interp::Value::Int(2123)),
        "pop must mutate the caller's list in place and return the element"
    );
}

#[test]
fn audit_vm_pop_empty_list_errors() {
    let src = r#"
func main() -> i64 {
    let l = range(0, 0)
    pop(l)
}
"#;
    let v = run_source_bytecode_result(src);
    let err = v.expect_err("pop on an empty list must error");
    assert!(
        err.contains("pop") && err.contains("empty"),
        "expected empty-list pop error, got: {err}"
    );
}

#[test]
fn audit_vm_pop_writeback_dual_backend_parity() {
    // L1: both backends must show the same in-place mutation + returned
    // element (codegen side owned by agent D2).
    let src = r#"
func main() -> i32 {
    let l = [1, 2, 3]
    let x = pop(l)
    println(len(l))
    println(x)
    println(l[0])
    println(l[1])
    0
}
"#;
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "2\n3\n1\n2", "VM in-place pop");
    if !can_link() {
        return;
    }
    let cg_out = compile_and_run(src).expect("codegen in-place pop");
    assert_eq!(
        cg_out.trim(),
        "2\n3\n1\n2",
        "codegen in-place pop (L1 parity — codegen agent D2)"
    );
}

#[test]
fn audit_vm_pop_non_ident_arg_keeps_builtin_semantics() {
    // Non-variable arguments fall through to the builtin (clone semantics,
    // still errors on empty).
    let src = r#"
func make() -> List<i64> {
    [7, 8, 9]
}

func main() -> i64 {
    pop(make())
}
"#;
    let v = run_source_bytecode_result(src);
    assert_eq!(v, Ok(interp::Value::Int(9)));
}
