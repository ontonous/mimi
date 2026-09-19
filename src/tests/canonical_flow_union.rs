//! Canonical multi-target Flow union (`-> A | B`) consumer tests (R6-1034).
//!
//! The flat Copy union face — one single-field variant payload of the same
//! Copy scalar type across every target — is executable by the reference
//! interpreter, the AST-free bytecode VM, and the native emitter on one shared
//! `MirProgram`.  The heterogeneous face (a variant payload outside the flat
//! Copy scalar contract) stays executable by reference/bytecode but must keep
//! failing closed on the native and capability consumers until its own native
//! tagged-union contract is promoted; the route layer keeps such graphs on the
//! compatibility route.

use super::*;
use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
use crate::core::{NodeId, ResolvedTypeId};
use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
use crate::interp::Value;

const FLAT_COPY_UNION_SOURCE: &str =
    include_str!("../../tests/real_world/flow_multi_target_union_copy_flat.mimi");
const HETEROGENEOUS_UNION_SOURCE: &str =
    include_str!("../../tests/real_world/flow_multi_target_union_match.mimi");

const FLAT_COPY_UNION_STDOUT: &str = "60\n40\n";
const HETEROGENEOUS_UNION_STDOUT: &str = "110\n5\n";

fn materialize(source: &str, label: &str) -> MirProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file)
        .unwrap_or_else(|diags| panic!("check {label}: {diags:?}"));
    MirProgram::from_checked_program(&checked)
        .unwrap_or_else(|error| panic!("materialize {label}: {error:?}"))
}

fn flow_union_instruction_mut(
    function: &mut crate::core::mir::MirFunction,
) -> Option<&mut crate::core::mir::types::MirFlowEffectReceipt> {
    function.blocks.values_mut().find_map(|block| {
        block
            .instructions
            .iter_mut()
            .find_map(|instruction| match &mut instruction.kind {
                crate::core::mir::MirInstructionKind::FlowTransition { effect_receipt, .. } => {
                    effect_receipt.as_mut()
                }
                _ => None,
            })
    })
}

const MULTI_FIELD_UNION_SOURCE: &str = r#"
    flow P {
        state A { v: i32, w: i32 }
        state B { v: i32, w: i32 }
        transition go(A, d: i32) -> A | B {
            if d > 0 {
                return B { v: d, w: 1 }
            } else {
                return A { v: d, w: 2 }
            }
        }
    }

    func main() -> i32 {
        let a = A { v: 10, w: 20 }
        let r = P::go(a, 5)
        let t = match r {
            A { v, w } => v + w
            B { v, w } => v + w
        }
        println(t)
        0
    }
"#;

const MULTI_FIELD_UNION_STDOUT: &str = "6\n";

const MIXED_MULTI_FIELD_UNION_SOURCE: &str = r#"
    flow P {
        state A { v: i32 }
        state B { name: string, score: i32 }
        transition go(A, d: i32) -> A | B {
            if d > 0 {
                return B { name: "hit", score: d }
            }
            return A { v: d }
        }
    }

    func main() -> i32 {
        let a = A { v: 10 }
        let r = P::go(a, 5)
        let t = match r {
            A { v } => v
            B { name, score } => score
        }
        println(t)
        0
    }
"#;

const MIXED_MULTI_FIELD_UNION_STDOUT: &str = "5\n";

#[test]
fn mixed_multi_field_union_three_consumers_match() {
    // R6-1038: one variant may carry Copy and owned payloads side by side.
    // The consuming match moves the owned String through its own native
    // slot, and the arm-less faces (drop) settle every field obligation of
    // the active variant.
    let mir = materialize(
        MIXED_MULTI_FIELD_UNION_SOURCE,
        "mixed multi-field union fixture",
    );
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor mixed multi-field union");
    assert_eq!(reference.output, MIXED_MULTI_FIELD_UNION_STDOUT);

    let bytecode = compile_mir_program(&mir).expect("mixed multi-field union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(
        vm.run_value().is_ok(),
        "bytecode mixed multi-field union runs"
    );
    assert_eq!(vm.stdout(), MIXED_MULTI_FIELD_UNION_STDOUT);

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_mixed_multi_field");
    generator
        .compile_mir_native(&mir)
        .expect("native mixed multi-field union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM mixed multi-field union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, MIXED_MULTI_FIELD_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn multi_field_union_three_consumers_match() {
    // R6-1038: the promoted multi-target union tagged-union contract widens
    // from one payload field per variant to a multi-field Copy/owned payload
    // union.  The same MirProgram must execute identically on reference,
    // bytecode, and native, with per-field native ABI slots in name-sorted
    // variant order.
    let mir = materialize(MULTI_FIELD_UNION_SOURCE, "multi-field union fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "a multi-field Copy union must close onto the widened contract"
    );
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    // Consumer 1: AST-free reference executor on the shared MirProgram.
    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor multi-field union");
    assert_eq!(reference.output, MULTI_FIELD_UNION_STDOUT);

    // Consumer 2: bytecode compiled from the same MirProgram (no AST).
    let bytecode = compile_mir_program(&mir).expect("multi-field union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode multi-field union runs");
    assert_eq!(vm.stdout(), MULTI_FIELD_UNION_STDOUT);

    // Consumer 3: native LLVM emission from the same MirProgram.
    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_flow_union_multi_field");
    generator
        .compile_mir_native(&mir)
        .expect("native multi-field union emission");
    generator
        .module
        .verify()
        .expect("valid LLVM multi-field union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, MULTI_FIELD_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn multi_field_union_contract_symbolic_proven() {
    // R6-1038: the symbolic verifier domain merges one return path per target
    // state into a multi-field symbolic union and discharges the contract
    // through the caller's multi-field SwitchMove distribution.
    let source = r#"
        flow Gate {
            state Shut { v: i32, w: i32 }
            state Opened { v: i32, w: i32 }
            transition toggle(Shut, flag: bool) -> Opened | Shut {
                requires: flag == true
                ensures: flag == true
                if flag {
                    return Opened { v: 1, w: 2 }
                } else {
                    return Shut { v: 3, w: 4 }
                }
            }
        }

        func main() -> i32 {
            ensures: result == 0
            let g = Shut { v: 40, w: 50 }
            let next = Gate::toggle(g, true)
            let t = match next {
                Opened { v, w } => v + w
                Shut { v, w } => v + w
            }
            println(t)
            0
        }
    "#;
    let mir = materialize(source, "multi-field union verifier fixture");
    let results = crate::verifier::verify_mir(&mir, "multi-field-union-proven".into())
        .expect("MIR verifier runs the multi-field union program");
    let toggle = results
        .iter()
        .find(|result| result.func_name.contains("toggle"))
        .expect("toggle verification result");
    assert_eq!(
        toggle.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        toggle.message
    );
    let main = results
        .iter()
        .find(|result| result.func_name.contains("main"))
        .expect("main verification result");
    assert_eq!(
        main.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        main.message
    );
}

#[test]
fn flat_copy_union_three_consumers_match() {
    let mir = materialize(FLAT_COPY_UNION_SOURCE, "flat Copy union fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));
    let transition = mir
        .transitions()
        .values()
        .find(|contract| contract.targets.len() > 1)
        .expect("multi-target transition contract");
    assert_eq!(transition.targets.len(), 2);
    assert_eq!(transition.failure, None);
    assert!(!transition.is_fallback && !transition.is_ffi_pinned);

    // Consumer 1: AST-free reference executor on the shared MirProgram.
    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor union face");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, FLAT_COPY_UNION_STDOUT);

    // Consumer 2: bytecode compiled from the same MirProgram (no AST).
    let bytecode = compile_mir_program(&mir).expect("AST-free union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(matches!(
        vm.run_value().expect("bytecode union execution"),
        Value::Int(0)
    ));
    assert_eq!(vm.stdout(), FLAT_COPY_UNION_STDOUT);

    // Consumer 3: native LLVM emission from the same MirProgram.
    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_flow_union_flat_copy");
    generator
        .compile_mir_native(&mir)
        .expect("native union emission");
    generator.module.verify().expect("valid LLVM union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, FLAT_COPY_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn heterogeneous_union_three_consumers_match() {
    // R6-1035B: the promoted multi-target union tagged-union contract admits
    // the heterogeneous face (Copy integer versus owned Move string payloads)
    // on every consumer.  The whole graph routes canonical and the same
    // MirProgram executes identically on reference, bytecode, and native.
    let mir = materialize(HETEROGENEOUS_UNION_SOURCE, "heterogeneous union fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));
    assert!(crate::verifier::validate_mir_capabilities(&mir).is_ok());

    // Consumer 1: AST-free reference executor on the shared MirProgram.
    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executor heterogeneous union");
    assert_eq!(reference.output, HETEROGENEOUS_UNION_STDOUT);

    // Consumer 2: bytecode compiled from the same MirProgram (no AST).
    let bytecode = compile_mir_program(&mir).expect("heterogeneous union bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode heterogeneous union runs");
    assert_eq!(vm.stdout(), HETEROGENEOUS_UNION_STDOUT);

    // Consumer 3: native LLVM emission from the same MirProgram.
    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator =
        crate::codegen::CodeGenerator::new(&context, "mir_flow_union_heterogeneous");
    generator
        .compile_mir_native(&mir)
        .expect("native heterogeneous union emission");
    generator.module.verify().expect("valid LLVM union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, HETEROGENEOUS_UNION_STDOUT);
    assert_eq!(native.stderr, "");
}

#[test]
fn heterogeneous_union_tag_contract_is_name_sorted_with_payload_mirroring() {
    // R6-1035A baseline pin: the union tag contract that the native
    // tagged-union promotion must preserve.  Variants stay name-sorted with
    // enumeration discriminants — the same ordering the legacy synthesized
    // enum uses — so a promoted native tag dispatch cannot silently disagree
    // with the reference/bytecode value model.  Each variant mirrors its
    // target state payload: `Closed{v: i32}` (Copy integer) versus
    // `Open{tag: string}` (owned Move) is the exact heterogeneous face the
    // promotion must admit.
    let mir = materialize(HETEROGENEOUS_UNION_SOURCE, "heterogeneous union fixture");
    let contract = mir
        .transitions()
        .values()
        .find(|contract| contract.owner.0.contains("Pipe::push"))
        .expect("Pipe::push multi-target contract");
    let union = mir
        .type_catalog()
        .get(&contract.result)
        .expect("union TypeDesc materialized");
    assert_eq!(
        union.kind,
        crate::core::mir::types::MirTypeKind::FlowStateSet
    );
    assert_eq!(union.ownership, crate::core::mir::types::MirOwnership::Move);
    let variants = match &union.layout {
        crate::core::mir::types::MirLayout::Enum { variants, .. } => variants,
        other => panic!("expected Enum layout, got {other:?}"),
    };
    let names: Vec<&str> = variants
        .iter()
        .map(|variant| variant.name.as_str())
        .collect();
    assert_eq!(names, vec!["Closed", "Open"], "variants stay name-sorted");
    for (index, variant) in variants.iter().enumerate() {
        assert_eq!(
            variant.discriminant, index as u16,
            "discriminants enumerate"
        );
    }
    let closed = &variants[0];
    assert_eq!(closed.fields.len(), 1);
    assert_eq!(closed.fields[0].name, "v");
    let closed_payload = mir
        .type_catalog()
        .get(&closed.fields[0].ty)
        .expect("Closed payload TypeDesc");
    assert!(matches!(
        closed_payload.abi,
        crate::core::mir::types::MirAbiClass::Integer { .. }
    ));
    let open = &variants[1];
    assert_eq!(open.fields.len(), 1);
    assert_eq!(open.fields[0].name, "tag");
    let open_payload = mir
        .type_catalog()
        .get(&open.fields[0].ty)
        .expect("Open payload TypeDesc");
    assert_eq!(
        open_payload.abi,
        crate::core::mir::types::MirAbiClass::StringHandle
    );
    assert_eq!(
        open_payload.ownership,
        crate::core::mir::types::MirOwnership::Move
    );
}

#[test]
fn heterogeneous_union_drop_face_runs_on_reference_and_bytecode() {
    // R6-1035B: consuming a heterogeneous union value with an explicit
    // `drop(...)` (no match projection) exercises the union drop face — the
    // tag-switched variant drop glue — on every consumer, including the
    // native emitter's recursive payload drop.
    let source = r#"
        flow Pipe {
            state Open { tag: string }
            state Closed { v: i32 }
            transition push(Open) -> Closed | Open {
                return Closed { v: 5 }
            }
        }

        func main() -> i32 {
            let o = Open { tag: "hello" }
            let r = Pipe::push(o)
            drop(r)
            println(7)
            0
        }
    "#;
    let mir = materialize(source, "heterogeneous union drop fixture");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    assert!(crate::core::mir::multi_target_flow_union_face_closed(&mir));

    let reference = MirReferenceInterpreter::new(&mir)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference union drop face");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output, "7\n");
    let bytecode = compile_mir_program(&mir).expect("union drop bytecode");
    assert!(bytecode.ast.is_none());
    let mut vm = BytecodeVM::new(bytecode);
    assert!(vm.run_value().is_ok(), "bytecode union drop runs");
    assert_eq!(vm.stdout(), "7\n");

    if !can_link() {
        return;
    }
    let context = inkwell::context::Context::create();
    let mut generator = crate::codegen::CodeGenerator::new(&context, "mir_flow_union_drop");
    generator
        .compile_mir_native(&mir)
        .expect("native union drop emission");
    generator.module.verify().expect("valid LLVM union module");
    let native = link_and_observe_canonical_mir(&generator).expect("native union drop execution");
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.stdout, "7\n");
    assert_eq!(native.stderr, "");
}

#[test]
fn union_outside_promoted_payload_contract_stays_fail_closed() {
    // The promoted contract admits Copy-scalar or owned-String payload
    // fields per variant (R6-1038 widened the one-field face to multi-field
    // variants, so the formerly-rejected wide variant is pinned as
    // converged).  Aggregate payloads — List and record — stay fail-closed
    // on the native and capability consumers, and an out-of-contract union
    // keeps the whole graph off the canonical route.
    let list_payload = r#"
        flow P {
            state A { v: i32 }
            state B { xs: List<i32> }
            transition go(A, d: i32) -> A | B {
                return B { xs: [d] }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let list_mir = materialize(list_payload, "list payload union fixture");
    assert!(!crate::core::mir::multi_target_flow_union_face_closed(
        &list_mir
    ));
    let list_native = crate::codegen::mir::validate_mir_native(&list_mir)
        .expect_err("native must reject a List payload union");
    assert!(list_native.iter().any(|error| {
        error
            .message
            .contains("only Copy scalars and owned Strings")
    }));
    let list_capability = crate::verifier::validate_mir_capabilities(&list_mir)
        .expect_err("capability gate must reject a List payload union");
    assert!(list_capability
        .iter()
        .any(|error| error.contains("only Copy scalars and owned Strings")));

    let wide_variant = r#"
        flow Q {
            state A { v: i32 }
            state W { a: i32, b: i32 }
            transition go(A, d: i32) -> A | W {
                return W { a: d, b: d }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = Q::go(a, 2)
            drop(r)
            0
        }
    "#;
    let wide_mir = materialize(wide_variant, "wide variant union fixture");
    assert!(
        crate::core::mir::multi_target_flow_union_face_closed(&wide_mir),
        "the multi-field variant face is promoted since R6-1038"
    );
    crate::codegen::mir::validate_mir_native(&wide_mir)
        .expect("native must admit the promoted multi-field union variant");
    crate::verifier::validate_mir_capabilities(&wide_mir)
        .expect("capability gate must admit the promoted multi-field union variant");

    let record_payload = r#"
        type Stats { hits: i32, misses: i32 }

        flow R {
            state A { v: i32 }
            state S { s: Stats }
            transition go(A, d: i32) -> A | S {
                return S { s: Stats { hits: d, misses: 0 } }
            }
        }

        func main() -> i32 {
            let a = A { v: 1 }
            let r = R::go(a, 2)
            drop(r)
            0
        }
    "#;
    let record_mir = materialize(record_payload, "record payload union fixture");
    assert!(!crate::core::mir::multi_target_flow_union_face_closed(
        &record_mir
    ));
    let record_native = crate::codegen::mir::validate_mir_native(&record_mir)
        .expect_err("native must reject a record payload union");
    assert!(
        record_native
            .iter()
            .any(|error| error.message.contains("not Copy with canonical no-op glue")),
        "unexpected record-payload rejection set: {record_native:?}"
    );
    let record_capability = crate::verifier::validate_mir_capabilities(&record_mir)
        .expect_err("capability gate must reject a record payload union");
    assert!(record_capability
        .iter()
        .any(|error| error.contains("not Copy with canonical no-op glue")));
}

#[test]
fn missing_union_effect_receipt_rejects_all_consumers() {
    let mir = materialize(FLAT_COPY_UNION_SOURCE, "missing receipt fixture");
    let mut forged_main = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main body")
        .clone();
    let flow_transition = forged_main
        .blocks
        .values_mut()
        .find_map(|block| {
            block
                .instructions
                .iter_mut()
                .find_map(|instruction| match &mut instruction.kind {
                    crate::core::mir::MirInstructionKind::FlowTransition {
                        effect_receipt, ..
                    } => Some(effect_receipt.take()),
                    _ => None,
                })
        })
        .expect("FlowTransition in main");
    assert!(
        flow_transition.is_some(),
        "the fixture main carries a union effect receipt"
    );
    let mut forged = mir.clone();
    forged.replace_function_for_test_only(forged_main);

    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&NodeId("function:main".into()), &[])
        .expect_err("reference must reject a missing union receipt");
    assert!(
        reference_error
            .to_string()
            .contains("explicit canonical effect receipt"),
        "{reference_error}"
    );
    let bytecode_error =
        compile_mir_program(&forged).expect_err("bytecode must reject a missing union receipt");
    assert!(bytecode_error
        .iter()
        .any(|error| error.message.contains("receipt")));
    let capability_error = crate::verifier::validate_mir_capabilities(&forged)
        .expect_err("capability gate must reject a missing union receipt");
    assert!(capability_error
        .iter()
        .any(|error| error.contains("receipt")));
    let native_error = crate::codegen::mir::validate_mir_native(&forged)
        .expect_err("native must reject a missing union receipt");
    assert!(native_error
        .iter()
        .any(|error| error.message.contains("explicit canonical effect receipt")));
}

#[test]
fn forged_union_receipt_target_rejects_all_consumers() {
    let mir = materialize(FLAT_COPY_UNION_SOURCE, "forged receipt fixture");
    // Forge the receipt to name the second target state instead of the union
    // identity: a multi-target union receipt must name contract.result.
    let second_target: ResolvedTypeId = {
        let contract = mir
            .transitions()
            .values()
            .find(|contract| contract.targets.len() > 1)
            .expect("multi-target contract");
        contract.targets[1].clone()
    };
    let mut forged_main = mir
        .functions()
        .get(&NodeId("function:main".into()))
        .expect("main body")
        .clone();
    let receipt =
        flow_union_instruction_mut(&mut forged_main).expect("union effect receipt in main");
    receipt.target = second_target;
    let mut forged = mir.clone();
    forged.replace_function_for_test_only(forged_main);

    let reference_error = MirReferenceInterpreter::new(&forged)
        .execute(&NodeId("function:main".into()), &[])
        .expect_err("reference must reject a forged union receipt target");
    assert!(
        reference_error
            .to_string()
            .contains("target identity disagrees"),
        "{reference_error}"
    );
    let bytecode_error = compile_mir_program(&forged)
        .expect_err("bytecode must reject a forged union receipt target");
    assert!(bytecode_error
        .iter()
        .any(|error| error.message.contains("receipt")));
    let capability_error = crate::verifier::validate_mir_capabilities(&forged)
        .expect_err("capability gate must reject a forged union receipt target");
    assert!(capability_error
        .iter()
        .any(|error| error.contains("receipt")));
}

#[test]
fn union_contract_disproven_is_real_and_no_contract_stays_no_obligations() {
    // R6-1036B flip of the R6-1034 boundary pin
    // (union_verifier_boundary_is_scoped_to_contract_bearing_callables): an
    // ensures-only union transition now gets a real verdict.  `flag == false`
    // has no requires constraining the parameter, so Z3 finds the
    // counterexample and returns Disproven — not a boundary observation and
    // not a vacuous green.  Contract-free functions emit no verification
    // obligation at all, and definitive verdicts are route-compatible
    // without the runtime-only boundary opt-in flags.
    let source = r#"
        flow Gate {
            state Shut { v: i32 }
            state Opened { v: i32 }
            transition toggle(Shut, flag: bool) -> Opened | Shut {
                ensures: flag == false
                if flag {
                    return Opened { v: 1 }
                } else {
                    return Shut { v: 0 }
                }
            }
        }

        func main() -> i32 {
            let g = Shut { v: 40 }
            let next = Gate::toggle(g, true)
            let t = match next {
                Opened { v } => v
                Shut { v } => v
            }
            println(t)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check union contract fixture");
    let mir = MirProgram::from_checked_program(&checked).expect("materialize");
    let results = crate::verifier::verify_mir(&mir, "flow-union-disproven".into())
        .expect("MIR verifier runs the union program");
    let toggle = results
        .iter()
        .find(|result| result.func_name.contains("toggle"))
        .expect("toggle verification result");
    assert_eq!(
        toggle.status,
        crate::verifier::VerifStatus::Disproven,
        "{}",
        toggle.message
    );
    assert!(
        !toggle
            .message
            .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE),
        "a real verdict must not carry the runtime-only boundary identity: {}",
        toggle.message
    );
    // Contract-free functions (main here) are outside the verifier's
    // obligation set: verify_mir emits no result for them, so the single
    // definitive toggle verdict keeps the route ready without any boundary
    // opt-in.
    assert!(
        results
            .iter()
            .all(|result| !result.func_name.contains("main")),
        "contract-free functions emit no verification result: {results:?}"
    );
    assert!(
        crate::verifier::canonical_execution_route_verifier_ready(&results, false, false),
        "a Disproven union verdict is definitive and route-compatible"
    );
}

#[test]
fn union_contract_symbolic_pre_post_proven() {
    // R6-1036B flip of union_contract_verifier_trusted_subset_baseline
    // (821bb80f pinned the wholesale MIR-VERIFIER-FLOW-UNION-001 rejection):
    // contract-bearing union callables now get real verdicts.  The
    // transition's requires/ensures are discharged on both union return
    // paths, and a contract-bearing caller is verified through the
    // union-aware transition call plus the generic variant SwitchMove.
    let source = r#"
        flow Gate {
            state Shut { v: i32 }
            state Opened { v: i32 }
            transition toggle(Shut, flag: bool) -> Opened | Shut {
                requires: flag == true
                ensures: flag == true
                if flag {
                    return Opened { v: 1 }
                } else {
                    return Shut { v: 0 }
                }
            }
        }

        func main() -> i32 {
            ensures: result == 0
            let g = Shut { v: 40 }
            let next = Gate::toggle(g, true)
            let t = match next {
                Opened { v } => v
                Shut { v } => v
            }
            println(t)
            0
        }
    "#;
    let mir = materialize(source, "verified union contract program");
    assert!(crate::core::mir::contains_multi_target_flow_union_candidate(&mir));
    let results = crate::verifier::verify_mir(&mir, "union-symbolic-proven".into())
        .expect("MIR verifier runs the union program");
    let toggle = results
        .iter()
        .find(|result| result.func_name.contains("toggle"))
        .expect("toggle verification result");
    assert_eq!(
        toggle.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        toggle.message
    );
    assert!(!toggle
        .message
        .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE));
    let main = results
        .iter()
        .find(|result| result.func_name.contains("main"))
        .expect("main verification result");
    assert_eq!(
        main.status,
        crate::verifier::VerifStatus::Proven,
        "{}",
        main.message
    );
    assert!(!main
        .message
        .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE));
}

#[test]
fn union_outside_symbolic_payload_contract_keeps_boundary() {
    // Fail-closed identity survives the promotion: a union whose payload
    // escapes the promoted contract (List<i32>) never reaches a verdict for
    // contract-bearing callables — the transition body construct and the
    // caller's transition call both reject, and the caller keeps the
    // explicit MIR-VERIFIER-FLOW-UNION-001 boundary identity.
    let source = r#"
        flow P {
            state A { v: i32 }
            state B { xs: List<i32> }
            transition go(A, d: i32) -> A | B {
                requires: d > 0
                ensures: d > 0
                if d > 0 {
                    return B { xs: [d] }
                } else {
                    return A { v: d }
                }
            }
        }

        func main() -> i32 {
            ensures: result == 0
            let a = A { v: 1 }
            let r = P::go(a, 2)
            drop(r)
            0
        }
    "#;
    let mir = materialize(source, "list payload union verifier fixture");
    let results = crate::verifier::verify_mir(&mir, "union-symbolic-negative".into())
        .expect("MIR verifier runs the out-of-contract union program");
    let go = results
        .iter()
        .find(|result| result.func_name.contains("go"))
        .expect("go verification result");
    assert_eq!(go.status, crate::verifier::VerifStatus::NotInTrustedSubset);
    let main = results
        .iter()
        .find(|result| result.func_name.contains("main"))
        .expect("main verification result");
    assert_eq!(
        main.status,
        crate::verifier::VerifStatus::NotInTrustedSubset
    );
    assert!(
        main.message
            .contains(crate::core::mir::types::MIR_VERIFIER_FLOW_UNION_BOUNDARY_CODE),
        "{}",
        main.message
    );
}

#[test]
fn union_transition_with_failure_stays_checker_rejected() {
    // E0433 fail-closed boundary: `fails` combined with a multi-target union
    // return stays rejected at the checker before any MIR exists.
    let source = r#"
        flow P {
            state A { v: i32 }
            state B { v: i32 }
            transition go(A, d: i32) -> A | B fails string {
                return B { v: d }
            }
        }

        func main() -> i32 {
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let diagnostics = crate::core::check_program(&file)
        .expect_err("fails + multi-target union must stay checker-rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_deref() == Some("E0433")),
        "{diagnostics:?}"
    );
}

#[test]
fn out_of_contract_union_still_executes_on_the_legacy_compatibility_route() {
    // R6-1037A deletion-audit reachability pin (R6-1034③), restated by
    // R6-1038: the promoted tagged-union contract now admits multi-field
    // Copy/owned-String variants, so the reachable out-of-contract face is
    // an aggregate payload (List<i32>).  That shape keeps the explicit
    // Legacy compatibility route — and that route is *reachable*, not
    // vestigial: the default-run legacy bytecode engine (the pipeline
    // `mimi run` falls to after the "canonical route disposition: legacy"
    // notice) compiles and executes the union graph with correct results.
    // This execution dependency is what blocks deleting the legacy union
    // path until those shapes are promoted or checker-rejected.
    let source = r#"
        flow P {
            state A { v: i32 }
            state B { xs: List<i32> }
            transition go(A, d: i32) -> A | B {
                if d > 0 {
                    return B { xs: [d, d] }
                } else {
                    return A { v: d }
                }
            }
        }

        func main() -> i32 {
            let a = A { v: 10 }
            let r = P::go(a, 5)
            drop(r)
            println(6)
            0
        }
    "#;
    let mir = materialize(source, "aggregate payload union legacy fixture");
    assert!(
        !crate::core::mir::multi_target_flow_union_face_closed(&mir),
        "an aggregate payload union is outside the promoted multi-field contract"
    );
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check legacy union fixture");
    let mut compiler = crate::interp::bytecode::BytecodeCompiler::new();
    compiler.install_checked_program(&checked);
    let prog = compiler
        .compile_file(&file)
        .expect("legacy route compiles the out-of-contract union graph");
    let mut vm = crate::interp::bytecode::BytecodeVM::new(prog);
    vm.enable_stdout_capture();
    let exit = vm.run().expect("legacy route executes the union graph");
    assert_eq!(exit, 0);
    assert_eq!(
        vm.take_stdout().trim(),
        "6",
        "the legacy union route must reach the end of main"
    );
}
