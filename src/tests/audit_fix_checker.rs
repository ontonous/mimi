//! Wave-1 audit-fix regression tests — checker.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
use super::*;

fn has_code(errors: &[crate::diagnostic::Diagnostic], code: &str) -> bool {
    errors.iter().any(|e| e.code.as_deref() == Some(code))
}

fn assert_err_code(src: &str, expected: &str) {
    let errors = match check_source(src) {
        Err(errors) => errors,
        Ok(()) => panic!("expected error {expected}, but check succeeded\nsrc: {src}"),
    };
    assert!(
        has_code(&errors, expected),
        "expected {expected}, got codes: {:?}\nsrc: {src}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

// ─── Fix 1: let-generalization must not run before check_pattern ─────
// [VERIFIED HIGH] check_stmt.rs — `let (a, b) = (None, 1)` raised a false
// E0251 ("cannot match tuple pattern against non-tuple type forall T0")
// because generalize() wrapped the tuple in ForAll before the Tuple arm of
// check_pattern could decompose it.

#[test]
fn fix1_tuple_let_with_free_typevar_checks() {
    check_source(
        r#"
func main() -> i32 {
    let (a, b) = (None, 1)
    let (c, d) = (Some(1), "x")
    0
}
"#,
    )
    .expect("tuple destructuring with free TypeVars must check");
}

#[test]
fn fix1_wrong_tuple_pattern_still_rejected() {
    // A genuinely wrong tuple pattern keeps E0251 after the reorder.
    assert_err_code(
        r#"
func main() -> i32 {
    let (a, b) = 5
    0
}
"#,
        crate::diagnostic::codes::E0251,
    );
}

#[test]
fn fix1_let_polymorphism_preserved() {
    // Moving check_pattern before generalize must not lose let-polymorphism
    // for plain-variable bindings (each read re-instantiates fresh vars).
    check_source(
        r#"
func main() -> i32 {
    let id = fn(x: _) { x }
    let a = id(1)
    let b = id("s")
    0
}
"#,
    )
    .expect("generalized let binding must stay polymorphic at every use");
}

// ─── Fix 2: `let x;` without initializer ─────────────────────────────
// [HIGH] check_stmt.rs — silently typed as unit; resolved lowering
// hard-rejected later (whole-program failure). Now E0820 at check time.

#[test]
fn fix2_let_without_initializer_rejected() {
    assert_err_code(
        r#"
func main() -> i32 {
    let x;
    0
}
"#,
        crate::diagnostic::codes::E0820,
    );
}

// ─── Fix 3: annotated `ref` linear let ───────────────────────────────
// [MEDIUM] check_stmt.rs — the E0427 rejection existed only in the
// unannotated branch; `let ref x: T = <linear>` silently dropped the ref
// flag (checker/IR divergence).

#[test]
fn fix3_annotated_ref_linear_let_rejected() {
    assert_err_code(
        r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let ref r: Zero = s0
    0
}
"#,
        crate::diagnostic::codes::E0427,
    );
}

#[test]
fn fix3_unannotated_ref_linear_let_still_rejected() {
    // The hoisted check must preserve the pre-existing unannotated rejection.
    assert_err_code(
        r#"
flow Counter {
    state Zero { count: i32 }
    state Positive { count: i32 }
    transition inc(Zero) -> Positive { return Positive { count: self.count + 1 } }
}
func main() -> i32 {
    let s0 = Zero { count: 0 }
    let ref r = s0
    0
}
"#,
        crate::diagnostic::codes::E0427,
    );
}

#[test]
#[ignore = "V-1 known gap (devdocs/full-audit-2026-08-05.md §16): bare `let ref` outside arena has no checker-finalized canonical Reference; fail-closed at lowering (Wave-3 item — materialize canonical Reference at lowering). Tracked: devdocs/wave1-progress-roadmap-2026-08-05.md §6 Wave-3."]
fn fix3_ref_nonlinear_let_still_checks() {
    check_source(
        r#"
func main() -> i32 {
    let v = 42
    let ref r = v
    0
}
"#,
    )
    .expect("non-linear ref let must keep checking");
}

// ─── Fix 4: plain-block shadowing aligned with branch blocks ─────────
// [MEDIUM] check_stmt.rs — Stmt::Block pushed a type scope but no var
// scope, so `{ let x }` after `let x` raised E0403 while `if c { let x }`
// did not.

#[test]
fn fix4_plain_block_shadowing_accepted_like_branches() {
    check_source(
        r#"
func main() -> i32 {
    let x = 1
    { let x = 2 }
    if x > 0 { let x = 3 }
    x
}
"#,
    )
    .expect("shadowing across a plain-block boundary must be legal");
}

#[test]
fn fix4_same_scope_rebind_still_rejected() {
    // Sanctioned contract (dual_let_shadow): same-scope rebinding stays E0403.
    assert_err_code(
        r#"
func main() -> i32 {
    let x = 1
    let x = 2
    x
}
"#,
        crate::diagnostic::codes::E0403,
    );
}

// ─── Fix 5: actor/impl method return + session hygiene ───────────────
// [HIGH] items.rs — methods skipped block_returns_on_all_paths and the
// E0425 session scope-exit check; session_residuals bled across methods.

#[test]
fn fix5_actor_method_missing_return_rejected() {
    assert_err_code(
        r#"
actor A {
    func bad() -> i32 { let x = 1 }
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0255,
    );
}

#[test]
fn fix5_valid_actor_methods_pass() {
    check_source(
        r#"
actor A {
    func ok() -> i32 { 42 }
    func ok_unit() { println("u") }
}
func main() -> i32 { 0 }
"#,
    )
    .expect("valid actor methods must keep checking");
}

#[test]
fn fix5_impl_method_missing_return_rejected() {
    assert_err_code(
        r#"
trait Getter {
    func get() -> i32;
}
type Holder { v: i32 }
impl Getter for Holder {
    func get() -> i32 { let y = 2 }
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0255,
    );
}

#[test]
fn fix5_session_residuals_do_not_bleed_between_methods() {
    // leaky leaves its SessionChan mid-protocol (E0425 belongs to leaky/ch1);
    // clean finishes its own endpoint and must NOT inherit ch1's residual.
    let src = r#"
session S = !i32 . end
actor A {
    func leaky(ch1: SessionChan<S>) -> i32 { 0 }
    func clean(ch2: SessionChan<S>) -> i32 {
        session_send(ch2, 1)
        session_close(ch2)
        0
    }
}
func main() -> i32 { 0 }
"#;
    let errors = check_source(src).expect_err("leaky session endpoint must be flagged");
    assert!(
        has_code(&errors, crate::diagnostic::codes::E0425),
        "expected E0425 for the unfinished endpoint, got: {:?}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
    let rendered: Vec<String> = errors.iter().map(|d| format!("{}", d)).collect();
    assert!(
        rendered.iter().any(|m| m.contains("ch1")),
        "E0425 must name the leaking endpoint ch1, got:\n{}",
        rendered.join("\n")
    );
    assert!(
        !rendered.iter().any(|m| m.contains("ch2")),
        "per-method reset must keep ch2 out of the diagnostics, got:\n{}",
        rendered.join("\n")
    );
}

// ─── Fix 6: duplicate extern declarations ────────────────────────────
// [HIGH] items.rs — extern registration inserted without duplicate check;
// a second extern block silently overwrote the first signature.

#[test]
fn fix6_duplicate_extern_rejected() {
    assert_err_code(
        r#"
extern "C" {
    func c_symbol(a: i32) -> i32;
}
extern "C" {
    func c_symbol(a: i64) -> i64;
}
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0402,
    );
}

#[test]
fn fix6_distinct_externs_pass() {
    check_source(
        r#"
extern "C" {
    func c_alpha(a: i32) -> i32;
    func c_beta(a: i32) -> i32;
}
func main() -> i32 { 0 }
"#,
    )
    .expect("distinct extern declarations must keep checking");
}

// ─── Fix 7: newtype constructor shadow check ─────────────────────────
// [HIGH] items.rs — newtype constructor registration lacked the CK3-style
// collision diagnostic that enum variants have.

#[test]
fn fix7_newtype_constructor_shadow_rejected() {
    assert_err_code(
        r#"
func UserId(x: i32) -> i32 { x }
newtype UserId = i32
func main() -> i32 { 0 }
"#,
        crate::diagnostic::codes::E0402,
    );
}

// ─── Fix 8: nested func no longer corrupts the funcs directory ───────
// [HIGH] check_stmt.rs — bare-name insertion permanently shadowed the
// top-level definition for all subsequently checked items.

#[test]
fn fix8_nested_func_shadow_does_not_leak() {
    // Inside `outer` the nested helper is visible and used; after `outer`,
    // `caller` must still type-check against the top-level helper signature.
    check_source(
        r#"
func helper(x: i32) -> i32 { x + 1 }
func outer() -> i32 {
    func helper(y: string) -> i32 { 0 }
    helper("shadowed")
}
func caller() -> i32 { helper(5) }
func main() -> i32 { caller() }
"#,
    )
    .expect("nested helper must not leak beyond its owner");
}

#[test]
fn fix8_nested_func_still_callable_inside_owner() {
    check_source(
        r#"
func outer2() -> i32 {
    func inc(a: i32) -> i32 { a + 1 }
    inc(41)
}
func main() -> i32 { outer2() }
"#,
    )
    .expect("nested func must stay callable after its declaration in the owner body");
}

#[test]
fn fix8_nested_func_shadow_dual_backend_execution() {
    // V-11 (audit 2026-08-05 §16): the shadowing shape must EXECUTE
    // identically on both backends, not just type-check. Inside `outer`
    // the nested `helper(string) -> i32` shadows the global
    // `helper(i32) -> i32`; `caller` outside still binds the global.
    // Pre-fix: lowering rejected the program (TOOL-RESOLUTION-001); with
    // lowering fixed but codegen untouched, the native build resolved the
    // shadowed call to the global LLVM symbol and crashed LLVM (string
    // struct passed for an i32 parameter).
    if !crate::tests::can_link() {
        return;
    }
    let src = r#"
func helper(x: i32) -> i32 { x + 1 }
func outer() -> i32 {
    func helper(y: string) -> i32 { len(y) }
    helper("shadowed")
}
func caller() -> i32 { helper(5) }
func main() -> i32 {
    println(outer())
    println(caller())
    0
}
"#;
    check_source(src).expect("shadowed nested helper must check");
    let (_interp_val, interp_stdout) = run_source_with_stdout(src);
    assert_eq!(interp_stdout.trim(), "8\n6", "VM shadow execution");
    let codegen_stdout = compile_and_run(src).expect("codegen failed");
    assert_eq!(codegen_stdout.trim(), "8\n6", "native shadow execution");
}

// ─── §4-#41: module function vs actor method NodeId collision ────────
// [MED] checker/items.rs + resolved catalog — the actor method key lacked
// the module path, so `module { func m }` + `actor { m }` collided into one
// NodeId and aborted a VALID program with TOOL-RESOLUTION-001.

#[test]
fn audit41_module_func_actor_method_same_name_no_node_id_collision() {
    // Top-level module function `util::m` and top-level actor method
    // `Counter::m` must coexist: qualified keys keep their NodeIds apart.
    check_source(
        r#"
module util {
    func m(x: i32) -> i32 { x + 1 }
}
actor Counter {
    mut count: i32 = 0;
    func m(v: i32) -> i32 { v + 2 }
}
func main() -> i32 { 0 }
"#,
    )
    .expect("module func and actor method with the same name must not collide");
}

#[test]
fn audit41_module_nested_actor_same_name_no_node_id_collision() {
    // The actor lives INSIDE the module: both `util::m` (function) and
    // `util::Counter::m` (method) must resolve — the method key must carry
    // the full module path, and the checker signature lookup for the
    // module-wrapped actor must still find its finalized signature.
    check_source(
        r#"
module util {
    func m(x: i32) -> i32 { x + 1 }
    actor Counter {
        mut count: i32 = 0;
        func m(v: i32) -> i32 { v + 2 }
        func go(n: i32) -> i32 { n * 2 }
    }
}
func main() -> i32 { 0 }
"#,
    )
    .expect("module-nested actor method must not collide with the module function");
}

// ─── Fix 9: guarded match arms are not full coverage ─────────────
// [HIGH] infer/match_.rs — guards can fail at runtime, leaving the variant
// unmatched; guarded arms must not count toward exhaustiveness.

#[test]
fn fix9_guarded_arm_alone_not_exhaustive() {
    assert_err_code(
        r#"
type Color { Red Green }
func pick(c: Color, flag: bool) -> i32 {
    match c {
        Red if flag => 1
        Green => 2
    }
}
func main() -> i32 { pick(Red, true) }
"#,
        crate::diagnostic::codes::E0215,
    );
}

#[test]
fn fix9_guarded_arm_with_wildcard_ok() {
    check_source(
        r#"
type Color { Red Green }
func pick(c: Color, flag: bool) -> i32 {
    match c {
        Red if flag => 1
        _ => 2
    }
}
func main() -> i32 { pick(Red, true) }
"#,
    )
    .expect("wildcard arm restores exhaustiveness");
}

// ─── Fix 10: exhaustiveness guard extended beyond 4 scalar types ─────
// [HIGH] infer/match_.rs — non-enum subjects other than i32/i64/f64/string
// silently matched nothing when no arm applied and no wildcard existed.

#[test]
fn fix10_tuple_subject_without_wildcard_rejected() {
    assert_err_code(
        r#"
func main() -> i32 {
    let t = (1, 2)
    match t {
        (0, 0) => 0
    }
}
"#,
        crate::diagnostic::codes::E0215,
    );
}

#[test]
fn fix10_tuple_subject_with_wildcard_ok() {
    check_source(
        r#"
func main() -> i32 {
    let t = (1, 2)
    match t {
        (0, 0) => 0
        _ => 1
    }
}
"#,
    )
    .expect("wildcard arm restores exhaustiveness for tuple subjects");
}

#[test]
fn fix10_catchall_tuple_pattern_ok() {
    // `(a, b)` binds any element values — a structural catch-all, so no
    // wildcard is required (keeps ck5/dual_match_tuple_bind_vars green).
    check_source(
        r#"
func main() -> i32 {
    let t = (1, 2)
    match t {
        (a, b) => a + b
    }
}
"#,
    )
    .expect("all-binding tuple pattern is exhaustive");
}

#[test]
fn fix10_newtype_self_constructor_match_ok() {
    // A constructor pattern naming the subject's own newtype always matches.
    check_source(
        r#"
newtype UserId = i32
func get_id(u: UserId) -> i32 {
    match u {
        UserId(v) => v
    }
}
func main() -> i32 { get_id(UserId(42)) }
"#,
    )
    .expect("self-constructor match on a newtype is exhaustive");
}

// §2-#14 (audit 2026-08-05, re-verified 2026-08-06): concurrency handle
// builtins lacked a checker arity check — `mutex_new(1,2,3)` passed
// `mimi check` and only trapped as E0800 at run/codegen (check/run
// divergence). Now rejected at check time (E0242), matching codegen/VM.
#[test]
fn audit_14_handle_builtin_arity_rejected_at_check() {
    assert_err_code(
        r#"
func main() -> i32 {
    let _ = mutex_new(1, 2, 3)
    0
}
"#,
        crate::diagnostic::codes::E0242,
    );
    // atomic factory surplus args, same rejection.
    assert_err_code(
        r#"
func main() -> i32 {
    let _ = atomic_i32_new(0, 1)
    0
}
"#,
        crate::diagnostic::codes::E0242,
    );
}

#[test]
fn audit_14_handle_builtin_legal_arities_still_check() {
    // channel_new is 0-arg; handle factories are 1-arg. Neither must regress.
    check_source(
        r#"
func main() -> i32 {
    let _ = channel_new()
    let _ = mutex_new(1)
    let _ = atomic_bool_new(true)
    let _ = actor_spawn_count()
    0
}
"#,
    )
    .expect("legal handle-builtin arities must still check");
}

// §2-#15 (audit 2026-08-05): expect's message arg was checked when present
// but SURPLUS args were silently accepted (`o.expect("m", 1, 2)`), and
// Op::Unwrap has no message slot so extras are never honored — check/run
// divergence. Reject args.len() > 1 at check time.
#[test]
fn audit_15_expect_surplus_args_rejected() {
    assert_err_code(
        r#"
func main() -> i32 {
    let o: Option<i32> = Some(5)
    let _ = o.expect("m", 1, 2)
    0
}
"#,
        crate::diagnostic::codes::E0242,
    );
}

#[test]
fn audit_15_expect_single_message_still_checks() {
    check_source(
        r#"
func main() -> i32 {
    let o: Option<i32> = Some(5)
    let _ = o.expect("must be some")
    let u: Option<i32> = Some(3)
    let _ = u.unwrap()
    0
}
"#,
    )
    .expect("expect(message) and unwrap() must still check");
}

/// 0.34.35b (M-011③): record 的 fn 字段带参数直接调用 → 诚实拒绝。
/// 此前参数被静默吞掉（checker 返回字段类型、不检查 args），最终以
/// lowering 的 TOOL-RESOLUTION-001 内部标志拒绝，诊断误导。现 E0223
/// 明确说明"callee must be a function name"并指导先绑定字段。
#[test]
fn audit_11c_record_fn_field_direct_call_rejected() {
    assert_err_code(
        r#"
func add_impl(a: i64, b: i64) -> i64 { a + b }
type VTable { add: func(i64, i64) -> i64 }
func main() -> i32 {
    let vt = VTable { add: add_impl }
    let r = vt.add(1, 2)
    println(r)
    0
}
"#,
        crate::diagnostic::codes::E0223,
    );
}

/// M-011③ 对侧：字段取值再调用（`let f = vt.add; f(1,2)`）必须保持合法
/// ——这是 N-2 修复支持的一等函数路径，不能因拒绝直接调用而误伤。
#[test]
fn audit_11c_record_fn_field_bind_then_call_still_checks() {
    check_source(
        r#"
func add_impl(a: i64, b: i64) -> i64 { a + b }
type VTable { add: func(i64, i64) -> i64 }
func main() -> i32 {
    let vt = VTable { add: add_impl }
    let f = vt.add
    let r = f(1, 2)
    println(r)
    0
}
"#,
    )
    .expect("field bind-then-call must still check");
}

/// K-4 复核（2026-08-07）：NumericNarrowChecked 的合法来源只有显式 cast
/// （wrap 语义，0.34.34 裁决）。隐式收窄被两道门禁拒绝——checker
/// E0209/E0211 + lower implicit_conversion 仅允许 widen——因此不存在
/// "Bind/call 实参裸 wrap" 的 L1 分歧路径。以下测试钉死该契约，
/// 防止未来放宽 checker/lower 时静默引入 wrap-vs-trap 分歧。
#[test]
fn audit_k4_implicit_narrow_rejected_at_check() {
    // Bind 收窄：checker 拒绝（E0209）
    assert_err_code(
        r#"
func main() -> i32 {
    let wide: i64 = 3000000000
    let x: i32 = wide
    x
}
"#,
        crate::diagnostic::codes::E0209,
    );
    // call 实参收窄：checker 拒绝（E0211）
    assert_err_code(
        r#"
func takes_i32(x: i32) -> i32 { x }
func main() -> i32 {
    let wide: i64 = 3000000000
    let x = takes_i32(wide)
    x
}
"#,
        crate::diagnostic::codes::E0211,
    );
}

/// K-4 对侧：显式 cast 收窄保持 wrap（0.34.34 裁决），i32 算术溢出保持
/// trap（SD-7）——双端语义不得被收窄守卫改动。
#[test]
fn audit_k4_explicit_cast_wraps_and_i32_overflow_traps() {
    // 显式 cast：3000000000 as i32 = -1294967296（wrap）
    let vm = run_source_bytecode_result(
        r#"
func main() -> i32 {
    let wide: i64 = 3000000000
    let x = wide as i32
    x
}
"#,
    )
    .expect("cast wrap must run");
    assert_eq!(
        vm,
        crate::interp::Value::Int(-1294967296),
        "cast should wrap 3000000000 to -1294967296"
    );
    // i32 算术溢出：trap（SD-7）
    let err = run_source_bytecode_result(
        r#"
func main() -> i32 {
    let x: i32 = 2147483647
    let y: i32 = x + 1
    y
}
"#,
    )
    .expect_err("i32 overflow must trap");
    assert!(
        err.contains("E0802") || err.contains("overflow"),
        "i32 overflow should trap, got: {err}"
    );
}

/// K-5 (audit 2026-08-05, closed 2026-08-07): resolved Drop no-op 的合法性契约。
/// 结论：resolved 路径的 `ResolvedStmtKind::Drop(_) => Ok(None)` 仅在 place
/// 类型非 Capability 时安全——Capability 局部在 eligible 函数中结构性不可达
/// （参数/Bind 初始化均过 require_scalar_type），且 eligibility 的 Drop arm
/// 现显式对 place base local 类型做 require_scalar_type 守卫（fail-closed
/// fallback legacy）。非 cap 的 drop 三端均为纯 no-op。以下测试钉死两端行为。
#[test]
fn audit_k5_non_cap_drop_is_noop_both_backends() {
    let src = r#"
func main() -> i32 {
    let x = 5
    drop(x)
    println(x)
    0
}
"#;
    // VM: drop(x) 不影响后续使用
    let (vm, vm_out) = run_source_bytecode_with_stdout(src);
    assert_eq!(vm, crate::interp::Value::Int(0));
    assert!(vm_out.contains("5"), "vm should print 5, got: {vm_out}");
    // codegen: 同样 no-op
    let native = checked_compile_and_run(src).expect("codegen drop noop must run");
    assert!(
        native.contains("5"),
        "codegen should print 5, got: {native}"
    );
}

/// K-5 对侧：cap 变量的 drop 必须走 legacy（mimi_cap_drop 释放句柄），
/// 程序整体编译运行正确——per-function eligibility 将含 Capability 类型的
/// 函数过滤出 resolved slice（实测 'resolved skip: type Capability(...) is
/// not in the resolved native slice'），不会静默泄漏 CAP_TABLE 条目。
#[test]
fn audit_k5_cap_drop_compiles_and_runs_via_legacy() {
    let src = r#"
cap FileReadCap;

func use_cap() -> i32 {
    let c = FileReadCap
    drop(c)
    7
}

func main() -> i32 {
    println(use_cap())
    0
}
"#;
    let native = checked_compile_and_run(src).expect("cap drop must compile via legacy");
    assert!(
        native.contains("7"),
        "cap drop program should print 7, got: {native}"
    );
}

/// M3 (0.35.37): a Capability-typed function must fall back to legacy
/// (fail-closed), never silently accepted by the resolved emitter with a
/// no-op Drop. Locks the K-5 eligibility contract for an EXPLICITLY typed
/// cap local (declared-type path) — must compile and run through the
/// legacy path with the release actually emitted.
///
/// NOTE: passing a cap to a function (`consume_param(c)`) is accepted by
/// the checker (transfer-on-call consumption) but the legacy cap_vars
/// tracker still demands an explicit drop — a known checker/codegen
/// alignment gap tracked outside this audit item. The test therefore
/// exercises independent typed-local drops.
#[test]
fn audit_m3_cap_typed_functions_fallback_not_silently_accepted() {
    let src = r#"
cap FileReadCap;

func typed_local_a() -> i32 {
    let c: cap FileReadCap = FileReadCap
    drop(c)
    11
}

func typed_local_b() -> i32 {
    let c: cap FileReadCap = FileReadCap
    drop(c)
    13
}

func main() -> i32 {
    println(typed_local_a())
    println(typed_local_b())
    0
}
"#;
    let native = checked_compile_and_run(src).expect("cap fallback must compile and run");
    assert!(
        native.contains("11") && native.contains("13"),
        "cap fallback program should print 11 and 13, got: {native}"
    );
}

/// 0.35.37 (exactly-once alignment): passing a capability to a function is a
/// MOVE — the CFG checker consumes the argument (resource_lower.rs
/// emit_consumes on Call arguments), so the caller must NOT be required to
/// drop(c) again. Previously the legacy emitter never marked call arguments
/// consumed, so `sink(c)` left `c` registered and codegen demanded an extra
/// drop the checker did not require (compilation failed on valid code).
/// Also: `let c = FileReadCap` (no annotation) now registers in cap_vars, so
/// its drop actually emits the release.
#[test]
fn audit_cap_argument_transfer_aligns_checker_and_codegen() {
    let src = r#"
cap FileReadCap;

func sink(c: cap FileReadCap) -> i32 {
    drop(c)
    21
}

func sink_list(v: List<cap FileReadCap>) -> i32 {
    drop(v)
    23
}

func unannotated_drop() -> i32 {
    let c = FileReadCap
    drop(c)
    22
}

func main() -> i32 {
    let c = FileReadCap
    println(sink(c))
    let c2 = FileReadCap
    println(sink_list([c2]))
    println(unannotated_drop())
    0
}
"#;
    // Checker must accept (transfer consumes the cap, incl. inside a list).
    let file = parse(src);
    core::check(&file).expect("checker must accept argument transfer");
    // Codegen must compile AND run without demanding an extra drop(c).
    let native = checked_compile_and_run(src).expect("transfer must compile and run");
    assert!(
        native.contains("21") && native.contains("22") && native.contains("23"),
        "transfer program should print 21, 22 and 23, got: {native}"
    );
    // Double use must be rejected (moved-after-consumed) — checker and
    // codegen agree.
    let bad = r#"
cap FileReadCap;
func sink(c: cap FileReadCap) -> i32 { drop(c); 21 }
func main() -> i32 {
    let c = FileReadCap
    sink(c)
    sink(c)
    0
}
"#;
    let diags = check_source(bad).expect_err("double transfer must be rejected");
    let rendered = diags
        .iter()
        .map(|d| format!("{}", d))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("E0304") || rendered.contains("moved"),
        "double use must report moved-after-consumed, got:\n{rendered}"
    );
}

/// H-9 (Wave-2, closed 2026-08-07): match 落空必须发射 NON_EXHAUSTIVE_MATCH
/// （运行时 E0805 panic），而非静默 LoadUnit。此前 compiler.rs 落空分支
/// `fc.emit(Op::LoadUnit { rd })` 使新 opcode 零发射——VM 与 codegen
/// （mimi_match_panic）行为分歧。表面层落空被 checker 穷尽性门禁
/// （E0215 等）挡住，运行时仅 dynamic/flow 路径可达；本测试在字节码级
/// 钉死发射契约。
#[test]
fn audit_h9_match_fallthrough_emits_non_exhaustive_match() {
    let src = r#"
func main() -> i32 {
    let s = "b"
    match s {
        "a" => println("A")
        _ => println("other")
    }
    0
}
"#;
    let file = parse(src);
    let mut compiler = crate::interp::bytecode::BytecodeCompiler::new();
    let prog = compiler
        .compile_file(&file)
        .expect("bytecode compile failed");
    let disasm = crate::interp::bytecode::disasm::disassemble_program(&prog);
    assert!(
        disasm.contains("NON_EXHAUSTIVE_MATCH"),
        "match fall-through must emit NON_EXHAUSTIVE_MATCH (E0805), got:\n{disasm}"
    );
    // 行为对齐：wildcard arm 命中时程序正常执行（fall-through 为死码）
    let (vm, out) = run_source_bytecode_with_stdout(src);
    assert_eq!(vm, crate::interp::Value::Int(0));
    assert!(
        out.contains("other"),
        "wildcard arm should fire, got: {out}"
    );
}

/// §2-#19 (audit 2026-08-05, closed 2026-08-07): bound-generic 用户 trait
/// 方法调用此前被 lowering 内部标志 TOOL-RESOLUTION-001 拒绝（连正确调用
/// 也拒，诊断误导）。现 checker 前置 E0437 诚实拒绝——lowering 无法为泛型
/// 接收者选 impl（需单态化，1.x 评估）。Clone 为端到端可用例外。
#[test]
fn audit_2_19_bound_generic_trait_method_honest_e0437() {
    assert_err_code(
        r#"
trait Speak {
    func speak(x: i32) -> i32
}

type Dog {
    v: i32
}

impl Speak for Dog {
    func speak(x: i32) -> i32 { x + 1 }
}

func call_speak<T: Speak>(x: T, n: i32) -> i32 {
    x.speak(n)
}

func main() -> i32 {
    println(call_speak(Dog { v: 1 }, 5))
    0
}
"#,
        crate::diagnostic::codes::E0437,
    );
}

/// §2-#19 对侧：`T: Clone` 的 `x.clone()` 必须保持合法（lower 拷贝语义
/// 特化，端到端双后端可用）——E0437 不得误伤内建 Clone 路径。
#[test]
fn audit_2_19_bound_clone_still_legal() {
    check_source(
        r#"
func clone_it<T: Clone>(x: T) -> T { x.clone() }
func main() -> i32 {
    println(clone_it(42))
    0
}
"#,
    )
    .expect("bounded Clone method call must still check");
}

// ─── R-4 (audit 2026-08-05): alias-wrapping a linear type must not open a
// leak. 2026-08-07 实测复核：三场景均无用户可见漏检——
// ① SessionChan alias 泄漏仍报 E0425（session 端点审计独立于 is_linear）；
// ② cap 名作为 alias 目标报 E0407（undefined type，fail-closed 拒绝）；
// ③ flow state alias 泄漏与直接 flow state 泄漏行为一致（基线即不查，
//    alias 不引入差异）。下列回归钉死契约。

#[test]
fn r4_alias_of_session_chan_stays_linear() {
    let src = r#"
session S = !i32 . end
type MyChan = SessionChan<S>
func leaky(ch: MyChan) -> i32 { 0 }
func main() -> i32 { 0 }
"#;
    let errors = check_source(src).expect_err("alias-wrapped session leak must be flagged");
    assert!(
        has_code(&errors, crate::diagnostic::codes::E0425),
        "expected E0425 for the unfinished alias-typed endpoint, got: {:?}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn r4_alias_of_cap_name_rejected() {
    // Capability names are not type declarations; aliasing one fails closed
    // (E0407 undefined type) instead of silently weakening linearity.
    let src = r#"
cap Token
type MyCap = Token
func main() -> i32 { 0 }
"#;
    let errors = check_source(src).expect_err("cap alias must be rejected");
    assert!(
        has_code(&errors, crate::diagnostic::codes::E0407),
        "expected E0407 for aliasing a capability name, got: {:?}",
        errors
            .iter()
            .map(|e| e.code.as_deref().unwrap_or("none"))
            .collect::<Vec<_>>()
    );
}
