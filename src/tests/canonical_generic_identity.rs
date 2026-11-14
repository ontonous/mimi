//! Same-graph execution evidence for the bounded scalar generic identity MIR
//! profile. The checker is run once and every consumer below receives the
//! same materialized `MirProgram` and its route receipt.

use super::*;
use crate::core::mir::reference::MirReferenceInterpreter;
use crate::core::NodeId;

#[test]
fn scalar_generic_identity_matches_reference_bytecode_native_and_verifier() {
    assert_scalar_generic_identity_differential(
        "func pass<T>(value: T) -> T { value }\nfunc main() -> i32 { pass(41) }",
        crate::core::PrimitiveType::I32,
        crate::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND,
        41,
    );
}

#[test]
fn scalar_generic_identity_i64_matches_reference_bytecode_native_and_verifier() {
    for value in [i32::MAX as i64 + 1, 2_147_483_690, i64::MAX] {
        let source = format!(
            "func pass<T>(value: T) -> T {{ value }}\nfunc main() -> i64 {{ pass({value}) }}"
        );
        assert_scalar_generic_identity_differential(
            &source,
            crate::core::PrimitiveType::I64,
            crate::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND,
            value,
        );
    }
}

#[test]
fn scalar_generic_first_argument_projection_matches_all_mir_consumers() {
    assert_scalar_generic_argument_projection_differential(
        "func choose<T>(left: T, right: T) -> T { left }\nfunc main() -> i32 { choose(42, 43) }",
        crate::core::PrimitiveType::I32,
        crate::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND,
        42,
        0,
        2,
    );
    assert_scalar_generic_argument_projection_differential(
        "func choose<T>(left: T, right: T) -> T { left }\nfunc main() -> i64 { choose(2147483690, 2147483691) }",
        crate::core::PrimitiveType::I64,
        crate::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND,
        2_147_483_690,
        0,
        2,
    );
}

#[test]
fn scalar_generic_argument_projection_nonzero_positions_match_all_mir_consumers() {
    for (source, expected, index, count) in [
        ("func pick<T>(a: T, b: T) -> T { b }\nfunc main() -> i64 { pick(2147483690, 4294967338) }", 4_294_967_338, 1, 2),
        ("func pick<T>(a: T, b: T, c: T) -> T { b }\nfunc main() -> i64 { pick(2147483690, 4294967338, 6442450986) }", 4_294_967_338, 1, 3),
        ("func pick<T>(a: T, b: T, c: T) -> T { c }\nfunc main() -> i64 { pick(2147483690, 4294967338, 6442450986) }", 6_442_450_986, 2, 3),
        // The seventh integer argument crosses the x86_64 register/stack ABI boundary.
        ("func pick<T>(a: T, b: T, c: T, d: T, e: T, f: T, g: T) -> T { g }\nfunc main() -> i64 { pick(2147483690, 4294967338, 6442450986, 8589934634, 10737418282, 12884901930, 15032385578) }", 15_032_385_578, 6, 7),
    ] {
        // Every actual has low byte 42. The native assertion below compares
        // all 64 bits, so a wrong parameter or truncated ABI cannot pass.
        assert_scalar_generic_argument_projection_differential(source, crate::core::PrimitiveType::I64, crate::core::mir::SCALAR_GENERIC_IDENTITY_I64_ISLAND, expected, index, count);
    }
    assert_scalar_generic_argument_projection_differential(
        "func pick<T>(a: T, b: T, c: T) -> T { c }\nfunc main() -> i32 { pick(41, 42, 43) }",
        crate::core::PrimitiveType::I32,
        crate::core::mir::SCALAR_GENERIC_IDENTITY_I32_ISLAND,
        43,
        2,
        3,
    );
}

#[test]
fn scalar_generic_first_argument_projection_rejects_forged_copy_source() {
    let source =
        "func choose<T>(left: T, right: T) -> T { left }\nfunc main() -> i32 { choose(41, 43) }";
    let file = parse_prod(source);
    let checked = crate::core::check_program(&file).expect("checker");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("checker-owned argument projection route");
    let program = route.program;
    let instance = program
        .instances()
        .values()
        .next()
        .expect("argument projection instance")
        .clone();
    let mut functions = program.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized projection target");
    let [_, second] = target.parameters.as_slice() else {
        panic!("projection target must have two formal parameters");
    };
    let second = second.clone();
    let block = target
        .blocks
        .get_mut(&target.entry)
        .expect("projection entry block");
    let [instruction] = block.instructions.as_mut_slice() else {
        panic!("projection body must have one canonical Clone");
    };
    let crate::core::mir::MirInstructionKind::Clone { source, .. } = &mut instruction.kind else {
        panic!("projection body must use the canonical Clone normalization");
    };
    *source = second;

    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        program.type_catalog().clone(),
        program.instances().clone(),
    )
    .expect_err("a forged Copy from formal parameter 1 must fail the MIR gate");
    assert!(
        errors.iter().any(|error| {
            error
                .message
                .contains("generic MIR argument projection contract is invalid")
                && error.message.contains("formal parameter 0")
        }),
        "forged source must be rejected by the shared MIR shape gate: {errors:?}"
    );
}

#[test]
fn scalar_generic_argument_projection_rejects_forged_index_arity_and_body() {
    let source =
        "func pick<T>(a: T, b: T, c: T) -> T { c }\nfunc main() -> i32 { pick(41, 42, 43) }";
    let checked = crate::core::check_program(&parse_prod(source)).expect("checker");
    let program = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("projection route")
        .program;
    let instance_id = program
        .instances()
        .keys()
        .next()
        .expect("projection instance")
        .clone();
    let target_owner = program.instances()[&instance_id].function.clone();
    for case in [
        "index",
        "index_oob",
        "count",
        "body_source",
        "return",
        "call_arity",
    ] {
        let mut functions = program.functions().clone();
        let mut instances = program.instances().clone();
        match case {
            "index" | "index_oob" | "count" => {
                let contract = &mut instances.get_mut(&instance_id).expect("instance").contract;
                let crate::core::mir::MirGenericInstanceContract::ScalarArgumentProjection {
                    parameter_index,
                    parameter_count,
                } = contract
                else {
                    panic!("projection contract")
                };
                match case {
                    "index" => *parameter_index = 1,
                    "index_oob" => *parameter_index = 3,
                    "count" => *parameter_count = 2,
                    _ => unreachable!(),
                }
            }
            "body_source" | "return" => {
                let function = functions.get_mut(&target_owner).expect("target");
                let wrong_formal = function.parameters[1].clone();
                let block = function.blocks.get_mut(&function.entry).expect("entry");
                if case == "return" {
                    block.terminator = crate::core::mir::MirTerminator::Return {
                        value: Some(wrong_formal),
                    };
                } else {
                    let crate::core::mir::MirInstructionKind::Clone { source, .. } =
                        &mut block.instructions[0].kind
                    else {
                        panic!("projection Clone")
                    };
                    *source = wrong_formal;
                }
            }
            "call_arity" => {
                let main = functions
                    .get_mut(&NodeId("function:main".into()))
                    .expect("main");
                let call = main
                    .blocks
                    .get_mut(&main.entry)
                    .expect("entry")
                    .instructions
                    .last_mut()
                    .expect("call");
                let crate::core::mir::MirInstructionKind::Call { arguments, .. } = &mut call.kind
                else {
                    panic!("generic call")
                };
                arguments.pop();
            }
            _ => unreachable!(),
        }
        let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
            functions,
            program.type_catalog().clone(),
            instances,
        )
        .expect_err("forged projection must fail before consumers");
        assert!(
            !errors.is_empty(),
            "{case} tamper must have a validation diagnostic"
        );
    }
}

#[test]
fn scalar_generic_argument_projection_keeps_owned_and_effectful_bodies_outside_contract() {
    for source in [
        "func pick<T>(a: T, b: T) -> T { b }\nfunc owned_string_result() -> string { pick(\"owned-a\", \"owned-b\") }\nfunc main() -> i32 { println(owned_string_result()); 0 }",
        "func pick<T>(a: T, b: T) -> T { println(99); b }\nfunc main() -> i32 { pick(41, 43) }",
    ] {
        let checked = crate::core::check_program(&parse_prod(source)).expect("source remains checker-valid");
        crate::core::mir::reference::MirProgram::from_checked_program(&checked).expect_err("owned or effectful generic bodies cannot inherit a scalar parameter projection receipt");
    }
    let linear = "func pick<linear T>(a: T, b: T, c: T) -> T { c }\nfunc main() -> i64 { let selected = pick(make_token(), make_token(), make_token()); token_id(selected) }";
    crate::core::check_program(&parse_prod(linear))
        .expect_err("unselected linear parameters must retain their exactly-once obligations");
}

#[test]
fn scalar_generic_argument_projection_shared_arity_limit_matches_bytecode_frame_budget() {
    let maximum = crate::core::mir::SCALAR_ARGUMENT_PROJECTION_MAX_PARAMETERS;
    assert_eq!(2 * maximum + 1, u16::MAX as u32);
    assert!(crate::core::mir::validate_generic_argument_projection_arity(maximum).is_ok());
    assert!(
        crate::core::mir::validate_generic_argument_projection_arity(maximum + 1)
            .expect_err("one extra formal exceeds the shared register ABI")
            .contains("2*N+1")
    );
    assert!(crate::core::mir::validate_generic_argument_projection_arity(1).is_err());
}

#[test]
fn scalar_generic_argument_projection_consumers_recheck_changed_clone_source() {
    let source =
        "func pick<T>(a: T, b: T, c: T) -> T { c }\nfunc main() -> i32 { pick(41, 42, 43) }";
    let checked = crate::core::check_program(&parse_prod(source)).expect("checker");
    let mut program = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("projection route")
        .program;
    let target_owner = program
        .instances()
        .values()
        .next()
        .expect("instance")
        .function
        .clone();
    let mut target = program.functions()[&target_owner].clone();
    let wrong_formal = target.parameters[1].clone();
    let block = target.blocks.get_mut(&target.entry).expect("entry");
    let crate::core::mir::MirInstructionKind::Clone { source, .. } =
        &mut block.instructions[0].kind
    else {
        panic!("projection Clone")
    };
    *source = wrong_formal;
    program.replace_function_for_test_only(target);
    MirReferenceInterpreter::new(&program)
        .execute(&NodeId("function:main".into()), &[])
        .expect_err("reference must recheck the instance receipt before execution");
    crate::interp::bytecode::compile_mir_program(&program)
        .expect_err("direct bytecode consumer must recheck the selected formal");
    crate::codegen::mir::validate_mir_native(&program)
        .expect_err("native consumer must recheck the selected formal");
    crate::verifier::validate_mir_capabilities(&program)
        .expect_err("verifier must recheck the selected formal");
}

fn assert_scalar_generic_argument_projection_differential(
    source: &str,
    primitive: crate::core::PrimitiveType,
    island: &str,
    expected: i64,
    parameter_index: u32,
    parameter_count: u32,
) {
    let file = parse_prod(source);
    let checked = crate::core::check_program(&file).expect("checker");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("checker-owned argument projection route");
    let admission = match primitive {
        crate::core::PrimitiveType::I32 => route.admission.scalar_generic_identity_i32,
        crate::core::PrimitiveType::I64 => route.admission.scalar_generic_identity_i64,
        unsupported => panic!("test helper only covers i32/i64 projection, got {unsupported:?}"),
    };
    assert_eq!(
        admission,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage
    );
    assert!(
        route.materialized_scalar_generic_identity_i32_candidate
            || route.materialized_scalar_generic_identity_i64_candidate
    );
    let mir = route.program;
    let instance = mir
        .instances()
        .values()
        .next()
        .expect("materialized generic projection instance");
    assert_eq!(
        instance.contract,
        crate::core::mir::MirGenericInstanceContract::ScalarArgumentProjection {
            parameter_index,
            parameter_count,
        }
    );
    let receipt = mir.route_receipt(island);

    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(expected)
    );

    let bytecode = crate::interp::bytecode::compile_mir_program_with_route_receipt(&mir, &receipt)
        .expect("AST-free bytecode consumer");
    let bytecode_value = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode execution");
    assert!(matches!(bytecode_value, crate::interp::Value::Int(value) if value == expected));

    crate::verifier::validate_mir_capabilities(&mir).expect("MIR verifier capability");
    let verified = crate::verifier::verify_mir_with_route_receipt(
        &mir,
        &receipt,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("no-contract MIR verifier pass");
    assert!(
        verified.is_empty(),
        "no source contracts means no proof results"
    );

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "generic_first_projection");
    codegen
        .compile_mir_native_with_route_receipt(&mir, &receipt)
        .expect("native consumer using the same MIR and route receipt");
    // The OS truncates process exit values to eight bits. Call the emitted
    // main from a test-only wrapper and compare its complete integer return
    // value before converting that comparison into the process status.
    let projected_main = codegen.module.get_function("main").expect("native main");
    projected_main
        .as_global_value()
        .set_name("mimi_projection_result");
    let result_type = projected_main
        .get_type()
        .get_return_type()
        .expect("projection result type")
        .into_int_type();
    assert_eq!(
        result_type.get_bit_width(),
        if primitive == crate::core::PrimitiveType::I64 {
            64
        } else {
            32
        }
    );
    let wrapper = codegen
        .module
        .add_function("main", context.i32_type().fn_type(&[], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    codegen.builder.position_at_end(entry);
    let actual = crate::codegen::call_try_basic_value(
        &codegen
            .build_call(projected_main, &[], "projection_result")
            .expect("call complete native result"),
    )
    .expect("native result")
    .into_int_value();
    let equals = codegen
        .builder
        .build_int_compare(
            inkwell::IntPredicate::EQ,
            actual,
            result_type.const_int(expected as u64, true),
            "projection_matches",
        )
        .expect("compare complete native result");
    let failure = codegen
        .builder
        .build_not(equals, "projection_mismatch")
        .expect("comparison status");
    let status = codegen
        .builder
        .build_int_z_extend(failure, context.i32_type(), "test_status")
        .expect("process status");
    codegen
        .build_return(Some(&status))
        .expect("return comparison status");
    codegen
        .module
        .verify()
        .expect("native LLVM module verifies");
    let native = link_and_observe_canonical_mir(&codegen).expect("native process execution");
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
    assert_eq!(
        native.exit_code,
        Some(0),
        "native must preserve the complete selected scalar value"
    );
}

fn assert_scalar_generic_identity_differential(
    source: &str,
    primitive: crate::core::PrimitiveType,
    island: &str,
    expected: i64,
) {
    let file = parse_prod(source);
    let checked = crate::core::check_program(&file).expect("checker");
    let route = crate::core::mir::materialize_canonical_mir_route(&checked, None)
        .expect("checker-owned scalar identity route");
    let (admission, materialized, validate) = match primitive {
        crate::core::PrimitiveType::I32 => (
            route.admission.scalar_generic_identity_i32,
            route.materialized_scalar_generic_identity_i32_candidate,
            crate::core::mir::validate_scalar_generic_identity_island
                as fn(&crate::core::mir::reference::MirProgram) -> Result<(), Vec<String>>,
        ),
        crate::core::PrimitiveType::I64 => (
            route.admission.scalar_generic_identity_i64,
            route.materialized_scalar_generic_identity_i64_candidate,
            crate::core::mir::validate_scalar_generic_identity_i64_island
                as fn(&crate::core::mir::reference::MirProgram) -> Result<(), Vec<String>>,
        ),
        unsupported => panic!("test helper only covers i32/i64 identity, got {unsupported:?}"),
    };
    assert_eq!(
        admission,
        crate::core::mir::ScalarGenericIdentityAdmission::CompleteCoverage
    );
    assert!(materialized);
    let mir = route.program;
    validate(&mir).expect("whole-program profile gate");
    let receipt = mir.route_receipt(island);

    let reference = MirReferenceInterpreter::new(&mir)
        .execute(&NodeId("function:main".into()), &[])
        .expect("reference execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(expected)
    );

    let bytecode = crate::interp::bytecode::compile_mir_program_with_route_receipt(&mir, &receipt)
        .expect("AST-free bytecode consumer");
    let bytecode_value = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode execution");
    assert!(matches!(bytecode_value, crate::interp::Value::Int(value) if value == expected));

    crate::verifier::validate_mir_capabilities(&mir).expect("MIR verifier capability");
    let verified = crate::verifier::verify_mir_with_route_receipt(
        &mir,
        &receipt,
        blake3::hash(source.as_bytes()).to_hex().to_string(),
    )
    .expect("no-contract MIR verifier pass");
    assert!(
        verified.is_empty(),
        "no source contracts means no proof results"
    );

    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "scalar_generic_identity");
    codegen
        .compile_mir_native_with_route_receipt(&mir, &receipt)
        .expect("native consumer using the same MIR and receipt");
    codegen
        .module
        .verify()
        .expect("native LLVM module verifies");
    let native = link_and_observe_canonical_mir(&codegen).expect("native process execution");
    assert_eq!(native.stdout, "");
    assert_eq!(native.stderr, "");
    assert_eq!(native.exit_code, Some((expected as u8) as i32));
}
