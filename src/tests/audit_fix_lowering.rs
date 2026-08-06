//! Wave-1 audit-fix regression tests — lowering.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).
//!
//! Fixes covered (IR lowering + resolved catalog construction):
//!  #1  type-gated value-position `None` interception        (lower.rs)
//!  #2  IfLet pattern bindings must not leak into else       (lower.rs)
//!  #3  nested func inside `defer` collects syntax           (lower.rs)
//!  #4  unsafe_cast_protocol keeps dyn typing                (lower.rs)
//!  #5  flow-state pattern payload types fail closed         (lower.rs)
//!  #6  deterministic ident resolution (qualified first)     (lower.rs)
//!  #7  ContainerAliasErase conversion kind (surface probe)  (body.rs unit tests are
//!      authoritative — see ir/body.rs `validator_*_container_alias_erase_*`; the
//!      divergent-representation shape is not constructible from surface syntax while
//!      interning is canonical, so this file carries the smoke probe only)
//!  #8  `?` inside a lambda fails closed                     (lower.rs)
//!  #9  module-wrapped actor methods get finalized types     (resolved/mod.rs)
//!  #10 nested funcs in transition bodies / expression trees (resolved/mod.rs + lower.rs)
//!  #11 disjoint same-name nested helpers don't collide      (resolved/mod.rs)
//!  #12 duplicate flow states error instead of shadow        (resolved/mod.rs; not
//!      surface-reachable — the checker rejects duplicate states with E0402 — enforced
//!      by the diagnostic + mimi_debug_assert in collect_flow)
//!  #13 deterministic call-site fact tables                  (resolved/mod.rs)
use super::*;

// ─── #1 — type-gated value-position `None` interception ──────────────────────

#[test]
fn audit1_user_defined_none_variant_not_shadowed_in_value_position() {
    // CRITICAL: `name == "None"` used to map to builtin:value:None BEFORE any
    // type-gated lookup, hijacking user enum variants named None into the
    // builtin Option shape {i1,i64}. The interception is now gated on the
    // checker-finalized node type actually being the builtin Option.
    let src = r#"
type MyOption { Some(i32) None }

func describe(x: MyOption) -> i32 {
    match x {
        Some(v) => v
        None => -1
    }
}

func main() -> i32 {
    let x: MyOption = None
    let y: MyOption = Some(99)
    println(describe(x) + describe(y))
    0
}
"#;
    check_source(src).expect("user-defined None variant must pass the pipeline");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "98",
        "bytecode: None must resolve to the user variant"
    );
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen user-defined None variant");
    assert_eq!(
        native.trim(),
        "98",
        "codegen: None must resolve to the user variant"
    );
}

#[test]
fn audit1_builtin_none_still_intercepted_for_builtin_option() {
    // Companion: the gate must not break the builtin path.
    let src = r#"
func main() -> i32 {
    let x: Option<i32> = None
    match x {
        Some(v) => v
        None => 7
    }
}
"#;
    check_source(src).expect("builtin None keeps working");
    let value = run_source(src);
    assert_eq!(value, interp::Value::Int(7));
}

// ─── #2 — IfLet pattern bindings must not leak into else ─────────────────────

#[test]
fn audit2_if_let_pattern_bindings_do_not_leak_into_else() {
    // HIGH: lowering popped the pattern scope AFTER lowering the else block,
    // while the checker pops BEFORE checking else. The else branch must see
    // the outer binding.
    let src = r#"
func main() -> i32 {
    let x = 41
    let opt: Option<i32> = None
    if let Some(x) = opt {
        println(x)
    } else {
        let y = x + 1
        println(y)
    }
    0
}
"#;
    check_source(src).expect("if-let scoping program checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "42",
        "bytecode: else must resolve x to the OUTER binding"
    );
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen if-let scoping");
    assert_eq!(
        native.trim(),
        "42",
        "codegen: else must resolve x to the OUTER binding"
    );
}

// ─── H-8 — tail bare/wrapper blocks keep their implicit value ───────────────

#[test]
fn audit_h8_tail_bare_block_implicit_return_both_backends() {
    // H-8 (full-audit-2026-08-05): a tail bare block (or unsafe/ieee_float/
    // arena wrapper) carries the function's implicit return value. lowering
    // dropped it: check passed, build failed (native) / VM returned a wrong
    // value. Now both backends must agree on 6 + 20 = 26.
    let src = r#"
func f() -> i32 {
    {
        let x = 5
        x + 1
    }
}

func g() -> i32 {
    unsafe {
        let y = 10
        y * 2
    }
}

func main() -> i32 {
    println(f() + g())
    0
}
"#;
    check_source(src).expect("tail wrapper blocks check");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "26", "bytecode: tail values must flow");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen tail wrapper blocks");
    assert_eq!(native.trim(), "26", "codegen: tail values must flow");
}

#[test]
fn audit_h8_statement_position_wrapper_still_discards_value() {
    // A wrapper block in statement position (not the tail) must keep
    // discarding its value — only the tail contributes the implicit return.
    let src = r#"
func f() -> i32 {
    unsafe {
        let x = 99
        x
    }
    42
}

func main() -> i32 {
    println(f())
    0
}
"#;
    check_source(src).expect("statement-position wrapper checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42", "bytecode: statement value discarded");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen statement-position wrapper");
    assert_eq!(native.trim(), "42", "codegen: statement value discarded");
}

// ─── H-20 — user `deref` method hijacked by the 0.5 shared-deref branch ─────

#[test]
fn audit_h20_user_deref_method_not_hijacked_by_shared_deref() {
    // CRITICAL (codegen): method.rs 0.5 branch ran `compile_shared_deref`
    // whenever the method name was "deref" and the receiver was an Ident,
    // BEFORE trait dispatch. An ordinary struct receiver was then unwrapped
    // as Option<shared T> (extract field1 → inttoptr → load) → garbage read
    // / segfault, and the user's `deref` method was unreachable. The branch
    // is now gated on the shared registry + Option<shared …> type shape.
    let src = r#"
type Point { x: i32, y: i32 }

trait Deref {
    func deref() -> i32
}
impl Deref for Point {
    func deref() -> i32 { self.x }
}

func main() -> i32 {
    let p = Point { x: 42, y: 7 }
    println(p.deref())
    0
}
"#;
    check_source(src).expect("user deref method checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42", "bytecode: trait deref dispatches");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen user deref method");
    assert_eq!(
        native.trim(),
        "42",
        "codegen: trait deref dispatches (must not be hijacked)"
    );
}

#[test]
fn audit_h20_shared_var_deref_still_works_after_gate() {
    // Companion: the new gate must not break the genuine shared deref path.
    let src = r#"
func main() -> i32 {
    shared x = 42;
    println(x.deref());
    0
}
"#;
    check_source(src).expect("shared deref checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42", "bytecode: shared deref");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen shared deref");
    assert_eq!(native.trim(), "42", "codegen: shared deref");
}

// ─── §7-#74 — legacy list index: negative reads must wrap (VM parity) ───────

#[test]
fn audit_h74_negative_index_read_wraps_both_backends() {
    // CRITICAL: legacy check_list_bounds compared with UGE, so a negative
    // index (huge unsigned) ALWAYS aborted — while the VM and the resolved
    // emitter wrap reads (Python-style: xs[-1] is the last element) and trap
    // only when the wrap stays negative or lands >= len. Now aligned.
    let src = r#"
func main() -> i32 {
    let xs = [10, 20, 30]
    println(xs[0 - 1])
    println(xs[0 - 3])
    0
}
"#;
    check_source(src).expect("negative index read checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "30\n10", "bytecode: negative reads wrap");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen negative index read");
    assert_eq!(native.trim(), "30\n10", "codegen: negative reads wrap");
}

#[test]
fn audit_h74_negative_index_write_traps_both_backends() {
    // VM ListSet parity: writes do NOT wrap — a negative index is a hard
    // bounds error (E0803), on both backends.
    let src = r#"
func main() -> i32 {
    let xs = [1, 2, 3]
    xs[0 - 1] = 99
    println(xs[2])
    0
}
"#;
    check_source(src).expect("negative index write checks");
    let vm_res = run_source_result(src);
    assert!(
        vm_res.is_err(),
        "VM: negative write must trap, got {:?}",
        vm_res
    );
    if !can_link() {
        return;
    }
    let native = compile_and_run(src);
    assert!(
        native.is_err(),
        "codegen: negative write must trap, got {:?}",
        native
    );
}

#[test]
fn audit_h74_wrap_past_front_traps_both_backends() {
    // A negative read that stays negative after the wrap (|idx| > len) is
    // still OOB and must trap, not read garbage.
    let src = r#"
func main() -> i32 {
    let xs = [10, 20, 30]
    println(xs[0 - 4])
    0
}
"#;
    check_source(src).expect("wrap-past-front checks");
    let vm_res = run_source_result(src);
    assert!(
        vm_res.is_err(),
        "VM: wrap past front must trap, got {:?}",
        vm_res
    );
    if !can_link() {
        return;
    }
    let native = compile_and_run(src);
    assert!(
        native.is_err(),
        "codegen: wrap past front must trap, got {:?}",
        native
    );
}

// ─── #3 — nested func inside `defer` ─────────────────────────────────────────

#[test]
fn audit3_nested_function_inside_defer_compiles() {
    // HIGH: the syntax walker lacked Stmt::Defer while the catalog walker had
    // it — a nested func inside defer had a signature but no syntax and the
    // whole-program lowering failed.
    let src = r#"
func main() -> i32 {
    defer {
        func helper() -> i32 { 1 }
        if helper() != 1 { println(999) }
    }
    println(42)
    0
}
"#;
    // Gate: the full checker+catalog+lowering pipeline (check_source) and the
    // bytecode VM. Legacy codegen emission of defer-nested callables belongs
    // to the codegen wave; the audit regression is "compiles".
    check_source(src).expect("nested func inside defer lowers");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "42",
        "bytecode: defer-nested helper is sound"
    );
}

// ─── #4 — unsafe_cast_protocol keeps its dyn typing ──────────────────────────

#[test]
fn audit4_unsafe_cast_protocol_keeps_dyn_typing_through_lowering() {
    // HIGH: the cast lowered the argument, rewrote its ty to the dyn type and
    // returned ONLY the kind — for a Load argument the post-processing
    // recomputed the type from the place, silently dropping the cast. The
    // resolved body must now carry an explicit Cast (DynamicPack) whose target
    // is the dyn trait type.
    let src = r#"
trait Sensor {
    func read() -> i32;
}

type Driver {
    value: i32
}

impl Sensor for Driver {
    func read() -> i32 { self.value }
}

func main() -> i32 {
    let driver = Driver { value: 42 }
    let sensor: dyn Sensor = unsafe_cast_protocol(driver)
    println(sensor.read())
    0
}
"#;
    let file = parse(src);
    let program = core::check_program(&file).expect("dyn cast program checks");
    let body = program
        .resolved_body(&core::NodeId("function:main".into()))
        .expect("main has a resolved body");
    let mut dyn_cast_found = false;
    for statement in &body.root.statements {
        let core::ResolvedStmtKind::Bind {
            initializer: Some(initializer),
            ..
        } = &statement.kind
        else {
            continue;
        };
        let core::ResolvedExprKind::Cast { value, conversion } = &initializer.kind else {
            continue;
        };
        let Some(core::ResolvedType::Trait {
            kind: core::TraitTypeKind::Dynamic,
            ..
        }) = program.resolved_types().get(&conversion.to)
        else {
            continue;
        };
        dyn_cast_found = true;
        // The cast must wrap the lowered argument (a Load of `driver`),
        // and the conversion source must be the value's own type — i.e. the
        // cast was recorded, not recomputed away.
        assert!(
            matches!(value.kind, core::ResolvedExprKind::Load(_)),
            "cast must wrap the lowered argument"
        );
        assert_eq!(
            conversion.from, value.ty,
            "conversion source is the value type"
        );
    }
    assert!(
        dyn_cast_found,
        "unsafe_cast_protocol must lower to an explicit Cast towards the dyn type"
    );

    // Behavioral dual: dispatch through the cast works on both backends.
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen unsafe_cast_protocol");
    assert_eq!(native.trim(), "42");
}

// ─── #5 — flow-state pattern payload types ───────────────────────────────────

#[test]
fn audit5_flow_state_match_bindings_use_canonical_payload_types() {
    // MEDIUM: flow-state constructor patterns silently fell back to unit for
    // the payload type (`let _ = fty;`). Lowering now uses the interned
    // declared payload type from the canonical field table and fails closed
    // (E0830) when the table lacks an entry. Behavioral dual over payloaded
    // states guards the rewritten path.
    let src = r#"
flow Scale {
    state Small { v: i32 }
    state Large { v: i32 }
    transition lift(Small, delta: i32) -> Small | Large {
        if self.v + delta > 50 {
            return Large { v: self.v + delta }
        } else {
            return Small { v: self.v + delta }
        }
    }
}

flow Pipe {
    state Open { tag: string }
    state Closed { v: i32 }
    transition push(Open) -> Closed | Open {
        return Closed { v: 5 }
    }
}

func main() -> i32 {
    let s1 = Small { v: 10 }
    let r1 = Scale::lift(s1, 100)
    let t1 = match r1 {
        Small { v } => v
        Large { v } => v
    }
    let s2 = Small { v: 10 }
    let r2 = Scale::lift(s2, 5)
    let t2 = match r2 {
        Small { v } => v
        Large { v } => v
    }
    let o = Open { tag: "hello" }
    let r3 = Pipe::push(o)
    let t3 = match r3 {
        Closed { v } => v
        Open { tag } => 99
    }
    println(t1)
    println(t2)
    println(t3)
    0
}
"#;
    check_source(src).expect("payloaded flow-state match lowers");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "110\n15\n5");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen payloaded flow-state match");
    assert_eq!(native.trim(), "110\n15\n5");
}

// ─── #6 — deterministic ident resolution ─────────────────────────────────────

#[test]
fn audit6_value_position_ident_resolution_is_deterministic() {
    // MEDIUM: value-position ident resolution (function-as-value and
    // constants) now prefers the exact qualified-name match and admits a
    // short-name match only when unique. Surface syntax cannot spell
    // module-qualified identifiers (the parser rewrites `m::f` into nested
    // field expressions), so the short-name ambiguity guard is exercised by
    // the catalog shape, not by a user program; TODO(#audit-wave2) tracks
    // mirroring the checker's import-order resolution for the residual
    // divergence.
    // NOTE: the function-as-value probe must use a >=1-param function.
    // Zero-param functions in value position resolve as constructor-style
    // immediate calls by language design (checker/vars.rs), so `let f = five`
    // binds the return type, not a Func type.
    let src = r#"
func incr(x: i32) -> i32 { x + 1 }

const LIMIT: i32 = 37

func main() -> i32 {
    let f = incr
    println(f(4) + LIMIT)
    0
}
"#;
    check_source(src).expect("function-as-value and const idents resolve");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen ident resolution");
    assert_eq!(native.trim(), "42");
}

// ─── #7 — ContainerAliasErase surface probe ──────────────────────────────────

#[test]
fn audit7_container_bindings_lower_without_identity_type_lie() {
    // MEDIUM (#7): the authoritative regressions for the ContainerAliasErase
    // kind live in src/core/ir/body.rs (validator_accepts_container_alias_*
    // unit tests) because the divergent-representation shape is not
    // constructible from surface syntax while interning stays canonical. This
    // probe keeps the everyday container annotation paths green through the
    // rewritten implicit_conversion code.
    let src = r#"
type Pair { a: i32, b: i32 }

func main() -> i32 {
    let opt: Option<i32> = Some(1)
    let list: List<i32> = [1, 2, 3]
    let p = Pair { a: 1, b: 2 }
    println(p.a + p.b)
    match opt {
        Some(v) => println(v)
        None => println(0)
    }
    println(list.len())
    0
}
"#;
    check_source(src).expect("container annotations lower");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "3\n1\n3");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen container annotations");
    assert_eq!(native.trim(), "3\n1\n3");
}

// ─── #8 — `?` inside a lambda fails closed ───────────────────────────────────

#[test]
fn audit8_try_inside_lambda_fails_closed() {
    // LOW→fail-closed: propagation_target was always the function owner; inside
    // a lambda that is a wrong target and ResolvedLambda has no propagation
    // contract. Lowering now rejects the construct instead of fabricating a
    // target (E0830, documented; TODO(#audit-wave2) lifts it).
    let src = r#"
func may_fail(x: i32) -> Result<i32, i32> {
    if x > 0 { Ok(x) } else { Err(0) }
}

func main() -> i32 {
    let f = fn(x: i32) -> i32 { may_fail(x)? }
    println(f(1))
    0
}
"#;
    let diagnostics = check_source(src)
        .expect_err("? inside a lambda must fail closed at resolved-body lowering");
    let rendered = diagnostics
        .iter()
        .map(|diagnostic| format!("{diagnostic}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("E0830") && rendered.contains("lambda"),
        "expected the fail-closed lambda propagation diagnostic, got:\n{rendered}"
    );
}

#[test]
fn audit8_try_in_plain_function_still_lowers() {
    // Positive control for the restriction above.
    let src = r#"
func parse(v: i32) -> Result<i32, i32> {
    if v > 0 { Ok(v) } else { Err(-v) }
}

func user(v: i32) -> Result<i32, i32> {
    let x = parse(v)?
    Ok(x + 1)
}

func main() -> i32 {
    match user(3) {
        Ok(v) => v
        Err(_) => 0
    }
}
"#;
    check_source(src).expect("plain-function ? keeps lowering");
    let value = run_source(src);
    assert_eq!(value, interp::Value::Int(4));
}

// ─── #9 — module-wrapped actors get checker-finalized signatures ─────────────

#[test]
fn audit9_module_wrapped_actor_methods_get_finalized_signatures() {
    // HIGH: the checker registers actor methods WITHOUT the module path
    // (`A::run`) while the catalog is module-qualified (`m::A::run`); the
    // zonked-signature lookup missed and compilation aborted with "no
    // checker-finalized signature". The lookup now tries suffix keys
    // (longest first) before failing. Nested modules exercise multiple
    // stripping steps.
    let src = r#"
module m {
    actor A {
        func run(x: i32) -> i32 { x }
    }
}

module outer {
    module inner {
        actor B {
            func get(x: i32) -> i32 { x + 2 }
        }
    }
}

func main() -> i32 {
    println(42)
    0
}
"#;
    // Before the fix this aborted with "no checker-finalized signature" for
    // `function:m::A::run`; check_source drives the whole pipeline, including
    // lowering and codegen-adjacent validation of every catalog entry.
    check_source(src).expect("module-wrapped actors finalize");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42");
    if !can_link() {
        return;
    }
    let native = compile_and_run(src).expect("codegen module-wrapped actors");
    assert_eq!(native.trim(), "42");
}

// ─── #10 — nested funcs in transition bodies and expression positions ────────

#[test]
fn audit10_nested_function_inside_transition_body_compiles_and_runs() {
    // HIGH: transition bodies were never walked by the nested-function
    // catalog collector — meta/call-site walks recorded the callable, the
    // catalog did not, leaving a dangling NestedCallable.
    let src = r#"
flow F {
    state S { v: i32 }
    state T { v: i32 }
    transition go(S) -> T {
        func bump(x: i32) -> i32 { x + 1 }
        return T { v: bump(self.v) }
    }
}

func main() -> i32 {
    let s = S { v: 41 }
    let t = F::go(s)
    println(t.v)
    0
}
"#;
    // Gate: full pipeline + bytecode VM; legacy codegen emission of
    // transition-nested callables belongs to the codegen wave.
    check_source(src).expect("transition-nested func collects a catalog record");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42");
}

#[test]
fn audit10_nested_function_inside_block_expression_compiles_and_runs() {
    // HIGH: expression positions (block expressions, lambda bodies) were
    // uncollected — `let x = { func f() ... f() }` produced a catalog
    // signature without a syntax/body pairing.
    let src = r#"
func main() -> i32 {
    let x = {
        func f() -> i32 { 1 }
        f() + 41
    }
    println(x)
    0
}
"#;
    // Gate: full pipeline + bytecode VM; legacy codegen emission of
    // expression-nested callables belongs to the codegen wave.
    check_source(src).expect("block-expression-nested func collects");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42");
}

#[test]
fn audit10_nested_function_inside_lambda_body_collects() {
    // Lambda bodies share the enclosing callable's owner; the collection must
    // follow the checker (which records the nested signature under the
    // enclosing function, not the lambda).
    let src = r#"
func main() -> i32 {
    let f = fn(base: i32) -> i32 {
        func one() -> i32 { 1 }
        base + one()
    }
    println(f(41))
    0
}
"#;
    // Gate: full pipeline + bytecode VM; legacy codegen emission of
    // lambda-nested callables belongs to the codegen wave.
    check_source(src).expect("lambda-nested func collects");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "42");
}

// ─── #11 — disjoint same-name nested helpers ─────────────────────────────────

#[test]
fn audit11_same_name_nested_helpers_in_disjoint_branches_compile() {
    // HIGH: nested_function_owner keyed on owner+name+signature only — two
    // checker-accepted helpers with identical signatures in disjoint branches
    // collided into one NodeId (duplicate-identity abort). The key now folds
    // in the declaration's source anchor. The two helpers below are
    // behaviorally identical ON PURPOSE: the checker itself conflates
    // same-named nested callables into one bare-name slot, and the backends
    // resolve the bare call by their own (unscoped) tables, so asserting
    // per-branch dispatch would over-constrain semantics this fix does not
    // own. The regression is the compile abort, asserted through the full
    // pipeline plus dual runs.
    let src = r#"
func pick(flag: bool) -> i32 {
    if flag {
        func helper() -> i32 { 1 }
        helper()
    } else {
        func helper() -> i32 { 1 }
        helper()
    }
}

func main() -> i32 {
    println(pick(true) + pick(false))
    0
}
"#;
    // Gate: full pipeline + bytecode VM (the VM binds nested callables
    // lexically per block, so both branches resolve); legacy codegen emission
    // of branch-nested callables belongs to the codegen wave.
    check_source(src).expect("disjoint same-name nested helpers are distinct nodes");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "2");
}

// ─── #13 — deterministic call-site fact tables ───────────────────────────────

#[test]
fn audit13_same_bare_method_name_across_actors_checks_builds_and_runs() {
    // MEDIUM: method_info/extern_info/function_info fact tables were built
    // over raw HashMap iteration (nondeterministic last-wins on bare-name
    // collisions). Tables are now built over sorted catalog order, bare names
    // with conflicting signatures are dropped (qualified keys remain).
    // Nondeterminism itself is only observable across process randomization;
    // this regression pins the correctness + stability of the shared-name
    // shape within a run.
    let src = r#"
actor Foo {
    func handle() -> i32 { 1 }
}

actor Bar {
    func handle() -> i32 { 2 }
}

func main() -> i32 {
    println(7)
    0
}
"#;
    // Two independent pipeline runs over the same source must agree.
    check_source(src).expect("same-bare-name actors check (run 1)");
    check_source(src).expect("same-bare-name actors check (run 2)");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "7");
    if !can_link() {
        return;
    }
    let first = compile_and_run(src).expect("codegen same-bare-name actors (run 1)");
    let second = compile_and_run(src).expect("codegen same-bare-name actors (run 2)");
    assert_eq!(first.trim(), "7");
    assert_eq!(second.trim(), "7");
}

// ─── §6-#57 — binop i32 width context (operator.rs compile_binop) ───────────

#[test]
fn audit_h57_list_i64_element_add_literal_both_backends() {
    // CRITICAL L1: the 0.34.34 i32_ctx predicate was `lhs is i32 || rhs is
    // i32`. `let xs: List<i64> = [2147483647, 1]; xs[0] + 1` lowers the i64
    // list element (runtime i64) against the i32 literal `1` — the checker
    // unifies the binop to i64, so the mixed pair is a REAL i64 expression.
    // The old predicate emitted a checked i64 add followed by a spurious i32
    // range guard that trapped on 2147483648 (native E0802) while the VM
    // printed it. The predicate is now `&&` (only two i32 operands are
    // i32-width); this pins the L1 equivalence across VM, resolved
    // (compile_checked) and legacy (compile_file) codegen.
    let src = r#"
func main() -> i32 {
    let xs: List<i64> = [2147483647, 1]
    println(xs[0] + 1)
    0
}
"#;
    check_source(src).expect("List<i64> element add literal checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "2147483648",
        "bytecode: i64 element arithmetic"
    );
    if !can_link() {
        return;
    }
    let resolved = checked_codegen_compile_and_run(src)
        .expect("resolved codegen List<i64> element add literal");
    assert_eq!(
        resolved.trim(),
        "2147483648",
        "resolved codegen: i64 element arithmetic (was spurious E0802)"
    );
    let legacy = compile_and_run(src).expect("legacy codegen List<i64> element add literal");
    assert_eq!(
        legacy.trim(),
        "2147483648",
        "legacy codegen: i64 element arithmetic"
    );
}

#[test]
fn audit_h57_i32_binop_still_traps_resolved_and_vm() {
    // Guard against regressing the original 0.34.34 fix: a genuine i32 binop
    // (both operands i32) must still trap at i32::MAX + 1 on the VM and on
    // the resolved (compile_checked) codegen — the resolved emitter stores
    // i32 variables as true i32 slots, so `&&` still sees two 32-bit
    // operands and keeps the range guard.
    let src = r#"
func main() -> i32 {
    let x: i32 = 2147483647
    println(x + 1)
    0
}
"#;
    check_source(src).expect("i32 binop trap checks");
    let vm_res = run_source_result(src);
    assert!(
        vm_res.is_err(),
        "VM: i32 overflow must trap, got {:?}",
        vm_res
    );
    if !can_link() {
        return;
    }
    let resolved = checked_codegen_compile_and_run(src);
    assert!(
        resolved.is_err(),
        "resolved codegen: i32 overflow must trap, got {:?}",
        resolved
    );
    // Legacy stores i32-annotated slots as true i32 (0.34.34) and int
    // literals as i64, so `x + 1` is a 32+64 pair — the `||` heuristic
    // still identifies the i32 context and traps, matching VM + resolved.
    let legacy = compile_and_run(src);
    assert!(
        legacy.is_err(),
        "legacy codegen: i32 overflow must trap, got {:?}",
        legacy
    );
}

#[test]
fn audit_h57_i64_shift_not_truncated_to_32_bits() {
    // The old `||` predicate also misrouted `x: i64 << 34` into a 32-bit
    // shift (shift-amount masked by 31 → shifted by 2 → 4 instead of
    // 17179869184) on the resolved emitter. `&&` restores width-based
    // shifting: i64 expression → 64-bit shift.
    let src = r#"
func main() -> i32 {
    let x: i64 = 1
    println(x << 34)
    0
}
"#;
    check_source(src).expect("i64 shift checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "17179869184", "bytecode: i64 shift width");
    if !can_link() {
        return;
    }
    let resolved = checked_codegen_compile_and_run(src).expect("resolved codegen i64 shift");
    assert_eq!(
        resolved.trim(),
        "17179869184",
        "resolved codegen: i64 shift width"
    );
    let legacy = compile_and_run(src).expect("legacy codegen i64 shift");
    assert_eq!(
        legacy.trim(),
        "17179869184",
        "legacy codegen: i64 shift width"
    );
}

#[test]
fn audit_h57_for_loop_i64_element_arithmetic_both_backends() {
    // §6-#57 core scenario: a List<i64> for-loop variable is i64-width —
    // v + 1 must not trap even when v = 2147483647 (i32 range). The bytecode
    // VM previously hard-wired loop variables to i64 anyway (no trap, no
    // i32 range) but with the WRONG width for inferred i32 lists; the
    // resolved codegen previously trapped v+1 through the spurious i32 binop
    // context. All three paths must agree.
    let src = r#"
func main() -> i32 {
    let xs: List<i64> = [2147483647, 1]
    for v in xs {
        println(v + 1)
    }
    0
}
"#;
    check_source(src).expect("for-loop i64 element checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "2147483648\n2",
        "bytecode: loop var i64 arithmetic"
    );
    if !can_link() {
        return;
    }
    let resolved =
        checked_codegen_compile_and_run(src).expect("resolved codegen for-loop i64 element");
    assert_eq!(
        resolved.trim(),
        "2147483648\n2",
        "resolved: loop var i64 arithmetic"
    );
    let legacy = compile_and_run(src).expect("legacy codegen for-loop i64 element");
    assert_eq!(
        legacy.trim(),
        "2147483648\n2",
        "legacy: loop var i64 arithmetic"
    );
}

// ─── H-11 — ieee_float{} finiteness divergence for `**` and unary `-` ───────
// codegen authority: Pow routes through check_float_finite (ieee-aware,
// operator.rs:1378); unary float negation is a bare `0.0 - x` with NO
// finiteness guard (operator.rs:256). The bytecode VM had Nyet: PowFloat and
// the NegInt float branch hardcoded is_nan/is_infinite (ignoring ieee_depth,
// over-strict inside ieee_float{}, divergent vs codegen in both directions),
// and the resolved scope emitter dropped the scope kind so ieee_float{} did
// not suspend the trap on the resolved path at all.

#[test]
fn audit_h11_pow_suspended_inside_ieee_float_both_backends() {
    // L1: `(-1.0) ** 0.5` is NaN under IEEE 754 — inside `ieee_float { }` it
    // must pass through on every backend. Pre-fix: VM (PowFloat) and resolved
    // (scope kind dropped) both trapped E0813; VM's error was "pow", resolved
    // emitted `mimi_trap_float_not_finite` at the call site.
    let src = r#"
func main() -> i32 {
    let mut p = 0.0
    ieee_float {
        p = (-1.0) ** 0.5
    }
    if is_nan(p) { println(1) } else { println(0) }
    0
}
"#;
    check_source(src).expect("ieee_float pow checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "1", "bytecode: pow NaN inside ieee_float");
    if !can_link() {
        return;
    }
    let resolved = checked_codegen_compile_and_run(src).expect("resolved ieee_float pow");
    assert_eq!(
        resolved.trim(),
        "1",
        "resolved codegen: pow NaN inside ieee_float (was E0813: scope kind dropped)"
    );
    let legacy = compile_and_run(src).expect("legacy ieee_float pow");
    assert_eq!(
        legacy.trim(),
        "1",
        "legacy codegen: pow NaN inside ieee_float"
    );
}

#[test]
fn audit_h11_pow_traps_outside_ieee_float_both_backends() {
    // Reverse guard: the finiteness trap is still ACTIVE outside the escape
    // hatch, on all three executors.
    let src = r#"
func main() -> i32 {
    let p = (-1.0) ** 0.5
    if is_nan(p) { println(1) } else { println(0) }
    0
}
"#;
    check_source(src).expect("pow outside ieee checks");
    let vm_res = run_source_result(src);
    assert!(vm_res.is_err(), "bytecode pow must trap outside ieee_float");
    if !can_link() {
        return;
    }
    let resolved = checked_codegen_compile_and_run(src);
    assert!(
        resolved.is_err(),
        "resolved codegen pow must trap outside ieee_float"
    );
    let legacy = compile_and_run(src);
    assert!(
        legacy.is_err(),
        "legacy codegen pow must trap outside ieee_float"
    );
}

#[test]
fn audit_h11_unary_neg_nan_suspended_inside_ieee_float_both_backends() {
    // Unary float negation cannot turn a finite value non-finite, and codegen
    // compiles it to a bare `0.0 - x` (no guard). So `-nan` must pass through
    // both inside and outside `ieee_float { }`. Pre-fix the VM's NegInt float
    // branch hard-trapped it inside the block (diverging from codegen).
    let src = r#"
func main() -> i32 {
    let mut n = 0.0
    ieee_float {
        let nan = 0.0 / 0.0
        n = -nan
    }
    if is_nan(n) { println(1) } else { println(0) }
    0
}
"#;
    check_source(src).expect("ieee_float unary neg checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "1",
        "bytecode: -NaN inside ieee_float must pass through"
    );
    if !can_link() {
        return;
    }
    let resolved = checked_codegen_compile_and_run(src).expect("resolved -NaN inside ieee");
    assert_eq!(resolved.trim(), "1", "resolved: -NaN inside ieee_float");
    let legacy = compile_and_run(src).expect("legacy -NaN inside ieee");
    assert_eq!(legacy.trim(), "1", "legacy: -NaN inside ieee_float");
}

// ─── D-2 — Set-method dispatch prefix hijack of user `Settings` type ───────
// legacy method.rs:210 dispatched to compile_set_method when obj_type
// `starts_with("Set")` || `starts_with("set")`. A user type whose name merely
// STARTS with "Set" (e.g. `Settings`), once its method name collided with the
// builtin set table (here: trait `size()`), would be compiled against
// mimi_set_size on a non-set struct (garbage/UB). The real set type boxes to
// `Set<T>`, so the guard is now exact: bare `set`/`Set` or a `Set<`
// instantiation.

#[test]
fn audit_d2_settings_method_not_hijacked_by_set_dispatch() {
    // A `Settings` struct's `size()` must run its trait impl on EVERY
    // backend. Pre-fix the legacy path would match `starts_with("Set")` and
    // call compile_set_method on the struct.
    let src = r#"
trait Sized {
    func size() -> i32
}
type Settings {
    n: i32
}
impl Sized for Settings {
    func size() -> i32 { 7 }
}
func main() -> i32 {
    let s = Settings { n: 1 }
    println(s.size())
    0
}
"#;
    check_source(src).expect("Settings trait size type checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "7", "bytecode: Settings.size() = 7");
    if !can_link() {
        return;
    }
    let resolved = checked_codegen_compile_and_run(src).expect("resolved Settings.size() must run");
    assert_eq!(
        resolved.trim(),
        "7",
        "resolved: Settings.size() not hijacked"
    );
    let legacy = compile_and_run(src).expect("legacy Settings.size() must run");
    assert_eq!(
        legacy.trim(),
        "7",
        "legacy: Settings.size() not hijacked by mimi_set_size (D-2)"
    );
}

#[test]
fn audit_d2_real_set_size_still_dispatches_to_builtin() {
    // Regression guard: a genuine `Set<T>` `.size()` must STILL route to the
    // builtin (mimi_set_size) on the legacy path (method.rs:210), now via the
    // exact `Set<` guard. (Resolved Set.SIZE dispatch is a SEPARATE gap — it
    // reports E0722 `{ptr,i64} → ptr` in resolved_emit, not D-2's prefix
    // hijack; asserted here only for the legacy compile path + VM.)
    let src = r#"
func main() -> i32 {
    let s = from_json::<Set<i32>>("[4,1,1]")
    println(s.size())
    0
}
"#;
    check_source(src).expect("real Set size type checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(vm_out.trim(), "2", "bytecode: Set size = 2 (dedup)");
    if !can_link() {
        return;
    }
    let legacy = compile_and_run(src).expect("legacy Set size");
    assert_eq!(
        legacy.trim(),
        "2",
        "legacy: genuine Set.size() still dispatches to builtin (D-2 guard is exact)"
    );
}

// ─── B-1 — str_parse_float must reject NaN/±Inf (SD-9 input boundary) ──────
// VM: `Ok(n) if n.is_finite()` — "NaN"/"inf" parse Ok but non-finite →
// (false, 0.0). codegen: build_parse_float_tuple had NO finiteness gate, so
// C strtod's successful "NaN"/"inf" parse produced (true, NaN/Inf) and the
// non-finite value entered the system (b1.mimi: VM nan:bad, native nan:ok —
// an L1 divergence). The gate now mirrors the VM arms: finite → ok, else
// (false, 0.0).

#[test]
fn audit_b1_str_parse_float_rejects_non_finite_both_backends() {
    let src = r#"
func main() -> i32 {
    let a = str_parse_float("NaN")
    let b = str_parse_float("inf")
    let c = str_parse_float("1.5")
    println(a.0)
    println(b.0)
    println(c.1)
    0
}
"#;
    check_source(src).expect("str_parse_float non-finite checks");
    let (_, vm_out) = run_source_with_stdout(src);
    assert_eq!(
        vm_out.trim(),
        "false\nfalse\n1.5",
        "bytecode: NaN/inf rejected, 1.5 accepted"
    );
    if !can_link() {
        return;
    }
    let resolved =
        checked_codegen_compile_and_run(src).expect("resolved str_parse_float non-finite");
    assert_eq!(
        resolved.trim(),
        "false\nfalse\n1.5",
        "resolved: NaN/inf rejected (was true/NaN — B-1 gate)"
    );
    let legacy = compile_and_run(src).expect("legacy str_parse_float non-finite");
    assert_eq!(
        legacy.trim(),
        "false\nfalse\n1.5",
        "legacy: NaN/inf rejected"
    );
}

// §6-#58 (audit 2026-08-05): match-guard must be evaluated ONLY after the
// pattern matched. Pre-fix, the resolved native emitter emitted the guard
// unconditionally on the fallthrough path, so a side effect inside the guard
// of a NON-matching arm still ran — diverging from the VM's sequential arm
// semantics.
#[test]
fn audit_58_match_guard_short_circuit() {
    let src = r#"
func probeA() -> bool { print("A"); true }
func probeB() -> bool { print("B"); true }
func main() -> i32 {
    let x = 5
    match x {
        1 if probeA() => { print("one") }
        5 if probeB() => { print("five") }
        _ => { print("none") }
    }
    0
}
"#;
    // x==5: arm `1` never matches so its guard probeA() must NOT run (no "A").
    // Only arm `5` matches and evaluates probeB() → "Bfive".
    let (_, vm_stdout) = run_source_with_stdout(src);
    assert_eq!(vm_stdout, "Bfive", "VM guard semantics");

    for (name, native) in [
        ("legacy", compile_and_run(src)),
        ("resolved", checked_codegen_compile_and_run(src)),
    ] {
        let out = native.unwrap_or_else(|e| panic!("{name} codegen: {e}"));
        assert_eq!(
            out, "Bfive",
            "native {name} match-guard short-circuit diverged; guard escaped arm"
        );
    }
}
