//! Actor oracle acceptance tests. These use production checker/MIR admission,
//! retain the original regressions, and observe actual reference stdout.

use super::actor_reference::MirActorReferenceDomain;
use super::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
use super::{MirActorOperation, MirInstructionId, MirInstructionKind};
use crate::core::NodeId;

const CAPTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/real_world/actor_nested_func_capture.mimi"
));
const SHADOW: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/real_world/actor_nested_func_shadow.mimi"
));

fn canonical(source: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex Actor fixture");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse Actor fixture");
    let checked = crate::core::check_program(&file).expect("check Actor fixture");
    MirProgram::from_checked_program(&checked).expect("materialize canonical Actor fixture")
}

fn main_owner() -> NodeId {
    NodeId("function:main".into())
}

fn assert_closed(
    reference: &MirReferenceInterpreter<'_>,
    spawned: u64,
    completed: u64,
    failed: u64,
) {
    let lifecycle = reference.actor_lifecycle_for_test();
    assert!(!lifecycle.open);
    assert_eq!(lifecycle.live_instances, 0);
    assert_eq!(lifecycle.active_turns, 0);
    assert_eq!(lifecycle.spawned, spawned);
    assert_eq!(lifecycle.started, completed + failed);
    assert_eq!(lifecycle.completed, completed);
    assert_eq!(lifecycle.failed, failed);
}

#[test]
fn canonical_actor_graph_rejects_compile_time_callables_as_executable_bodies() {
    let source = r#"
comptime func compile_only() -> i32 { 7 }
actor Counter {
    func read() -> i32 { compile_only() }
}
func main() -> i32 {
    let counter = Counter.spawn()
    counter.read()
}
"#;
    let tokens = crate::lexer::Lexer::new(source)
        .tokenize()
        .expect("lex comptime Actor source");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse comptime Actor source");
    let checked = crate::core::check_program(&file).expect("check comptime Actor source");
    let error = MirProgram::from_checked_program(&checked)
        .expect_err("Actor graph cannot materialize a comptime body as executable MIR");
    assert!(
        error
            .to_string()
            .contains("compile-time-only callable 'function:compile_only'"),
        "unexpected Actor comptime diagnostic: {error}"
    );
}

#[test]
fn canonical_actor_reference_preserves_original_capture_stdout() {
    let program = canonical(CAPTURE);
    let reference = MirReferenceInterpreter::new(&program);
    let observation = reference
        .execute_with_output(&main_owner(), &[])
        .expect("Actor capture execution");
    assert_eq!(observation.value, MirRuntimeValue::Int(0));
    assert_eq!(observation.output, "15\n");
    assert_closed(&reference, 1, 1, 0);
}

#[test]
fn canonical_actor_reference_preserves_original_shadow_and_global_stdout() {
    let program = canonical(SHADOW);
    let reference = MirReferenceInterpreter::new(&program);
    let observation = reference
        .execute_with_output(&main_owner(), &[])
        .expect("Actor shadow execution");
    assert_eq!(observation.value, MirRuntimeValue::Int(0));
    assert_eq!(observation.output, "42\n101\n");
    assert_closed(&reference, 1, 1, 0);
}

#[test]
fn canonical_actor_nested_helpers_use_actor_environment_receipts() {
    for source in [CAPTURE, SHADOW] {
        let program = canonical(source);
        assert!(
            !program.actor_environments().is_empty(),
            "each original nested Actor helper has a dedicated lexical-environment receipt"
        );
        assert!(
            program.nested_callable_scopes().is_empty(),
            "Actor helpers must not be claimed by the ordinary nested-callable receipt"
        );
        program
            .validate_nested_callable_scope_receipts()
            .expect("ordinary nested-callable validation excludes Actor-specific helper owners");
    }
}

#[test]
fn canonical_actor_reference_repeated_mailbox_calls_and_root_replay_resets_it() {
    let source = CAPTURE.replace(
        "    println(r)",
        "    println(r)\n    let advanced2 = c.bump(7)\n    println(advanced2)",
    );
    let program = canonical(&source);
    let reference = MirReferenceInterpreter::new(&program);
    for _ in 0..2 {
        let observation = reference
            .execute_with_output(&main_owner(), &[])
            .expect("repeat Actor turns");
        assert_eq!(observation.output, "15\n17\n");
        assert_closed(&reference, 1, 2, 0);
    }
}

#[test]
fn canonical_actor_bytecode_repeated_mailbox_calls_settle_without_ast() {
    let source = CAPTURE.replace(
        "    println(r)",
        "    println(r)\n    let advanced2 = c.bump(7)\n    println(advanced2)",
    );
    let program = canonical(&source);
    let bytecode = crate::interp::bytecode::compile_mir_program(&program)
        .expect("AST-free Bytecode compiles checked nested-helper Actor calls");
    assert!(bytecode.ast.is_none());
    let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
    vm.run().expect("two nested-helper Actor mailbox turns");
    assert_eq!(vm.stdout(), "15\n17\n");
    let counts = vm
        .canonical_actor_domain_counts()
        .expect("Flow Actor domain lifecycle is observed");
    assert_eq!(counts.accepted_messages, 2);
    assert_eq!(counts.settled_replies, 2);
    assert_eq!(counts.joined_workers, 1);
    assert_eq!(counts.live_roots, 0);
    assert_eq!(counts.live_workers, 0);
}

#[test]
fn canonical_actor_reference_two_instances_keep_nonzero_initial_state_independently() {
    let program = canonical(
        r#"
flow CounterFlow {
    state Active { base: i32 }
    transition seed(Active, value: i32) -> Active {
        return Active { base: value }
    }
    transition add(Active, amount: i32) -> Active {
        return Active { base: self.base + amount }
    }
}
actor Counter runs CounterFlow {
    func bump(delta: i32) -> i32 {
        let scale = 10
        func scaled(d: i32) -> i32 { scale + d }
        scaled(delta)
    }
}
func main() -> i32 {
    let a = Counter.spawn()
    let b = Counter.spawn()
    let a_initial = a.seed(9)
    let b_initial = b.seed(9)
    let a_seed = a_initial.base
    let b_seed = b_initial.base
    let first = a.add(a.bump(5) + a_seed - 9)
    let second = b.add(b.bump(7) + b_seed - 9)
    let third = a.add(a.bump(7))
    println(first.base)
    println(second.base)
    println(third.base)
    0
}
"#,
    );
    let reference = MirReferenceInterpreter::new(&program);
    let observation = reference
        .execute_with_output(&main_owner(), &[])
        .expect("isolated Actor instances");
    assert_eq!(observation.output, "24\n26\n41\n");
    assert_closed(&reference, 2, 8, 0);
}

#[test]
fn canonical_actor_reference_two_capture_environment_is_bound_to_lexical_helper() {
    let program = canonical(
        r#"
func scaled(x: i32) -> i32 { x + 100 }
flow CounterFlow {
    state Active { base: i32 }
    transition add(Active, amount: i32) -> Active {
        return Active { base: self.base + amount }
    }
}
actor Counter runs CounterFlow {
    func bump(delta: i32) -> i32 {
        let scale = 10
        let bias = 3
        func scaled(d: i32) -> i32 { scale + bias + d }
        scaled(delta)
    }
}
func main() -> i32 {
    let a = Counter.spawn()
    let first = a.add(a.bump(5))
    let second = a.add(a.bump(7))
    println(first.base)
    println(second.base)
    println(scaled(1))
    0
}
"#,
    );
    let reference = MirReferenceInterpreter::new(&program);
    let observation = reference
        .execute_with_output(&main_owner(), &[])
        .expect("two Actor captures");
    assert_eq!(observation.output, "18\n38\n101\n");
    assert_closed(&reference, 1, 4, 0);
}

#[test]
fn canonical_actor_reference_method_trap_settles_turn_and_closes_root() {
    let program = canonical(
        r#"
actor Counter {
    func divide(delta: i32) -> i32 {
        9 / delta
    }
}
func main() -> i32 {
    let c = Counter.spawn()
    println(c.divide(3))
    c.divide(0)
}
"#,
    );
    let reference = MirReferenceInterpreter::new(&program);
    for _ in 0..2 {
        let error = reference
            .execute(&main_owner(), &[])
            .expect_err("Actor division must trap");
        assert!(error.message.contains("division by zero"), "{error}");
        assert_eq!(reference.captured_output(), "3\n");
        assert_closed(&reference, 1, 1, 1);
    }
}

#[test]
fn canonical_actor_reference_root_helper_trap_closes_live_actor_state() {
    let program = canonical(
        r#"
func divide(x: i32) -> i32 { 9 / x }
actor Counter {
    base: i32 = 4;
    func read() -> i32 { self.base }
}
func main() -> i32 {
    let c = Counter.spawn()
    println(c.read())
    divide(0)
}
"#,
    );
    let reference = MirReferenceInterpreter::new(&program);
    let error = reference
        .execute(&main_owner(), &[])
        .expect_err("root helper division must trap");
    assert!(error.message.contains("division by zero"), "{error}");
    assert_eq!(reference.captured_output(), "4\n");
    assert_closed(&reference, 1, 1, 0);
}

#[test]
fn canonical_actor_reference_step_limit_closes_spawned_state_before_first_turn() {
    let program = canonical(CAPTURE);
    let function = program
        .functions()
        .get(&main_owner())
        .expect("root function");
    let entry = function
        .blocks
        .get(&function.entry)
        .expect("root entry block");
    let spawn_offset = entry
        .instructions
        .iter()
        .position(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Actor {
                    operation: MirActorOperation::Spawn { .. }
                }
            )
        })
        .expect("root entry spawn");
    // One step enters the block; the inclusive instruction budget runs spawn
    // and then stops before the root can enter its first mailbox turn.
    let reference = MirReferenceInterpreter::new(&program).with_step_limit(spawn_offset + 2);
    let error = reference
        .execute(&main_owner(), &[])
        .expect_err("step budget must stop the root");
    assert!(error.message.contains("step limit"), "{error}");
    assert_eq!(reference.captured_output(), "");
    assert_closed(&reference, 1, 0, 0);
}

fn operation_matching(
    program: &MirProgram,
    predicate: impl Fn(&MirActorOperation) -> bool,
) -> (MirInstructionId, MirActorOperation) {
    program
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Actor { operation } if predicate(operation) => {
                Some((instruction.id.clone(), operation.clone()))
            }
            _ => None,
        })
        .expect("fixture Actor operation")
}

fn assert_preflight_rejects(
    mut program: MirProgram,
    id: &MirInstructionId,
    replacement: MirActorOperation,
) {
    program.replace_actor_operation_for_test_only(id, replacement);
    let reference = MirReferenceInterpreter::new(&program);
    reference
        .execute(&main_owner(), &[])
        .expect_err("forged Actor graph must fail preflight");
    assert_eq!(
        reference.captured_output(),
        "",
        "a forged graph must be rejected before stdout"
    );
    assert_closed(&reference, 0, 0, 0);
}

#[test]
fn canonical_actor_reference_rejects_forged_field_method_and_handle_roles_before_output() {
    let program = canonical(CAPTURE);
    let (id, mut operation) = operation_matching(&program, |operation| {
        matches!(operation, MirActorOperation::Call { .. })
    });
    if let MirActorOperation::Call { method, .. } = &mut operation {
        *method = NodeId("method:forged".into());
    }
    assert_preflight_rejects(program.clone(), &id, operation);

    let (id, mut operation) = operation_matching(&program, |operation| {
        matches!(operation, MirActorOperation::Call { .. })
    });
    if let MirActorOperation::Call { handle, args, .. } = &mut operation {
        *handle = args[0].clone();
    }
    assert_preflight_rejects(program.clone(), &id, operation);

    let field_program = canonical(
        r#"
        actor Reader {
            base: i32 = 3;
            func read(delta: i32) -> i32 { self.base + delta }
        }
        func main() -> i32 {
            let r = Reader.spawn()
            println(r.read(4))
            0
        }
    "#,
    );
    let (id, mut operation) = operation_matching(&field_program, |operation| {
        matches!(operation, MirActorOperation::FieldLoad { .. })
    });
    if let MirActorOperation::FieldLoad { field, .. } = &mut operation {
        *field = NodeId("field:forged".into());
    }
    assert_preflight_rejects(field_program.clone(), &id, operation);

    let (id, operation) = operation_matching(&field_program, |operation| {
        matches!(operation, MirActorOperation::FieldLoad { .. })
    });
    let MirActorOperation::FieldLoad {
        result,
        actor,
        field,
        state,
    } = operation
    else {
        unreachable!()
    };
    assert_preflight_rejects(
        field_program,
        &id,
        MirActorOperation::FieldStore {
            actor,
            field,
            state: result,
            value: state,
        },
    );
}

#[test]
fn canonical_actor_reference_dynamic_roles_and_stale_turns_fail_closed() {
    let program = canonical(CAPTURE);
    let (_, actor) = program
        .actors()
        .iter()
        .next()
        .expect("fixture Actor contract");
    let method = &actor.methods[0];
    let field = &actor.fields[0].field;
    let mut domain = MirActorReferenceDomain::default();
    domain.open().expect("open Actor domain");
    let handle = domain
        .spawn(program.actors(), &actor.actor)
        .expect("spawn reference Actor");
    assert!(domain
        .field_load(
            program.actors(),
            &method.owner,
            &actor.actor,
            field,
            &handle
        )
        .expect_err("handle is not state")
        .contains("internal state role"));
    let turn = domain
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &handle,
            vec![MirRuntimeValue::Int(5)],
        )
        .expect("prepare Actor turn");
    let state = turn.arguments[0].clone();
    assert!(domain
        .field_load(
            program.actors(),
            &NodeId("function:main".into()),
            &actor.actor,
            field,
            &state
        )
        .is_err());
    assert!(domain
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &state,
            vec![MirRuntimeValue::Int(5)]
        )
        .is_err());
    domain
        .field_store(
            program.actors(),
            &method.owner,
            &actor.actor,
            field,
            &state,
            &MirRuntimeValue::Int(15),
        )
        .expect("persistent store");
    assert_eq!(
        domain
            .field_load(program.actors(), &method.owner, &actor.actor, field, &state)
            .unwrap(),
        MirRuntimeValue::Int(15)
    );
    domain
        .finish_call(&turn, &Ok(MirRuntimeValue::Int(15)))
        .expect("settle Actor turn");
    assert!(domain
        .field_load(program.actors(), &method.owner, &actor.actor, field, &state)
        .is_err());
    assert!(domain
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &state,
            vec![MirRuntimeValue::Int(5)]
        )
        .expect_err("state is not handle")
        .contains("external handle role"));
    let next = domain
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &handle,
            vec![MirRuntimeValue::Int(7)],
        )
        .expect("next turn");
    assert_eq!(
        domain
            .field_load(
                program.actors(),
                &method.owner,
                &actor.actor,
                field,
                &next.arguments[0]
            )
            .unwrap(),
        MirRuntimeValue::Int(15)
    );
    assert!(
        domain
            .field_load(program.actors(), &method.owner, &actor.actor, field, &state)
            .is_err(),
        "prior turn state cannot authorize a new turn"
    );
    domain
        .finish_call(&next, &Ok(MirRuntimeValue::Int(15)))
        .expect("settle next turn");
    domain.close();
    domain.close();
    let closed = domain.lifecycle();
    assert!(!closed.open);
    assert_eq!(closed.live_instances, 0);
    assert_eq!(closed.active_turns, 0);
    domain.open().expect("new root");
    let fresh = domain
        .spawn(program.actors(), &actor.actor)
        .expect("same Actor new root instance");
    assert!(domain
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &handle,
            vec![MirRuntimeValue::Int(7)]
        )
        .expect_err("old root handle")
        .contains("another root"));
    let cancelled = domain
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &fresh,
            vec![MirRuntimeValue::Int(7)],
        )
        .expect("accepted synchronous turn before cancellation");
    domain.close();
    assert_eq!(domain.lifecycle().failed, 1);
    assert_eq!(domain.lifecycle().active_turns, 0);
    assert!(domain
        .field_load(
            program.actors(),
            &method.owner,
            &actor.actor,
            field,
            &cancelled.arguments[0],
        )
        .is_err());
    domain.close();
    assert_eq!(
        domain.lifecycle().failed,
        1,
        "close cannot settle the same turn twice"
    );
}

#[test]
fn canonical_actor_reference_domains_reject_foreign_handles_and_invalid_payloads() {
    let program = canonical(CAPTURE);
    let (_, actor) = program.actors().iter().next().unwrap();
    let method = &actor.methods[0];
    let mut first = MirActorReferenceDomain::default();
    let mut second = MirActorReferenceDomain::default();
    first.open().unwrap();
    second.open().unwrap();
    let handle = first.spawn(program.actors(), &actor.actor).unwrap();
    second.spawn(program.actors(), &actor.actor).unwrap();
    assert!(second
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &handle,
            vec![MirRuntimeValue::Int(5)]
        )
        .is_err());
    assert!(first
        .prepare_call(
            program.actors(),
            &actor.actor,
            &method.owner,
            &handle,
            vec![MirRuntimeValue::Int(i64::MAX)]
        )
        .is_err());
    assert!(first
        .prepare_call(
            program.actors(),
            &actor.actor,
            &NodeId("method:forged".into()),
            &handle,
            vec![MirRuntimeValue::Int(5)]
        )
        .is_err());
    assert_eq!(first.lifecycle().started, 0);
    first.close();
    second.close();
}

#[test]
fn canonical_actor_route_shared_graph_public_consumers_and_no_proof() {
    for (source, expected) in [(CAPTURE, "15\n"), (SHADOW, "42\n101\n")] {
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("Actor lexer");
        let mut file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("Actor parser");
        crate::loader::merge_prelude_into(&mut file);
        let checked = crate::core::check_program(&file).expect("Actor checker");
        let route =
            super::materialize_canonical_mir_route(&checked, None).expect("shared Actor route");
        assert!(route.admission.scalar_actor);
        assert!(super::CanonicalMirRouteProfile::ScalarActor.is_materialized(&route));
        let excluded = checked
            .source_registry()
            .records()
            .iter()
            .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
            .map(|record| record.id)
            .collect::<std::collections::HashSet<_>>();
        let explicit = MirProgram::from_checked_program_excluding_sources(&checked, &excluded)
            .expect("explicit Actor graph");
        assert_eq!(
            route.program.canonical_digest(),
            explicit.canonical_digest()
        );
        let receipt = route.program.route_receipt(super::SCALAR_ACTOR_ISLAND);
        assert_eq!(receipt.manifest_round_trip().unwrap(), receipt);
        let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(&receipt);
        assert_eq!(
            reference
                .execute_with_output(&main_owner(), &[])
                .unwrap()
                .output,
            expected
        );
        assert_closed(&reference, 1, 1, 0);
        let bytecode = crate::interp::bytecode::compile_mir_program_with_route_receipt(
            &route.program,
            &receipt,
        )
        .unwrap();
        assert!(bytecode.ast.is_none());
        assert!(bytecode.actor_defs.is_empty() && bytecode.actor_method_funcs.is_empty());
        let mut vm = crate::interp::bytecode::BytecodeVM::new(bytecode);
        vm.run().expect("same receipted Actor VM graph");
        assert_eq!(vm.stdout(), expected);
        let counts = vm.canonical_actor_domain_counts().unwrap();
        assert_eq!(
            (
                counts.live_roots,
                counts.live_workers,
                counts.live_states,
                counts.live_controls,
                counts.live_tasks,
                counts.pending_replies,
                counts.live_message_buffers,
                counts.live_reply_buffers
            ),
            (0, 0, 0, 0, 0, 0, 0, 0)
        );
        assert_eq!(counts.joined_workers, 1);
        for results in [
            crate::verifier::verify_checked(&checked, "actor-route-original".into()).unwrap(),
            crate::verifier::verify_checked_dual(&checked, "actor-route-original".into()).unwrap(),
            crate::verifier::verify_mir_with_route_receipt(
                &route.program,
                &receipt,
                "actor-route-original".into(),
            )
            .unwrap(),
        ] {
            assert!(
                results.is_empty(),
                "Actor NoContracts must stay neutral; no Proven/artifact may be synthesized"
            );
        }
        let mut configurable = crate::verifier::Verifier::new().expect("configured verifier host");
        configurable.set_source_hash("actor-route-original".into());
        assert!(configurable.verify_checked(&checked).is_empty());
        let mut forged_receipt = receipt.clone();
        forged_receipt.mir_digest = "0".repeat(64);
        assert!(crate::verifier::verify_mir_with_route_receipt(
            &route.program,
            &forged_receipt,
            "actor-route-original".into()
        )
        .is_err());
        let mut forged_program = route.program.clone();
        let mut function = forged_program.functions()[&main_owner()].clone();
        function.contracts.push(super::MirContract {
            id: NodeId("forged:actor-static-proof".into()),
            kind: super::MirContractKind::Ensures,
            condition: super::MirContractExpr::Bool(true),
            expression_types: std::collections::BTreeMap::from([(
                Vec::new(),
                crate::core::ir::primitive_type_id(crate::core::PrimitiveType::Bool),
            )]),
            ffi_helper_call: None,
        });
        forged_program.replace_function_for_test_only(function);
        assert!(
            crate::verifier::verify_mir(&forged_program, "actor-route-original".into()).is_err(),
            "mailbox graph plus injected contract cannot produce a proof"
        );
    }
}

#[test]
fn canonical_actor_route_retains_top_level_contracts_and_rejects_embedded_contracts() {
    for (suffix, expected_proof) in [
        (
            "func guarded(x: i32) -> i32 {\n requires: x > 0\n ensures: result == x\n x\n}",
            Some(true),
        ),
        (
            "func guarded(x: i32) -> i32 {\n requires: x > 0\n ensures: result != x\n x\n}",
            Some(false),
        ),
        (
            "func identity_i32(x: i32) -> i32 { x }\nfunc guarded(x: i32) -> i32 { identity_i32({\n ensures: false\n x\n}) }",
            None,
        ),
        (
            "func guarded(x: i32) -> i32 { x + {\n ensures: false\n 0\n} }",
            None,
        ),
    ] {
        for original in [CAPTURE, SHADOW] {
            let source = format!("{original}\n{suffix}");
            let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
            let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
            crate::loader::merge_prelude_into(&mut file);
            let checked = match crate::core::check_program(&file) {
                Ok(checked) => {
                    assert!(
                        expected_proof.is_some(),
                        "embedded contract is rejected by the top-level contract rule"
                    );
                    checked
                }
                Err(diagnostics) => {
                    assert!(
                        expected_proof.is_none(),
                        "valid top-level contract must remain accepted: {diagnostics:?}"
                    );
                    assert!(diagnostics.iter().any(|diagnostic| {
                        diagnostic.code.as_deref() == Some("E0503")
                            && diagnostic
                                .message
                                .contains("top-level callable-body contract")
                    }));
                    continue;
                }
            };
            let owner = NodeId("function:guarded".into());
            let callable = checked
                .callable(&owner)
                .expect("whole checked callable owner");
            assert!(callable.has_proof_obligations());
            assert!(
                !callable.contracts.is_empty(),
                "Call/Binary descendants must remain in the checker contract census"
            );
            assert!(
                super::scalar_actor_contract_boundary_reason(&checked)
                    .unwrap()
                    .contains("guarded")
            );
            assert!(!super::classify_canonical_mir_route_admission(&checked).scalar_actor);
            let excluded = checked
                .source_registry()
                .records()
                .iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id)
                .collect::<std::collections::HashSet<_>>();
            assert!(
                MirProgram::from_checked_program_excluding_sources(&checked, &excluded)
                    .unwrap_err()
                    .to_string()
                    .contains("cannot discard proof obligations")
            );
            let mut configured = crate::verifier::Verifier::new().unwrap();
            configured.set_source_hash("whole-actor-user-contract-census".into());
            for results in [
                crate::verifier::verify_checked(
                    &checked,
                    "whole-actor-user-contract-census".into(),
                )
                .unwrap(),
                crate::verifier::verify_checked_dual(
                    &checked,
                    "whole-actor-user-contract-census".into(),
                )
                .unwrap(),
                configured.verify_checked(&checked),
            ] {
                let guarded = results
                    .iter()
                    .find(|result| result.func_name == "guarded")
                    .expect("default public verifier must retain the dormant owner obligation");
                assert_ne!(guarded.status, crate::verifier::VerifStatus::NoObligations);
                if expected_proof == Some(true) {
                    assert_eq!(
                        guarded.status,
                        crate::verifier::VerifStatus::Proven,
                        "supported original user proof must remain supported: {guarded:?}"
                    );
                } else {
                    assert_ne!(
                        guarded.status,
                        crate::verifier::VerifStatus::Proven,
                        "false embedded/unused claim cannot be proved: {guarded:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn canonical_actor_proof_census_retains_dormant_nested_impl_flow_and_declaration_owners() {
    for (suffix, owner_fragment, callable_body_contract) in [
        (
            "func outer() -> i32 {\nfunc guarded(x: i32) -> i32 {\nensures: false\nx\n}\n0\n}",
            "guarded",
            true,
        ),
        (
            "trait ProofTrait { func check(x: i32) -> i32 }\ntype ProofProbe { marker: i32 }\nimpl ProofTrait for ProofProbe { func check(x: i32) -> i32 {\nensures: false\nx\n} }",
            "check",
            true,
        ),
        (
            "flow Hidden {\nstate A { n: i32 }\ntransition tick(A) -> A {\nensures: false\nreturn A { n: self.n }\n}\n}",
            "Hidden",
            true,
        ),
        (
            "actor Dormant { n: i32 = 0;\nfunc read() -> i32 {\nensures: false\nself.n\n}\n}",
            "Dormant",
            true,
        ),
        ("const VALUE: i32 = {\nensures: false\n1\n}", "VALUE", false),
    ] {
        let source = format!("{CAPTURE}\n{suffix}");
        let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
        let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        crate::loader::merge_prelude_into(&mut file);
        let checked = match crate::core::check_program(&file) {
            Ok(checked) => {
                assert!(
                    callable_body_contract,
                    "constant initializer contracts are not callable-body contracts"
                );
                checked
            }
            Err(diagnostics) => {
                assert!(
                    !callable_body_contract,
                    "valid callable-body owner must remain accepted: {diagnostics:?}"
                );
                assert!(diagnostics.iter().any(|diagnostic| {
                    diagnostic.code.as_deref() == Some("E0503")
                        && diagnostic
                            .message
                            .contains("top-level callable-body contract")
                }));
                continue;
            }
        };
        assert!(super::scalar_actor_contract_boundary_reason(&checked).is_some());
        assert!(!super::classify_canonical_mir_route_admission(&checked).scalar_actor);
        let mut configured = crate::verifier::Verifier::new().unwrap();
        configured.set_source_hash("actor-whole-owner-census".into());
        for results in [
            crate::verifier::verify_checked(&checked, "actor-whole-owner-census".into()).unwrap(),
            crate::verifier::verify_checked_dual(&checked, "actor-whole-owner-census".into())
                .unwrap(),
            configured.verify_checked(&checked),
        ] {
            let obligation = results
                .iter()
                .find(|result| result.func_name.contains(owner_fragment))
                .unwrap_or_else(|| {
                    panic!(
                        "typed owner {owner_fragment} must retain a proof obligation: {results:?}"
                    )
                });
            assert_ne!(
                obligation.status,
                crate::verifier::VerifStatus::NoObligations
            );
            assert_ne!(
                obligation.status,
                crate::verifier::VerifStatus::Proven,
                "false dormant claim cannot be proved"
            );
        }
    }
}

#[test]
fn canonical_actor_compatibility_preserves_called_ffi_requires_and_defers_foreign_ensures() {
    let source = format!(
        r#"{CAPTURE}
extern "C" {{
    func actor_ffi_probe(value: i64) -> i64
        requires: value > 0
        ensures: result == value;
    func actor_ffi_unused(value: i64) -> i64
        ensures: result == value;
}}
func actor_ffi_positive(value: i64) -> i64 {{
    requires: value > 0
    actor_ffi_probe(value)
}}
func actor_ffi_negative() -> i64 {{
    actor_ffi_probe(0 as i64)
}}
func actor_ffi_twice(value: i64) -> i64 {{
    requires: value > 0
    actor_ffi_probe(value);
    actor_ffi_probe(value)
}}
"#
    );
    let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
    let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    crate::loader::merge_prelude_into(&mut file);
    let checked = crate::core::check_program(&file).expect("mixed Actor/FFI source checks");
    let twice_sites = checked
        .call_sites_sorted()
        .into_iter()
        .filter(|site| site.owner == "function:actor_ffi_twice" && site.callee == "actor_ffi_probe")
        .collect::<Vec<_>>();
    assert_eq!(twice_sites.len(), 2, "Checker preserves both source calls");
    assert!(super::scalar_actor_contract_boundary_reason(&checked).is_some());
    let admission = super::classify_canonical_mir_route_admission(&checked);
    assert!(
        !super::CanonicalMirRouteProfile::ScalarActor.is_admitted(admission),
        "verifier-only FFI caller materialization must not admit Actor execution"
    );

    let mut configured = crate::verifier::Verifier::new().expect("configured verifier host");
    configured.set_source_hash("actor-mixed-ffi-contracts".into());
    let batches = [
        crate::verifier::verify_checked(&checked, "actor-mixed-ffi-contracts".into()).unwrap(),
        crate::verifier::verify_checked_dual(&checked, "actor-mixed-ffi-contracts".into()).unwrap(),
        configured.verify_checked(&checked),
        crate::verifier::verify_ffi_checked_with_source_hash(
            &checked,
            "actor-mixed-ffi-contracts".into(),
        )
        .unwrap(),
    ];

    for (batch_index, results) in batches.into_iter().enumerate() {
        let positive = results
            .iter()
            .find(|result| result.func_name == "actor_ffi_positive calls actor_ffi_probe")
            .unwrap_or_else(|| panic!("missing positive FFI call-site check: {results:?}"));
        assert_eq!(
            positive.status,
            crate::verifier::VerifStatus::Proven,
            "batch {batch_index}: {results:#?}"
        );
        let positive_artifact = positive
            .artifact
            .as_ref()
            .expect("proved FFI Requires retains a canonical MIR proof artifact");
        assert_eq!(
            positive_artifact.engine,
            crate::verifier::ProofArtifact::ENGINE_MIR
        );
        let positive_receipt = positive_artifact
            .mir_route_receipt
            .as_ref()
            .expect("proved FFI Requires retains its verifier-only route receipt");
        assert_eq!(
            positive_receipt.profile, "verifier-ffi-owner-slice-v1",
            "ordinary FFI caller proof is materialized independently of Actor execution"
        );
        assert_eq!(positive_artifact.source_hash, "actor-mixed-ffi-contracts");
        let negative = results
            .iter()
            .find(|result| result.func_name == "actor_ffi_negative calls actor_ffi_probe")
            .unwrap_or_else(|| panic!("missing negative FFI call-site check: {results:?}"));
        assert_eq!(negative.status, crate::verifier::VerifStatus::Disproven);

        for site in &twice_sites {
            let exact_instruction = format!("'inst:call:{}'", site.node_id.0);
            let twice_result = results
                .iter()
                .filter(|result| {
                    result.message.contains(&exact_instruction)
                        && result.message.contains("extern requires")
                })
                .collect::<Vec<_>>();
            assert_eq!(
                twice_result.len(),
                1,
                "exact source call {} has one Requires result: {results:#?}",
                site.node_id.0
            );
            assert_eq!(
                twice_result[0].func_name,
                "actor_ffi_twice calls actor_ffi_probe"
            );
            assert_eq!(
                twice_result[0].status,
                crate::verifier::VerifStatus::Proven,
                "each call in one owner is proved independently: {twice_result:#?}"
            );
        }

        let foreign_ensures = results
            .iter()
            .filter(|result| result.func_name.contains("extern actor_ffi_probe Ensures"))
            .collect::<Vec<_>>();
        assert_eq!(
            foreign_ensures.len(),
            4,
            "one foreign Ensures observation per actual call"
        );
        assert!(foreign_ensures.iter().all(|result| {
            result.status == crate::verifier::VerifStatus::NotInTrustedSubset
                && result.artifact.is_none()
        }));
        assert!(
            !results
                .iter()
                .any(|result| result.func_name == "extern actor_ffi_probe"),
            "foreign declaration consistency is not an implementation proof"
        );
        assert!(
            !results
                .iter()
                .any(|result| result.func_name.contains("actor_ffi_unused")),
            "an uncalled foreign contract has no call-site proof obligation"
        );
    }
}

#[test]
fn canonical_actor_ffi_actor_method_boundary_is_per_callsite() {
    let source = r#"
extern "C" {
    func actor_ffi_probe(value: i32) -> i32
        requires: value > 0
        ensures: result == value;
}
actor Counter {
    base: i32 = 0;
    func check(value: i32) -> i32 {
        actor_ffi_probe(value)
    }
}
func actor_ffi_free(value: i32) -> i32 {
    requires: value > 0
    actor_ffi_probe(value)
}
func main() -> i32 { 0 }
"#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
    let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    crate::loader::merge_prelude_into(&mut file);
    let checked = crate::core::check_program(&file).expect("Actor method FFI source checks");
    let actor_method = checked
        .actor_descriptors()
        .values()
        .flat_map(|descriptor| descriptor.methods.iter())
        .find(|method| method.name == "check")
        .expect("Actor method descriptor")
        .owner
        .clone();
    let actor_callsite = checked
        .call_sites_sorted()
        .into_iter()
        .find(|site| site.owner == actor_method.0 && site.callee == "actor_ffi_probe")
        .expect("Checker preserves the Actor method's exact FFI call site")
        .node_id
        .0
        .clone();

    let results = crate::verifier::verify_ffi_checked_with_source_hash(
        &checked,
        "actor-method-ffi-callsite-boundary".into(),
    )
    .expect("FFI verifier retains per-owner results");
    let free = results
        .iter()
        .find(|result| result.func_name == "actor_ffi_free calls actor_ffi_probe")
        .expect("isolated free-function FFI result");
    assert_eq!(
        free.status,
        crate::verifier::VerifStatus::Proven,
        "{results:#?}"
    );
    assert!(
        free.artifact.as_ref().is_some_and(|artifact| {
            artifact.engine == crate::verifier::ProofArtifact::ENGINE_MIR
                && artifact
                    .mir_route_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.profile == "verifier-ffi-owner-slice-v1")
        }),
        "free-function proof carries its independent verifier-only MIR route receipt"
    );

    let method_result = results
        .iter()
        .find(|result| result.func_name == "Counter::check calls actor_ffi_probe")
        .expect("Actor method FFI Requires remains visible");
    assert_eq!(
        method_result.status,
        crate::verifier::VerifStatus::NotInTrustedSubset,
        "Actor method owner {} is not an isolated free-function proof slice: {results:#?}",
        actor_method.0
    );
    assert!(method_result.artifact.is_none());
    assert!(method_result.message.contains(&actor_method.0));
    assert!(method_result.message.contains(&actor_callsite));
    assert!(method_result.message.contains("exact call site"));

    let ensures = results
        .iter()
        .filter(|result| {
            result
                .func_name
                .contains("-> extern actor_ffi_probe Ensures")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ensures.len(),
        2,
        "each call site keeps a foreign Ensures row"
    );
    assert!(ensures.iter().all(|result| {
        result.status == crate::verifier::VerifStatus::NotInTrustedSubset
            && result.artifact.is_none()
    }));
}

#[test]
fn canonical_actor_compatibility_does_not_bypass_mixed_map_ffi_owner_gate() {
    const FFI_AND_MAP: &str = r#"
extern "C" {
    func scalar_probe(value: i32) -> i32 requires: value >= 0;
}
"#;
    const USED_ACTOR_MAIN: &str = r#"
func main() -> i32 {
    let counter = Counter.spawn()
    let actor_value = counter.bump(5)
    match map_new() {
        Ok(root) => {
            match map_set(root, "answer", scalar_probe(41 as i32)) {
                Ok(updated) => {
                    let observed = match map_get_i32(updated, "answer") {
                        Ok(value) => value,
                        Err(code) => code,
                    }
                    let size = map_size(updated)
                    drop(updated)
                    println(actor_value)
                    actor_value + size + observed
                },
                Err(code) => code,
            }
        },
        Err(code) => code,
    }
}
"#;
    const UNUSED_ACTOR_MAIN: &str = r#"
func main() -> i32 {
    match map_new() {
        Ok(root) => {
            match map_set(root, "answer", scalar_probe(41 as i32)) {
                Ok(updated) => {
                    let observed = match map_get_i32(updated, "answer") {
                        Ok(value) => value,
                        Err(code) => code,
                    }
                    let size = map_size(updated)
                    drop(updated)
                    size + observed
                },
                Err(code) => code,
            }
        },
        Err(code) => code,
    }
}
"#;
    let actor_prefix = CAPTURE
        .split("\nfunc main() -> i32 {")
        .next()
        .expect("original Actor fixture has its main entry");

    for (label, main) in [
        ("used-actor", USED_ACTOR_MAIN),
        ("unused-actor", UNUSED_ACTOR_MAIN),
    ] {
        let source = format!("{actor_prefix}\n{FFI_AND_MAP}\n{main}");
        let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
        let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        crate::loader::merge_prelude_into(&mut file);
        let checked = crate::core::check_program(&file)
            .unwrap_or_else(|error| panic!("{label} mixed Actor/Map/FFI checks: {error:?}"));
        let admission = super::classify_canonical_mir_route_admission(&checked);
        assert!(admission.scalar_ffi, "{label}: {admission:?}");
        assert!(
            admission.map_root.is_candidate()
                || admission.map_any_owner.is_candidate()
                || admission
                    .ordinary_map_owner_lifecycle_i32_string_v1
                    .has_profile_signal()
                || admission
                    .ordinary_map_owned_get_i32_string_v1
                    .has_profile_signal(),
            "{label} must be recognized as an independent Map owner attempt: {admission:?}"
        );
        assert!(
            admission
                .ordinary_map_owner_lifecycle_i32_string_v1
                .has_profile_signal()
                || admission
                    .ordinary_map_owned_get_i32_string_v1
                    .has_profile_signal(),
            "{label} must specifically include an ordinary Map-owner lifecycle: {admission:?}"
        );
        assert!(
            super::scalar_actor_contract_boundary_reason(&checked).is_some(),
            "{label} FFI contract must select the Actor proof compatibility boundary"
        );

        match crate::verifier::verify_ffi_checked(&checked) {
            Ok(direct) => {
                let direct_requirement = direct
                    .iter()
                    .find(|result| result.func_name == "main calls scalar_probe")
                    .unwrap_or_else(|| {
                        panic!("{label}: missing dedicated FFI result: {direct:?}")
                    });
                assert_eq!(
                    direct_requirement.status,
                    crate::verifier::VerifStatus::NotInTrustedSubset
                );
                assert!(direct_requirement.artifact.is_none());
                assert!(direct_requirement.message.contains("Map profile"));
            }
            Err(error) => assert!(
                error.contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                    && error.contains("ordinary Map/Any owner-v2")
                    && error.contains("cannot be combined")
                    && error.contains("scalar FFI"),
                "{label}: verifier must reject the unreceipted mixed owner composition explicitly: {error}"
            ),
        }

        for (entry, result) in [
            (
                "single",
                crate::verifier::verify_checked(&checked, format!("{label}-single")),
            ),
            (
                "dual",
                crate::verifier::verify_checked_dual(&checked, format!("{label}-dual")),
            ),
        ] {
            let results = match result {
                Ok(results) => results,
                Err(error) => {
                    assert!(
                        error.contains(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE),
                        "{label}/{entry}: mixed owner rejection must be an explicit MIR boundary: {error}"
                    );
                    continue;
                }
            };
            let requirement = results
                .iter()
                .find(|result| result.func_name == "main calls scalar_probe")
                .unwrap_or_else(|| {
                    panic!("{label}: missing named mixed-owner obligation: {results:?}")
                });
            assert_eq!(
                requirement.status,
                crate::verifier::VerifStatus::NotInTrustedSubset,
                "{label}: an Actor compatibility route must not bypass the Map/FFI receipt guard"
            );
            assert!(requirement.artifact.is_none());
            assert!(requirement.message.contains("Map profile"));
        }
    }
}

#[test]
fn checker_rejects_nested_requires_before_canonical_proof_census() {
    let source = "func guarded(x: i32) -> i32 {\nensures: false\nx + {\nrequires: false\n0\n}\n}\nfunc main() -> i32 { 0 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
    let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    let diagnostics = crate::core::check_program(&file)
        .expect_err("nested Requires is not a function-level contract field");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0503)
                && diagnostic.message.contains("requires")
        }),
        "{diagnostics:?}"
    );
}
