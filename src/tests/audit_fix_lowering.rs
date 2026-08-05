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
