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
    }
}

#[test]
fn canonical_actor_reference_repeated_turns_keep_state_and_root_replay_resets_it() {
    let source = CAPTURE.replace("println(r)", "println(r)\n    println(c.bump(7))");
    let program = canonical(&source);
    let reference = MirReferenceInterpreter::new(&program);
    for _ in 0..2 {
        let observation = reference
            .execute_with_output(&main_owner(), &[])
            .expect("repeat Actor turns");
        assert_eq!(observation.output, "15\n32\n");
        assert_closed(&reference, 1, 2, 0);
    }
}

#[test]
fn canonical_actor_reference_two_instances_keep_nonzero_initial_state_independently() {
    let program = canonical(
        r#"
actor Counter {
    base: i32 = 9;
    func bump(delta: i32) -> i32 {
        let scale = 10
        func scaled(d: i32) -> i32 { scale + d }
        self.base = self.base + scaled(delta)
        self.base
    }
}
func main() -> i32 {
    let a = Counter.spawn()
    let b = Counter.spawn()
    println(a.bump(5))
    println(b.bump(7))
    println(a.bump(7))
    0
}
"#,
    );
    let reference = MirReferenceInterpreter::new(&program);
    let observation = reference
        .execute_with_output(&main_owner(), &[])
        .expect("isolated Actor instances");
    assert_eq!(observation.output, "24\n26\n41\n");
    assert_closed(&reference, 2, 3, 0);
}

#[test]
fn canonical_actor_reference_two_capture_environment_is_bound_to_lexical_helper() {
    let program = canonical(
        r#"
func scaled(x: i32) -> i32 { x + 100 }
actor Counter {
    base: i32 = 0;
    func bump(delta: i32) -> i32 {
        let scale = 10
        let bias = 3
        func scaled(d: i32) -> i32 { scale + bias + d }
        self.base = self.base + scaled(delta)
        self.base
    }
}
func main() -> i32 {
    let a = Counter.spawn()
    println(a.bump(5))
    println(a.bump(7))
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
    assert_closed(&reference, 1, 2, 0);
}

#[test]
fn canonical_actor_reference_method_trap_settles_turn_and_closes_root() {
    let program = canonical(
        r#"
actor Counter {
    base: i32 = 0;
    func divide(delta: i32) -> i32 {
        self.base = self.base + delta
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

    let (id, mut operation) = operation_matching(&program, |operation| {
        matches!(operation, MirActorOperation::FieldLoad { .. })
    });
    if let MirActorOperation::FieldLoad { field, .. } = &mut operation {
        *field = NodeId("field:forged".into());
    }
    assert_preflight_rejects(program.clone(), &id, operation);

    let (id, mut operation) = operation_matching(&program, |operation| {
        matches!(operation, MirActorOperation::FieldStore { .. })
    });
    if let MirActorOperation::FieldStore { state, value, .. } = &mut operation {
        *state = value.clone();
    }
    assert_preflight_rejects(program, &id, operation);
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
        crate::core::CheckedProgram::reset_test_legacy_body_access();
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
        assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty());
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
        });
        forged_program.replace_function_for_test_only(function);
        assert!(
            crate::verifier::verify_mir(&forged_program, "actor-route-original".into()).is_err(),
            "mailbox graph plus injected contract cannot produce a proof"
        );
    }
}

#[test]
fn canonical_actor_route_retains_unused_and_embedded_checked_contract_obligations() {
    for (suffix, proven) in [
        ("func guarded(x: i32) -> i32 {\n requires: x > 0\n ensures: result == x\n x\n}", true),
        ("func guarded(x: i32) -> i32 {\n requires: x > 0\n ensures: result != x\n x\n}", false),
        ("func identity_i32(x: i32) -> i32 { x }\nfunc guarded(x: i32) -> i32 { identity_i32({\n ensures: false\n x\n}) }", false),
        ("func guarded(x: i32) -> i32 { x + {\n ensures: false\n 0\n} }", false),
    ] {
        for original in [CAPTURE, SHADOW] {
            let source = format!("{original}\n{suffix}");
            let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
            let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
            crate::loader::merge_prelude_into(&mut file);
            let checked = crate::core::check_program(&file).expect("accepted dormant user contract");
            let owner = NodeId("function:guarded".into());
            let callable = checked.callable(&owner).expect("whole checked callable owner");
            assert!(callable.has_proof_obligations());
            assert!(!callable.contracts.is_empty(), "Call/Binary descendants must remain in the checker contract census");
            assert!(super::scalar_actor_contract_boundary_reason(&checked).unwrap().contains("guarded"));
            assert!(!super::classify_canonical_mir_route_admission(&checked).scalar_actor);
            let excluded = checked.source_registry().records().iter()
                .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
                .map(|record| record.id).collect::<std::collections::HashSet<_>>();
            assert!(MirProgram::from_checked_program_excluding_sources(&checked, &excluded).unwrap_err()
                .to_string().contains("cannot discard proof obligations"));
            let mut configured = crate::verifier::Verifier::new().unwrap();
            configured.set_source_hash("whole-actor-user-contract-census".into());
            for results in [crate::verifier::verify_checked(&checked, "whole-actor-user-contract-census".into()).unwrap(),
                crate::verifier::verify_checked_dual(&checked, "whole-actor-user-contract-census".into()).unwrap(),
                configured.verify_checked(&checked)] {
                let guarded = results.iter().find(|result| result.func_name == "guarded")
                    .expect("default public verifier must retain the dormant owner obligation");
                assert_ne!(guarded.status, crate::verifier::VerifStatus::NoObligations);
                if proven {
                    assert_eq!(guarded.status, crate::verifier::VerifStatus::Proven, "supported original user proof must remain supported: {guarded:?}");
                } else {
                    assert_ne!(guarded.status, crate::verifier::VerifStatus::Proven, "false embedded/unused claim cannot be proved: {guarded:?}");
                }
            }
        }
    }
}

#[test]
fn canonical_actor_proof_census_retains_dormant_nested_impl_flow_and_declaration_owners() {
    for (suffix, owner_fragment) in [
        ("func outer() -> i32 {\nfunc guarded(x: i32) -> i32 {\nensures: false\nx\n}\n0\n}", "guarded"),
        ("trait ProofTrait { func check(x: i32) -> i32 }\ntype ProofProbe { marker: i32 }\nimpl ProofTrait for ProofProbe { func check(x: i32) -> i32 {\nensures: false\nx\n} }", "check"),
        ("flow Hidden {\nstate A { n: i32 }\ntransition tick(A) -> A {\nensures: false\nreturn A { n: self.n }\n}\n}", "Hidden"),
        ("actor Dormant { n: i32 = {\nensures: false\n0\n};\nfunc read() -> i32 { self.n }\n}", "Dormant"),
        ("const VALUE: i32 = {\nensures: false\n1\n}", "VALUE"),
    ] {
        let source = format!("{CAPTURE}\n{suffix}");
        let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
        let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
        crate::loader::merge_prelude_into(&mut file);
        let checked = crate::core::check_program(&file).expect("accepted independent proof owner");
        assert!(super::scalar_actor_contract_boundary_reason(&checked).is_some());
        assert!(!super::classify_canonical_mir_route_admission(&checked).scalar_actor);
        let mut configured = crate::verifier::Verifier::new().unwrap();
        configured.set_source_hash("actor-whole-owner-census".into());
        for results in [crate::verifier::verify_checked(&checked, "actor-whole-owner-census".into()).unwrap(),
            crate::verifier::verify_checked_dual(&checked, "actor-whole-owner-census".into()).unwrap(),
            configured.verify_checked(&checked)] {
            let obligation = results.iter().find(|result| result.func_name.contains(owner_fragment))
                .unwrap_or_else(|| panic!("typed owner {owner_fragment} must retain a proof obligation: {results:?}"));
            assert_ne!(obligation.status, crate::verifier::VerifStatus::NoObligations);
            assert_ne!(obligation.status, crate::verifier::VerifStatus::Proven, "false dormant claim cannot be proved");
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
"#
    );
    let tokens = crate::lexer::Lexer::new(&source).tokenize().unwrap();
    let mut file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    crate::loader::merge_prelude_into(&mut file);
    let checked = crate::core::check_program(&file).expect("mixed Actor/FFI source checks");
    assert!(super::scalar_actor_contract_boundary_reason(&checked).is_some());

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

    for results in batches {
        let positive = results
            .iter()
            .find(|result| result.func_name == "actor_ffi_positive calls actor_ffi_probe")
            .unwrap_or_else(|| panic!("missing positive FFI call-site check: {results:?}"));
        assert_eq!(positive.status, crate::verifier::VerifStatus::Proven);
        let negative = results
            .iter()
            .find(|result| result.func_name == "actor_ffi_negative calls actor_ffi_probe")
            .unwrap_or_else(|| panic!("missing negative FFI call-site check: {results:?}"));
        assert_eq!(negative.status, crate::verifier::VerifStatus::Disproven);

        let foreign_ensures = results
            .iter()
            .filter(|result| result.func_name.contains("extern actor_ffi_probe Ensures"))
            .collect::<Vec<_>>();
        assert_eq!(
            foreign_ensures.len(),
            2,
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
    let root = map_new()
    let updated = map_set(root, "answer", scalar_probe(41 as i32))
    let observed = match map_get_i32(updated, "answer") {
        Ok(value) => value,
        Err(code) => code,
    }
    let size = map_size(updated)
    drop(updated)
    println(actor_value)
    actor_value + size + observed
}
"#;
    const UNUSED_ACTOR_MAIN: &str = r#"
func main() -> i32 {
    let root = map_new()
    let updated = map_set(root, "answer", scalar_probe(41 as i32))
    let observed = match map_get_i32(updated, "answer") {
        Ok(value) => value,
        Err(code) => code,
    }
    let size = map_size(updated)
    drop(updated)
    size + observed
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

        let direct = crate::verifier::verify_ffi_checked(&checked)
            .unwrap_or_else(|error| panic!("{label}: dedicated FFI adapter: {error}"));
        let direct_requirement = direct
            .iter()
            .find(|result| result.func_name == "main calls scalar_probe")
            .unwrap_or_else(|| panic!("{label}: missing dedicated FFI result: {direct:?}"));
        assert_eq!(
            direct_requirement.status,
            crate::verifier::VerifStatus::NotInTrustedSubset
        );
        assert!(direct_requirement.artifact.is_none());
        assert!(direct_requirement.message.contains("Map profile"));

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
fn canonical_actor_complete_proof_census_does_not_promote_scoped_requires_to_function_assumptions()
{
    let source = "func guarded(x: i32) -> i32 {\nensures: false\nx + {\nrequires: false\n0\n}\n}\nfunc main() -> i32 { 0 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().unwrap();
    let file = crate::parser::Parser::new(tokens).parse_file().unwrap();
    let checked = crate::core::check_program(&file).unwrap();
    let callable = checked
        .callable(&NodeId("function:guarded".into()))
        .unwrap();
    assert!(callable.has_proof_obligations() && callable.has_unmodeled_proof_roots());
    assert_eq!(
        callable.contracts.len(),
        2,
        "scope-aware census still retains both source obligations"
    );
    let program = MirProgram::from_checked_program(&checked)
        .expect("scoped proof roots are retained as explicit MIR boundaries");
    let guarded = program
        .functions()
        .get(&NodeId("function:guarded".into()))
        .expect("guarded MIR owner");
    assert!(
        guarded.contracts.iter().all(|contract| {
            contract.id.0.contains("stmt.ensures") && !contract.id.0.contains("stmt.requires")
        }),
        "only the direct entry Ensures may become a MIR function contract"
    );
    let boundary = guarded
        .proof_worklist
        .as_ref()
        .expect("scoped proof boundary worklist")
        .boundaries
        .iter()
        .flat_map(|boundary| boundary.scoped_boundaries.iter())
        .find(|boundary| boundary.point.0.contains("expr.binary"))
        .expect("nested Requires must remain a scoped, reason-bearing boundary");
    assert!(boundary.reason.contains("proof root") || boundary.reason.contains("evaluation-order"));
    let mut verifier = crate::verifier::Verifier::new().unwrap();
    let results = verifier.verify_checked(&checked);
    let result = results
        .iter()
        .find(|result| result.func_name == "guarded")
        .unwrap();
    assert_eq!(
        result.status,
        crate::verifier::VerifStatus::NotInTrustedSubset
    );
    assert!(result.artifact.is_none());
}
