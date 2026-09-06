use super::*;
use crate::core::ir::{PrimitiveType, ResolvedType, ResolvedTypeTable};
use crate::core::mir::types::{MirGlueKind, MirOwnership};

fn type_id(table: &mut ResolvedTypeTable, ty: ResolvedType) -> ResolvedTypeId {
    table.intern_resolved(ty).expect("test type must intern")
}

fn checked_program(source: &str) -> crate::core::CheckedProgram {
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    crate::core::check_program(&file).expect("check")
}

#[test]
fn materializes_terminal_session_close_with_backend_neutral_receipt() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_close.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("terminal SessionChan close must lower to canonical MIR");
    let owner = crate::core::NodeId("function:close_endpoint".into());
    let function = program.functions().get(&owner).expect("close_endpoint MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                endpoint,
                contract: Some(contract),
                ..
            } => Some((*operation, endpoint.clone(), contract.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        session_calls.len(),
        1,
        "close must be one explicit SessionCall"
    );
    let (operation, endpoint, contract) = &session_calls[0];
    assert_eq!(
        *operation,
        crate::core::mir::types::MirSessionOperation::Close
    );
    let endpoint_ty = function
        .values
        .get(endpoint)
        .expect("endpoint value")
        .ty
        .clone();
    assert_eq!(contract.endpoint_ty, endpoint_ty);
    assert!(contract.payload_ty.is_none());
    assert!(contract.terminal);
    assert_eq!(contract.after.as_str(), "closed");
    program
        .type_catalog()
        .validate_session_channel(&endpoint_ty)
        .expect("SessionChan TypeDesc contract");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
        )
        .expect("reference SessionCall execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the same canonical SessionCall");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the same canonical SessionCall");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the same canonical SessionCall");
}

#[test]
fn materializes_session_open_as_canonical_builtin_with_backend_neutral_oracle() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_open.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("session_open must lower to canonical MIR");
    let owner = crate::core::NodeId("function:open_close".into());
    let function = program.functions().get(&owner).expect("open_close MIR");
    let (result, kind) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                result,
                kind: crate::core::mir::types::MirBuiltinKind::SessionOpen,
                arguments,
            } => Some((result.clone(), arguments.len())),
            _ => None,
        })
        .expect("session_open builtin instruction");
    assert_eq!(kind, 0, "session_open has no value arguments");
    let result_ty = function
        .values
        .get(&result)
        .expect("session_open result value")
        .ty
        .clone();
    program
        .type_catalog()
        .validate_session_channel(&result_ty)
        .expect("session_open result must be transfer-only SessionChan");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference session_open/session_close execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume canonical SessionOpen");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume canonical SessionOpen");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume canonical SessionOpen");
}

#[test]
fn rejects_forged_session_open_result_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_open.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical session_open MIR");
    let owner = crate::core::NodeId("function:open_close".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("open_close MIR");
    let result = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                kind: crate::core::mir::types::MirBuiltinKind::SessionOpen,
                result,
                ..
            } => Some(result.clone()),
            _ => None,
        })
        .expect("session_open result");
    let scalar_i32 = program
        .type_catalog()
        .iter()
        .find_map(|(ty, descriptor)| {
            (descriptor.abi
                == crate::core::mir::types::MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| ty.clone())
        })
        .expect("canonical i32 TypeDesc");
    function.values.get_mut(&result).expect("result value").ty = scalar_i32;
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("non-SessionChan SessionOpen result must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("canonical SessionChan contract")
            || error.message.contains("SessionChan endpoint type")
    }));
}

#[test]
fn materializes_plain_session_pair_as_copy_tuple_for_all_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_pair.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("plain session_pair must lower to canonical MIR");
    let owner = crate::core::NodeId("function:pair_order".into());
    let function = program.functions().get(&owner).expect("pair_order MIR");
    let result = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                result,
                kind: crate::core::mir::types::MirBuiltinKind::SessionPair,
                arguments,
            } => {
                assert!(arguments.is_empty(), "session_pair has no value arguments");
                Some(result.clone())
            }
            _ => None,
        })
        .expect("session_pair builtin instruction");
    let result_ty = function
        .values
        .get(&result)
        .expect("session_pair result value")
        .ty
        .clone();
    program
        .type_catalog()
        .validate_plain_session_pair(&result_ty)
        .expect("session_pair result Copy tuple contract");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference session_pair execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume canonical SessionPair");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume canonical SessionPair");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume canonical SessionPair");
}

#[test]
fn rejects_forged_session_pair_result_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_pair.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical session_pair MIR");
    let owner = crate::core::NodeId("function:pair_order".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("pair_order MIR");
    let result = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::BuiltinCall {
                kind: crate::core::mir::types::MirBuiltinKind::SessionPair,
                result,
                ..
            } => Some(result.clone()),
            _ => None,
        })
        .expect("session_pair result");
    let scalar_i32 = program
        .type_catalog()
        .iter()
        .find_map(|(ty, descriptor)| {
            (descriptor.abi
                == crate::core::mir::types::MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                })
            .then(|| ty.clone())
        })
        .expect("canonical i32 TypeDesc");
    function.values.get_mut(&result).expect("result value").ty = scalar_i32;
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("non-tuple SessionPair result must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("canonical session-pair contract")
            || error.message.contains("two-field tuple layout")
    }));
}

#[test]
fn materializes_typed_session_pair_direct_binding_for_all_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_bind.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("typed SessionPair direct binding must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = program
        .functions()
        .get(&owner)
        .expect("typed pair main MIR");
    let pair = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionPairBind {
                lo,
                hi,
                contract: Some(contract),
            } => Some((lo.clone(), hi.clone(), contract.clone())),
            _ => None,
        })
        .expect("typed pair binding instruction");
    let (lo, hi, contract) = pair;
    let lo_ty = function.values.get(&lo).expect("lo value").ty.clone();
    let hi_ty = function.values.get(&hi).expect("hi value").ty.clone();
    program
        .type_catalog()
        .validate_session_pair_bind_receipt(&contract.pair_ty, &lo_ty, &hi_ty, &contract)
        .expect("typed pair binding receipt");
    program
        .type_catalog()
        .validate_session_channel(&lo_ty)
        .expect("lo endpoint TypeDesc");
    program
        .type_catalog()
        .validate_session_channel(&hi_ty)
        .expect("hi endpoint TypeDesc");
    assert_eq!(
        program
            .type_catalog()
            .get(&lo_ty)
            .and_then(|descriptor| descriptor.session_protocol.as_ref()),
        Some(&contract.lo_protocol),
        "lo endpoint protocol identity must be carried by its TypeDesc receipt"
    );
    assert_eq!(
        program
            .type_catalog()
            .get(&hi_ty)
            .and_then(|descriptor| descriptor.session_protocol.as_ref()),
        Some(&contract.hi_protocol),
        "hi endpoint protocol identity must be carried by its TypeDesc receipt"
    );

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference typed session_pair execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume canonical typed SessionPair binding");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume canonical typed SessionPair binding");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume canonical typed SessionPair binding");
}

#[test]
fn rejects_typed_session_pair_non_binding_pattern_before_consumers() {
    let checked = checked_program(
        r#"
session S = end
func main() -> i32 {
    let pair = session_pair::<S>()
    drop(pair)
    0
}
"#,
    );
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("typed SessionPair aggregate binding must remain fail-closed");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("direct endpoint bindings")
            || rendered.contains("canonical aggregate")
            || rendered.contains("glue"),
        "typed SessionPair rejection must identify the unsupported aggregate shape: {rendered}"
    );
}

#[test]
fn executes_typed_session_pair_send_recv_roundtrip_across_mir_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_send_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("typed SessionPair send/recv roundtrip must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = program.functions().get(&owner).expect("roundtrip main MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                contract: Some(contract),
                ..
            } => Some((*operation, contract.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(session_calls.len(), 4);
    assert_eq!(
        session_calls
            .iter()
            .filter(|(operation, _)| {
                *operation == crate::core::mir::types::MirSessionOperation::Send
            })
            .count(),
        1
    );
    assert_eq!(
        session_calls
            .iter()
            .filter(|(operation, _)| {
                *operation == crate::core::mir::types::MirSessionOperation::Recv
            })
            .count(),
        1
    );
    assert!(session_calls.iter().all(|(_, contract)| !contract.terminal));
    assert_eq!(
        session_calls
            .iter()
            .filter(|(operation, _)| {
                *operation == crate::core::mir::types::MirSessionOperation::Close
            })
            .count(),
        2
    );

    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&owner, &[])
        .expect("reference pair send/recv execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(41)
    );

    let bytecode = crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the canonical pair roundtrip MIR");
    let value = crate::interp::bytecode::BytecodeVM::new(bytecode)
        .run_value()
        .expect("bytecode pair send/recv execution");
    assert!(matches!(value, crate::interp::Value::Int(41)));

    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the canonical pair roundtrip MIR");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the canonical pair roundtrip MIR");
}

#[test]
fn rejects_forged_typed_session_pair_roundtrip_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_send_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical pair roundtrip MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("roundtrip main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::SessionPairBind { .. }))
        .expect("typed pair binding instruction");
    let MirInstructionKind::SessionPairBind { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let receipt = contract.as_mut().expect("typed pair receipt");
    receipt.lo_ty = receipt.hi_ty.clone();
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged pair receipt must be rejected before consumers");
    assert!(errors.iter().any(|error| {
        error.message.contains("typed session_pair binding receipt")
            || error.message.contains("endpoint identities")
    }));
}

#[test]
fn rejects_forged_typed_session_pair_protocol_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_typed_session_pair_send_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical pair roundtrip MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("roundtrip main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::SessionPairBind { .. }))
        .expect("typed pair binding instruction");
    let MirInstructionKind::SessionPairBind { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let receipt = contract.as_mut().expect("typed pair receipt");
    receipt.lo_protocol = receipt.hi_protocol.clone();
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged protocol receipt must be rejected before consumers");
    assert!(errors.iter().any(|error| {
        error.message.contains("typed session_pair binding receipt")
            || error.message.contains("protocol identity")
    }));
}

#[test]
fn materializes_generic_tuple_copy_projection_for_all_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_native_generic_tuple_projection.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic tuple projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarTupleProjection { .. }
            )
        })
        .expect("generic tuple projection instance");
    let MirGenericInstanceContract::ScalarTupleProjection { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.arity, 2);
    assert_eq!(contract.field_index, 0);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized tuple projection target");
    assert_eq!(contract.field_ty, target.result);
    assert!(target.canonical_text().contains("project"));
    let parameter = target.parameters.first().expect("tuple parameter");
    let parameter_ty = target
        .values
        .get(parameter)
        .expect("tuple parameter value")
        .ty
        .clone();
    program
        .type_catalog()
        .validate_flat_copy_tuple(&parameter_ty)
        .expect("specialized tuple must be flat Copy");

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic tuple projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume generic tuple projection MIR");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume generic tuple projection MIR");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume generic tuple projection MIR");
}

#[test]
fn rejects_generic_tuple_projection_outside_two_element_copy_island() {
    let checked = checked_program(
        r#"
func first<T>(pair: (T, T, T)) -> T { pair.0 }
func main() -> i32 { first((1, 2, 3)) }
"#,
    );
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("three-element generic tuple projection must remain fail-closed");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("two-element")
            || rendered.contains("generic tuple projection")
            || rendered.contains("scalar contract"),
        "unsupported generic tuple projection must have a stable diagnostic: {rendered}"
    );
}

#[test]
fn materializes_integer_session_send_with_backend_neutral_receipt() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_send.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("integer SessionChan send must lower to canonical MIR");
    let owner = crate::core::NodeId("function:send_once".into());
    let function = program.functions().get(&owner).expect("send_once MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                endpoint,
                payload: Some(payload),
                contract: Some(contract),
                ..
            } => Some((
                *operation,
                endpoint.clone(),
                payload.clone(),
                contract.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        session_calls.len(),
        1,
        "send must be one explicit SessionCall"
    );
    let (operation, endpoint, payload, contract) = &session_calls[0];
    assert_eq!(
        *operation,
        crate::core::mir::types::MirSessionOperation::Send
    );
    let endpoint_ty = function
        .values
        .get(endpoint)
        .expect("endpoint value")
        .ty
        .clone();
    let payload_ty = function
        .values
        .get(payload)
        .expect("payload value")
        .ty
        .clone();
    assert_eq!(contract.endpoint_ty, endpoint_ty);
    assert_eq!(contract.payload_ty.as_ref(), Some(&payload_ty));
    assert!(!contract.terminal);
    assert!(contract.before.as_str().starts_with("!i64"));
    assert_eq!(contract.after.as_str(), "end");
    let payload_conversions = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| matches!(instruction.kind, MirInstructionKind::Convert { .. }))
        .count();
    assert_eq!(
        payload_conversions, 1,
        "literal i32 payload must carry one explicit checker-approved i64 conversion"
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
        )
        .expect("reference SessionCall execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(17));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the same canonical SessionCall");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the same canonical SessionCall");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the same canonical SessionCall");
}

#[test]
fn materializes_integer_session_recv_with_deterministic_queue_oracle() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("integer SessionChan recv must lower to canonical MIR");
    let owner = crate::core::NodeId("function:recv_once".into());
    let function = program.functions().get(&owner).expect("recv_once MIR");
    let session_calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation,
                contract: Some(contract),
                payload,
                ..
            } => Some((*operation, contract.clone(), payload.is_some())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        session_calls.len(),
        2,
        "recv followed by close is explicit MIR"
    );
    let (operation, recv_contract, has_payload) = session_calls
        .iter()
        .find(|(operation, _, _)| *operation == crate::core::mir::types::MirSessionOperation::Recv)
        .expect("recv receipt");
    assert_eq!(
        *operation,
        crate::core::mir::types::MirSessionOperation::Recv
    );
    assert!(!has_payload);
    assert!(recv_contract.before.as_str().starts_with("?i64"));
    assert_eq!(recv_contract.after.as_str(), "end");
    assert!(!recv_contract.terminal);
    assert!(recv_contract.payload_ty.is_none());

    let interpreter = crate::core::mir::reference::MirReferenceInterpreter::new(&program);
    let value = interpreter
        .execute_with_session_queues(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
            &[crate::core::mir::reference::MirSessionQueueInput {
                endpoint: 17,
                values: vec![73],
            }],
        )
        .expect("reference SessionCall recv execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(73));
    let missing = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
        )
        .expect_err("recv without an explicit queue must fail closed");
    assert!(missing.message.contains("queue is missing or exhausted"));

    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the same canonical SessionCall");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the same canonical SessionCall");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier capability gate must consume the same canonical SessionCall");
}

#[test]
fn materializes_i32_session_recv_with_range_checked_queue_oracle() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_recv_i32.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("i32 SessionChan recv must lower to canonical MIR");
    let owner = crate::core::NodeId("function:recv_once".into());
    let function = program.functions().get(&owner).expect("recv_once MIR");
    let (result, contract) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::SessionCall {
                operation: crate::core::mir::types::MirSessionOperation::Recv,
                result,
                contract: Some(contract),
                ..
            } => Some((result.clone(), contract.clone())),
            _ => None,
        })
        .expect("i32 recv receipt");
    assert!(contract.before.as_str().starts_with("?i32"));
    assert_eq!(
        function
            .values
            .get(&result)
            .expect("recv result")
            .ty
            .clone(),
        contract.result_ty
    );
    assert_eq!(
        program
            .type_catalog()
            .get(&contract.result_ty)
            .expect("i32 result TypeDesc")
            .abi,
        crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true
        }
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute_with_session_queues(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(
                2_147_483_647,
            )],
            &[crate::core::mir::reference::MirSessionQueueInput {
                endpoint: 17,
                values: vec![2_147_483_647],
            }],
        )
        .expect("i32 queue value in range");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::Int(2_147_483_647)
    );
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute_with_session_queues(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(17)],
            &[crate::core::mir::reference::MirSessionQueueInput {
                endpoint: 17,
                values: vec![2_147_483_648],
            }],
        )
        .expect_err("i32 queue overflow must trap");
    assert!(error.message.contains("outside the canonical signed range"));
}

#[test]
fn rejects_forged_session_close_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_close.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical SessionCall");
    let owner = crate::core::NodeId("function:close_endpoint".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("close_endpoint MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::SessionCall { .. }))
        .expect("SessionCall instruction");
    let MirInstructionKind::SessionCall { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    contract.as_mut().expect("receipt").after =
        crate::core::SessionResidualId::new("not-closed").expect("test residual");
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged SessionCall receipt must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("session_close must be terminal")
            || error.message.contains("SessionCall receipt disagrees")
    }));
}

#[test]
fn rejects_forged_session_send_payload_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_send.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical SessionCall");
    let owner = crate::core::NodeId("function:send_once".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("send_once MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::SessionCall {
                    operation: crate::core::mir::types::MirSessionOperation::Send,
                    ..
                }
            )
        })
        .expect("SessionCall instruction");
    let MirInstructionKind::SessionCall { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let receipt = contract.as_mut().expect("receipt");
    receipt.payload_ty = Some(receipt.result_ty.clone());
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged SessionCall payload receipt must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("session_send payload")
            || error.message.contains("SessionCall payload")
    }));
}

#[test]
fn rejects_forged_session_recv_result_receipt_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_recv.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical SessionCall");
    let owner = crate::core::NodeId("function:recv_once".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("recv_once MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::SessionCall {
                    operation: crate::core::mir::types::MirSessionOperation::Recv,
                    ..
                }
            )
        })
        .expect("SessionCall instruction");
    let MirInstructionKind::SessionCall { contract, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let endpoint_ty = contract.as_ref().expect("receipt").endpoint_ty.clone();
    contract.as_mut().expect("receipt").result_ty = endpoint_ty;
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
            program.transitions().clone(),
        )
        .expect_err("forged SessionCall result receipt must be rejected");
    assert!(errors.iter().any(|error| {
        error.message.contains("session_recv result")
            || error.message.contains("SessionCall receipt disagrees")
    }));
}

#[test]
fn ordinary_session_call_materializes_transfer_effect_receipt() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("ordinary SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let function = program.functions().get(&owner).expect("forward MIR");
    let (argument, receipts) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                arguments,
                effect_receipts,
                ..
            } if callee.0 == "function:pass" => {
                Some((arguments[0].clone(), effect_receipts.clone()))
            }
            _ => None,
        })
        .expect("ordinary SessionChan call");
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(receipt.argument_index, 0);
    assert_eq!(
        receipt.kind,
        crate::core::mir::types::MirCallEffectKind::TransferSession
    );
    assert_eq!(
        receipt.argument_ty,
        function.values.get(&argument).expect("call argument").ty
    );
    program
        .type_catalog()
        .validate_session_channel(&receipt.argument_ty)
        .expect("receipt argument TypeDesc");

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::Int(29)],
        )
        .expect("reference ordinary SessionChan call");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(29));
    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume the canonical call receipt");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume the canonical call receipt");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier must consume the canonical call receipt");
}

#[test]
fn missing_session_call_transfer_receipt_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("ordinary SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("forward MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("ordinary SessionChan call");
    let MirInstructionKind::Call {
        effect_receipts, ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    effect_receipts.clear();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("missing SessionChan call receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("SessionChan call argument 0 has no canonical TransferSession effect receipt")
    }));
}

#[test]
fn multi_session_call_materializes_ordered_transfer_effect_receipts() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer_multi.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("multi-argument SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let function = program.functions().get(&owner).expect("forward MIR");
    let (arguments, receipts) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                arguments,
                effect_receipts,
                ..
            } if callee.0 == "function:sink" => Some((arguments.clone(), effect_receipts.clone())),
            _ => None,
        })
        .expect("multi-argument SessionChan call");
    assert_eq!(arguments.len(), 2);
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].argument_index, 0);
    assert_eq!(receipts[1].argument_index, 1);
    assert!(receipts
        .windows(2)
        .all(|window| { window[0].argument_index < window[1].argument_index }));
    for receipt in &receipts {
        assert_eq!(
            receipt.kind,
            crate::core::mir::types::MirCallEffectKind::TransferSession
        );
        assert_eq!(
            receipt.argument_ty,
            function
                .values
                .get(&arguments[receipt.argument_index])
                .expect("call argument")
                .ty
        );
        program
            .type_catalog()
            .validate_call_effect_contract(receipt)
            .expect("SessionChan effect receipt TypeDesc");
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &owner,
            &[
                crate::core::mir::reference::MirRuntimeValue::Int(11),
                crate::core::mir::reference::MirRuntimeValue::Int(22),
            ],
        )
        .expect("reference multi-argument SessionChan call");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(11));
    crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode must consume multi-argument call receipts");
    crate::codegen::mir::validate_mir_native(&program)
        .expect("native validator must consume multi-argument call receipts");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier must consume multi-argument call receipts");
}

#[test]
fn missing_second_multi_session_call_receipt_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer_multi.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("multi-argument SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("forward MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("multi-argument SessionChan call");
    let MirInstructionKind::Call {
        effect_receipts, ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    effect_receipts.retain(|receipt| receipt.argument_index != 1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("missing second SessionChan call receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("SessionChan call argument 1 has no canonical TransferSession effect receipt")
    }));
}

#[test]
fn reordered_multi_session_call_receipts_are_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_session_call_transfer_multi.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("multi-argument SessionChan call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:forward".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("forward MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("multi-argument SessionChan call");
    let MirInstructionKind::Call {
        effect_receipts, ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    effect_receipts.reverse();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("reordered SessionChan call receipts must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("call effect receipts are not in canonical argument order")
    }));
}

#[test]
fn protocol_method_call_uses_checker_method_identity_across_mir_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let main = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    let method = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method },
                arguments,
                ..
            } => Some((protocol.clone(), method.clone(), arguments.clone())),
            _ => None,
        })
        .expect("ProtocolMethod MIR call");
    assert_eq!(method.0 .0, "trait:Read");
    assert!(method
        .1
        .as_str()
        .starts_with("function:Read:for:Counter::read:"));
    let target = crate::core::mir::canonical_protocol_call_target(
        &crate::core::ir::ResolvedCallee::ProtocolMethod {
            protocol: method.0,
            method: method.1,
        },
    )
    .expect("ProtocolMethod has a concrete MIR target");
    assert!(program.functions().contains_key(&target));
    assert_eq!(method.2.len(), 1);

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference ProtocolMethod execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
    let bytecode = crate::interp::bytecode::compile_mir_program(&program)
        .expect("bytecode ProtocolMethod consumer");
    assert!(!bytecode.functions.is_empty());
    crate::codegen::mir::validate_mir_native(&program).expect("native ProtocolMethod validator");
    crate::verifier::validate_mir_capabilities(&program)
        .expect("verifier ProtocolMethod capability gate");
}

#[test]
fn protocol_method_with_non_function_identity_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("ProtocolMethod MIR call");
    let MirInstructionKind::Call { callee, .. } = &mut instruction.kind else {
        unreachable!();
    };
    *callee = crate::core::ir::ResolvedCallee::ProtocolMethod {
        protocol: crate::core::NodeId("trait:Read".into()),
        method: crate::core::ir::MethodId::new("method:Read::read").expect("method id"),
    };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("non-function ProtocolMethod identity must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("is not a canonical function identity")
    }));
}

#[test]
fn protocol_method_protocol_identity_drift_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
        .expect("ProtocolMethod MIR call");
    let MirInstructionKind::Call { callee, .. } = &mut instruction.kind else {
        unreachable!();
    };
    let crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, .. } = callee else {
        unreachable!();
    };
    *protocol = crate::core::NodeId("trait:Other".into());
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("protocol/method identity drift must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("disagrees with protocol 'trait:Other'")
    }));
}

#[test]
fn protocol_method_argument_typedesc_drift_is_rejected_before_consumers() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    let (argument, result) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                arguments,
                result: Some(result),
                ..
            } => Some((arguments[0].clone(), result.clone())),
            _ => None,
        })
        .expect("ProtocolMethod MIR call");
    let i64_ty = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi
                == crate::core::mir::types::MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                })
            .then(|| id.clone())
        })
        .expect("canonical i64 TypeDesc");
    function
        .values
        .get_mut(&argument)
        .expect("receiver value")
        .ty = i64_ty.clone();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("protocol method argument TypeDesc drift must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("protocol method argument 0 TypeDesc disagrees with canonical method ABI")
    }));

    let mut functions = program.functions().clone();
    let function = functions.get_mut(&owner).expect("main MIR");
    function.values.get_mut(&result).expect("call result").ty = i64_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("protocol method result TypeDesc drift must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("protocol method result TypeDesc disagrees with canonical method ABI")
    }));
}

#[test]
fn protocol_method_abi_contract_is_shared_and_rejects_arity_drift() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_protocol_method.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("protocol method must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let caller = program.functions().get(&owner).expect("main MIR");
    let (callee, arguments, result) = caller
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::ProtocolMethod { protocol, method },
                arguments,
                result,
                ..
            } => Some((
                crate::core::ir::ResolvedCallee::ProtocolMethod {
                    protocol: protocol.clone(),
                    method: method.clone(),
                },
                arguments.clone(),
                result.clone(),
            )),
            _ => None,
        })
        .expect("ProtocolMethod MIR call");
    let target_owner = crate::core::mir::canonical_protocol_call_target(&callee)
        .expect("ProtocolMethod has canonical target");
    let target = program
        .functions()
        .get(&target_owner)
        .expect("protocol method target MIR");
    assert!(crate::core::mir::validate_protocol_method_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &arguments,
    )
    .is_empty());
    assert!(
        crate::core::mir::validate_materialized_call_result_presence(
            &callee,
            target,
            result.as_ref(),
            program.type_catalog(),
        )
        .is_empty()
    );

    let missing_result = crate::core::mir::validate_materialized_call_result_presence(
        &callee,
        target,
        None,
        program.type_catalog(),
    );
    assert_eq!(
        missing_result,
        vec!["protocol method result value is absent for non-unit canonical method ABI"]
    );

    let mut malformed_arguments = arguments;
    malformed_arguments.pop();
    let errors = crate::core::mir::validate_protocol_method_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &malformed_arguments,
    );
    assert_eq!(
        errors,
        vec!["protocol method call arity disagrees with canonical method ABI"]
    );
}

#[test]
fn direct_function_abi_contract_is_shared_and_rejects_arity_drift() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_native_f64_add.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("direct function must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let caller = program.functions().get(&owner).expect("main MIR");
    let (callee, arguments, result) = caller
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                arguments,
                result,
                ..
            } if callee.0 == "function:add" => Some((
                crate::core::ir::ResolvedCallee::Function(callee.clone()),
                arguments.clone(),
                result.clone(),
            )),
            _ => None,
        })
        .expect("direct function MIR call");
    let target_owner = crate::core::mir::canonical_protocol_call_target(&callee)
        .expect("Function has canonical target");
    let target = program
        .functions()
        .get(&target_owner)
        .expect("direct function target MIR");
    assert!(crate::core::mir::validate_materialized_call_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &arguments,
    )
    .is_empty());
    assert!(
        crate::core::mir::validate_materialized_call_result_presence(
            &callee,
            target,
            result.as_ref(),
            program.type_catalog(),
        )
        .is_empty()
    );

    let missing_result = crate::core::mir::validate_materialized_call_result_presence(
        &callee,
        target,
        None,
        program.type_catalog(),
    );
    assert_eq!(
        missing_result,
        vec![format!(
            "non-unit callee '{}' has no MIR result value",
            target.result.as_str()
        )]
    );

    let mut malformed_arguments = arguments;
    malformed_arguments.pop();
    let errors = crate::core::mir::validate_materialized_call_abi(
        &callee,
        caller,
        target,
        result.as_ref(),
        &malformed_arguments,
    );
    assert_eq!(
        errors,
        vec!["call to 'function:add' supplies 1 arguments but its MIR signature requires 2"]
    );
}

#[test]
fn canonical_gate_rejects_non_unit_call_without_result_value() {
    let checked = checked_program(include_str!(
        "../../../tests/fixtures/mir_native_f64_add.mimi"
    ));
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("direct function must lower to canonical MIR");
    let main_owner = crate::core::NodeId("function:main".into());
    let mut functions = program.functions().clone();
    let main = functions.get_mut(&main_owner).expect("main MIR");
    let call = main
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(owner),
                result,
                ..
            } if owner.0 == "function:add" => Some(result),
            _ => None,
        })
        .expect("direct function call");
    *call = None;

    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        program.type_catalog().clone(),
    )
    .expect_err("non-unit direct call without a result must fail at MIR admission");
    assert!(errors.iter().any(|error| {
        error.message
            == format!(
                "non-unit callee '{}' has no MIR result value",
                program
                    .functions()
                    .get(&crate::core::NodeId("function:add".into()))
                    .expect("add MIR")
                    .result
                    .as_str()
            )
    }));
}

#[test]
fn materializes_generic_option_predicate_with_a_specialized_variant_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option predicate must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantPredicate {
                    contract: crate::core::mir::types::MirVariantPredicateContract {
                        predicate: crate::core::mir::MirVariantPredicate::IsSome,
                        ..
                    }
                }
            )
        })
        .expect("generic Option predicate instance");
    let MirGenericInstanceContract::ScalarVariantPredicate { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.alternate_variant_name, "None");
    assert_eq!(contract.discriminant, 1);

    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized generic predicate target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantPredicate {
                contract: Some(receipt),
                predicate: crate::core::mir::MirVariantPredicate::IsSome,
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0], contract);
    let Some(option_desc) = program.type_catalog().get(&receipts[0].variant_ty) else {
        panic!("specialized Option TypeDesc is absent");
    };
    let crate::core::mir::types::MirLayout::Option { inner, .. } = &option_desc.layout else {
        panic!("generic predicate receipt must point at an Option TypeDesc");
    };
    assert_eq!(inner, &instance.arguments[0]);

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option predicate execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_option_predicate_for_non_copy_payload_before_legacy() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_predicate_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy generic Option predicate must fail closed");
    let message = error.to_string();
    assert!(
        message.contains("MIR lowering failed"),
        "unexpected rejection: {message}"
    );
}

#[test]
fn materializes_generic_option_unwrap_with_a_specialized_projection_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { .. }
            )
        })
        .expect("generic Option projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.discriminant, 1);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_f64_with_float_projection_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f64> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Copy
            )
        })
        .expect("generic Option<f64> projection instance");
    assert_eq!(instance.arguments.len(), 1);
    let payload = program
        .type_catalog()
        .get(&instance.arguments[0])
        .expect("generic Option<f64> payload TypeDesc");
    assert_eq!(
        payload.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(payload.layout, crate::core::mir::types::MirLayout::Scalar);
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered to generic variant projection");
    };
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f64> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn materializes_generic_option_unwrap_owned_string_with_move_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_owned_string.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<string> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Move
            )
        })
        .expect("owned generic Option projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.discriminant, 1);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Move);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::OwnedString);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized owned generic Option target");
    assert!(target.blocks.values().any(|block| {
        matches!(
            block.instructions.as_slice(),
            [
                MirInstruction {
                    kind: MirInstructionKind::Move { .. },
                    ..
                },
                MirInstruction {
                    kind: MirInstructionKind::VariantProjectMove {
                        contract: Some(_),
                        ..
                    },
                    ..
                }
            ]
        )
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<string> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_owned_list_with_move_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_owned_list.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<List<i32>> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Move
            )
        })
        .expect("owned generic Option<List> projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::List);
    let (_, payload_glue) = program
        .type_catalog()
        .validate_option_move_variant(&contract.source_ty)
        .expect("Option<List<i32>> TypeDesc receipt");
    assert_eq!(payload_glue, MirGlueKind::List);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized owned generic Option<List> target");
    assert!(target.blocks.values().any(|block| {
        matches!(
            block.instructions.as_slice(),
            [
                MirInstruction {
                    kind: MirInstructionKind::Move { .. },
                    ..
                },
                MirInstruction {
                    kind: MirInstructionKind::VariantProjectMove {
                        contract: Some(_),
                        ..
                    },
                    ..
                }
            ]
        )
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<List> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_owned_list_i64_and_bool_family() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_list_scalars.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<List<i64|bool>> unwrap must lower to canonical MIR");
    let instances = program
        .instances()
        .values()
        .filter_map(|instance| match &instance.contract {
            MirGenericInstanceContract::ScalarVariantProjection { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Option"
                    && contract.projection.ownership == MirOwnership::Move
                    && contract.projection.move_out_glue == MirGlueKind::List =>
            {
                Some(instance)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        instances.len(),
        2,
        "both concrete List scalar instances must materialize"
    );
    let element_kinds = instances
        .iter()
        .filter_map(|instance| {
            let descriptor = program.type_catalog().get(&instance.arguments[0])?;
            let crate::core::mir::types::MirLayout::List { element } = &descriptor.layout else {
                return None;
            };
            Some(program.type_catalog().get(element)?.kind.clone())
        })
        .collect::<Vec<_>>();
    assert!(element_kinds.iter().any(|kind| {
        matches!(
            kind,
            crate::core::mir::types::MirTypeKind::Primitive(PrimitiveType::I64)
        )
    }));
    assert!(element_kinds.iter().any(|kind| {
        matches!(
            kind,
            crate::core::mir::types::MirTypeKind::Primitive(PrimitiveType::Bool)
        )
    }));
    for instance in &instances {
        let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
        else {
            unreachable!("filtered to generic variant projections");
        };
        let (_, payload_glue) = program
            .type_catalog()
            .validate_option_move_variant(&contract.source_ty)
            .expect("Option<List<Copy scalar>> TypeDesc receipt");
        assert_eq!(payload_glue, MirGlueKind::List);
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<List<i64|bool>> unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_option_unwrap_owned_float_list_before_legacy() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_owned_float_list_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic Option<List<f64>> unwrap must fail closed");
    let text = error.to_string();
    let debug = format!("{error:?}");
    assert!(text.contains("MIR lowering failed"));
    assert!(
        debug.contains("not a Copy signed scalar/bool with no-op glue"),
        "{debug}"
    );
}

#[test]
fn generic_option_unwrap_none_preserves_the_canonical_trap() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap None MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("generic Option unwrap None must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn materializes_generic_option_unwrap_or_with_a_specialized_fallback_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Option fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Some");
    assert_eq!(contract.discriminant, 1);
    assert_eq!(contract.fallback_variant_name, "None");
    assert_eq!(contract.fallback_discriminant, 0);
    assert_eq!(contract.fallback_arity, 0);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_generic_option_unwrap_or_f64_with_float_fallback_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f64>.unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Copy
            )
        })
        .expect("generic Option<f64> fallback projection instance");
    assert_eq!(instance.arguments.len(), 1);
    let payload = program
        .type_catalog()
        .get(&instance.arguments[0])
        .expect("generic Option<f64> fallback payload TypeDesc");
    assert_eq!(
        payload.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered to generic fallback projection");
    };
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.fallback_arity, 0);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f64>.unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_option_unwrap_or_f64_none_selects_the_fallback() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_f64_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option<f64> unwrap_or None MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option<f64>.unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_option_unwrap_or_none_selects_the_explicit_fallback() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap_or None MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Option unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn materializes_generic_option_unwrap_or_owned_string_with_consuming_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Option"
                        && contract.projection.ownership == MirOwnership::Move
            )
        })
        .expect("managed generic Option fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::OwnedString);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("managed generic Option fallback target");
    assert!(target.blocks.values().any(|block| {
        matches!(
            block.instructions.as_slice(),
            [MirInstruction {
                kind: MirInstructionKind::VariantProjectOr {
                    base,
                    fallback,
                    contract: Some(_),
                    ..
                },
                ..
            }] if base == &target.parameters[0] && fallback == &target.parameters[1]
        )
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_option_unwrap_or_owned_string_none_transfers_fallback() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_string_none.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option unwrap_or None must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn materializes_generic_option_unwrap_or_owned_list_with_consuming_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option<List<i32>> unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                    if contract.projection.ownership == MirOwnership::Move
                        && contract.projection.move_out_glue == MirGlueKind::List
            )
        })
        .expect("managed generic Option<List> fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Option");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option<List> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_option_unwrap_or_owned_list_none_transfers_fallback() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list_none.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic managed Option<List> unwrap_or None must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Option<List> unwrap_or None execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn rejects_generic_option_unwrap_or_unsupported_managed_payload_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported managed Option fallback must remain fail-closed");
    assert!(error.to_string().contains("generic MIR instance"));
}

#[test]
fn rejects_generic_option_unwrap_or_owned_list_with_non_copy_element_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_option_unwrap_or_owned_list_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List<string> fallback must remain outside the managed List island");
    assert!(error.to_string().contains("generic MIR instance"));
}

#[test]
fn generic_option_unwrap_or_stale_receipt_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap_or MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Option fallback projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic Option fallback target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProjectOr {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("fallback projection receipt");
    receipt.fallback_discriminant = receipt.fallback_discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic Option fallback receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection fallback receipt disagrees with TypeDesc")
    }));
}

#[test]
fn generic_option_unwrap_stale_receipt_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Option unwrap MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { .. }
            )
        })
        .expect("generic Option projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic Option projection target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("projection receipt");
    receipt.discriminant = receipt.discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic Option projection receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection trap receipt disagrees with TypeDesc")
    }));
}

#[test]
fn materializes_generic_result_unwrap_with_a_specialized_projection_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { .. }
            )
        })
        .expect("generic Result projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.discriminant, 0);
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_result_unwrap_err_preserves_the_canonical_trap() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_none.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap Err MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("generic Result unwrap Err must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn materializes_generic_result_distinct_unwrap_with_copy_scalar_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        })
        .expect("generic distinct Result projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(
        ok, error,
        "distinct projection must preserve separate payload IDs"
    );
    program
        .type_catalog()
        .validate_copy_result_scalar_variant(&contract.source_ty)
        .expect("heterogeneous Copy Result TypeDesc contract");
    assert!(
        program
            .type_catalog()
            .validate_flat_copy_variant(&contract.source_ty)
            .is_err(),
        "the homogeneous concrete Result island remains closed"
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference distinct Result unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_result_distinct_unwrap_stale_receipt_is_rejected_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic distinct Result unwrap MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        })
        .expect("generic distinct Result projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic distinct Result target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("distinct Result projection receipt");
    receipt.discriminant = receipt.discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic distinct Result receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection trap receipt disagrees with TypeDesc")
    }));
}

#[test]
fn generic_result_distinct_unwrap_err_preserves_the_canonical_trap() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_err.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic distinct Result Err MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("generic distinct Result unwrap Err must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn materializes_generic_result_f64_unwrap_with_heterogeneous_copy_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_f64.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> f64 unwrap must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                &instance.contract,
                MirGenericInstanceContract::ScalarVariantProjection { contract }
                    if contract.projection.nominal.as_str() == "builtin:type:Result"
            )
        })
        .expect("generic Result f64 projection instance");
    let MirGenericInstanceContract::ScalarVariantProjection { contract } = &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized heterogeneous Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(ok, error);
    assert!(matches!(
        program
            .type_catalog()
            .get(ok)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Float { bits: 64 })
    ));
    assert!(matches!(
        program
            .type_catalog()
            .get(error)
            .map(|descriptor| descriptor.abi),
        Some(crate::core::mir::types::MirAbiClass::Integer {
            bits: 32,
            signed: true,
        })
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result f64 unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(42));
}

#[test]
fn generic_result_f64_unwrap_err_preserves_the_canonical_trap() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_f64_err.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result<T, i32> f64 Err MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("generic Result f64 unwrap Err must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn generic_result_homogeneous_f64_unwrap_is_rejected_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_homogeneous_f64_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("homogeneous Result<T, T> f64 must remain outside the generic island");
    let message = format!("{error:?}");
    assert!(
        message.contains("generic MIR instance") && message.contains("f64"),
        "unexpected lowering rejection: {message}"
    );
}

#[test]
fn direct_result_f64_unwrap_remains_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_result_f64_unwrap_rejected.mimi");
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("direct concrete Result<f64, i32> must remain outside the generic island");
    let message = format!("{error:?}");
    assert!(
        message.contains("Result projection") || message.contains("variant projection"),
        "unexpected direct Result<f64> rejection: {message}"
    );
}

#[test]
fn materializes_generic_result_unwrap_or_with_a_specialized_fallback_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Result fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.discriminant, 0);
    assert_eq!(contract.fallback_variant_name, "Err");
    assert_eq!(contract.fallback_discriminant, 1);
    assert_eq!(contract.fallback_arity, 1);
    assert_eq!(contract.projection.field_index, 0);
    assert_eq!(contract.projection.arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Copy);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(48));
}

#[test]
fn materializes_generic_distinct_result_unwrap_or_with_two_payload_slots() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic heterogeneous Result unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic heterogeneous Result fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    let crate::core::mir::types::MirLayout::Result { ok, error, .. } = &program
        .type_catalog()
        .get(&contract.source_ty)
        .expect("specialized heterogeneous Result TypeDesc")
        .layout
    else {
        panic!("specialized source must retain a Result layout");
    };
    assert_ne!(ok, error);
    assert_eq!(ok, &contract.result_ty);
    assert!(matches!(
        program.type_catalog().get(error).map(|desc| &desc.kind),
        Some(crate::core::mir::types::MirTypeKind::Primitive(
            crate::core::PrimitiveType::I32
        ))
    ));
    assert_eq!(contract.fallback_ty, contract.result_ty);
    assert_eq!(contract.fallback_variant_name, "Err");
    assert_eq!(contract.fallback_discriminant, 1);
    assert_eq!(contract.fallback_arity, 1);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference heterogeneous Result unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(50));
}

#[test]
fn materializes_managed_generic_result_unwrap_or_with_move_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("managed Result unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("managed Result fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.fallback_variant_name, "Err");
    assert_eq!(contract.fallback_arity, 1);
    assert_eq!(contract.projection.ownership, MirOwnership::Move);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::OwnedString);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Result unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_managed_generic_bool_result_unwrap_or_with_move_receipt() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_owned_string.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("managed Result<bool> unwrap_or must lower to canonical MIR");
    let contracts = program
        .instances()
        .values()
        .filter_map(|instance| match &instance.contract {
            MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result" =>
            {
                Some(contract)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        contracts.len(),
        1,
        "one deduplicated String specialization serves Ok and Err"
    );
    assert!(contracts.iter().all(|contract| {
        contract.projection.ownership == MirOwnership::Move
            && contract.projection.move_out_glue == MirGlueKind::OwnedString
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Result<bool> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_managed_generic_bool_result_unwrap_or_outside_move_payload_contract() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_bool_unwrap_or_owned_rejected.mimi"
    );
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Result<List<string>, bool> fallback must fail closed");
    let message = error.to_string();
    assert!(message.contains("generic MIR instance") || message.contains("generic Result"));
}

#[test]
fn managed_generic_result_unwrap_or_moves_the_fallback_on_err() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_string_err.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("managed Result Err unwrap_or must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference managed Result Err unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn rejects_managed_generic_result_unwrap_or_outside_move_payload_contract() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_rejected.mimi"
    );
    let checked = checked_program(source);
    let errors = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported managed Result payload must fail closed");
    let message = errors.to_string();
    assert!(
        message.contains("generic Result fallback projection")
            || message.contains("generic MIR instance argument")
    );
}

#[test]
fn materializes_generic_result_unwrap_or_owned_list_with_move_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list.mimi");
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<List<i32>> unwrap_or must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("Result<List> fallback projection instance");
    let MirGenericInstanceContract::ScalarVariantProjectionFallback { contract } =
        &instance.contract
    else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.projection.ownership, MirOwnership::Move);
    assert_eq!(contract.projection.move_out_glue, MirGlueKind::List);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn generic_result_unwrap_or_owned_list_err_transfers_fallback() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_err.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<List<i32>> Err unwrap_or must lower to canonical MIR");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List> Err unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(7));
}

#[test]
fn rejects_generic_result_unwrap_or_owned_list_with_non_copy_element_before_consumers() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_rejected.mimi"
    );
    let checked = checked_program(source);
    let errors = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Result<List<string>> fallback must fail closed");
    let message = errors.to_string();
    assert!(
        message.contains("generic Result fallback projection")
            || message.contains("generic MIR instance argument")
    );
}

#[test]
fn materializes_generic_result_unwrap_or_owned_list_copy_scalar_family() {
    let source = include_str!(
        "../../../tests/fixtures/mir_native_generic_result_unwrap_or_owned_list_scalars.mimi"
    );
    let checked = checked_program(source);
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<List<i64|bool>> unwrap_or must lower to canonical MIR");
    let contracts = program
        .instances()
        .values()
        .filter_map(|instance| match &instance.contract {
            MirGenericInstanceContract::ScalarVariantProjectionFallback { contract }
                if contract.projection.nominal.as_str() == "builtin:type:Result" =>
            {
                Some(contract)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(contracts.len(), 2, "i64 and bool specializations");
    assert!(contracts.iter().all(|contract| {
        contract.projection.ownership == MirOwnership::Move
            && contract.projection.move_out_glue == MirGlueKind::List
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List<i64|bool>> unwrap_or execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(49));
}

#[test]
fn generic_result_unwrap_or_stale_receipt_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_or.mimi");
    let checked = checked_program(source);
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result unwrap_or MIR");
    let instance = canonical
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantProjectionFallback { .. }
            )
        })
        .expect("generic Result fallback projection instance");
    let mut functions = canonical.functions().clone();
    let target = functions
        .get_mut(&instance.function)
        .expect("materialized generic Result fallback target");
    let receipt = target
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::VariantProjectOr {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("fallback projection receipt");
    receipt.fallback_discriminant = receipt.fallback_discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog_and_instances(
        functions,
        canonical.type_catalog().clone(),
        canonical.instances().clone(),
    )
    .expect_err("stale generic Result fallback receipt must fail closed");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection fallback receipt disagrees with TypeDesc")
    }));
}

#[test]
fn rejects_generic_option_unwrap_for_unsupported_copy_payload_before_legacy() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_rejected.mimi");
    let checked = checked_program(source);
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported generic Option unwrap must fail closed");
    assert!(error.to_string().contains("MIR lowering failed"));
}

#[test]
fn rejects_generic_result_unwrap_and_option_unwrap_or_before_legacy() {
    for source in [
        include_str!("../../../tests/fixtures/mir_native_generic_result_unwrap_rejected.mimi"),
        include_str!(
            "../../../tests/fixtures/mir_native_generic_result_distinct_unwrap_or_rejected.mimi"
        ),
        include_str!("../../../tests/fixtures/mir_native_generic_option_unwrap_or_rejected.mimi"),
    ] {
        let checked = checked_program(source);
        let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
            .expect_err("unsupported generic variant projection must fail closed");
        assert!(error.to_string().contains("MIR lowering failed"));
    }
}

#[test]
fn materializes_generic_result_predicate_with_a_specialized_variant_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic Result predicate must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantPredicate {
                    contract: crate::core::mir::types::MirVariantPredicateContract {
                        predicate: crate::core::mir::MirVariantPredicate::IsOk,
                        ..
                    }
                }
            )
        })
        .expect("generic Result predicate instance");
    let MirGenericInstanceContract::ScalarVariantPredicate { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Ok");
    assert_eq!(contract.alternate_variant_name, "Err");
    assert_eq!(contract.discriminant, 0);
    let Some(result_desc) = program.type_catalog().get(&contract.variant_ty) else {
        panic!("specialized Result TypeDesc is absent");
    };
    assert!(matches!(
        result_desc.layout,
        crate::core::mir::types::MirLayout::Result { .. }
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference generic Result predicate execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_result_predicate_for_non_copy_payload_before_legacy() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_result_predicate_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy generic Result predicate must fail closed");
    let message = error.to_string();
    assert!(
        message.contains("MIR lowering failed"),
        "unexpected rejection: {message}"
    );
}

#[test]
fn materializes_generic_result_error_slot_predicate_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_result_error_slot.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<i32, T> predicate must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarVariantPredicate {
                    contract: crate::core::mir::types::MirVariantPredicateContract {
                        predicate: crate::core::mir::MirVariantPredicate::IsErr,
                        ..
                    }
                }
            )
        })
        .expect("generic Result error-slot predicate instance");
    let MirGenericInstanceContract::ScalarVariantPredicate { contract } = &instance.contract else {
        unreachable!("filtered above");
    };
    assert_eq!(contract.nominal.as_str(), "builtin:type:Result");
    assert_eq!(contract.variant_name, "Err");
    assert_eq!(contract.alternate_variant_name, "Ok");
    assert_eq!(contract.discriminant, 1);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<i32, T> predicate execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn lowers_option_string_unwrap_to_consuming_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_string_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<string>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_owned".into()))
        .expect("unwrap_owned MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProjectMove {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("unwrap must carry a consuming variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Move);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Option<string>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn lowers_option_i32_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_i32_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<i32>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_copy".into()))
        .expect("unwrap_copy MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("Copy unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Option<i32>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn lowers_result_i32_i32_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_result_i32_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Result<i32, i32>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_copy".into()))
        .expect("unwrap_copy MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("Result unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Ok");
    assert_eq!(receipt.projection.nominal.as_str(), "builtin:type:Result");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    program
        .type_catalog()
        .validate_copy_result_i32_variant(&receipt.source_ty)
        .expect("Result<i32, i32> TypeDesc contract");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<i32, i32>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:unwrap_err".into()), &[])
        .expect_err("Result::unwrap on Err must trap");
    assert!(
        error.message.contains("E0800"),
        "unexpected Result active-tag trap: {error}"
    );
}

#[test]
fn lowers_option_bool_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_bool_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<bool>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_copy".into()))
        .expect("unwrap_copy MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("Copy bool unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
}

#[test]
fn lowers_option_i64_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_i64_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<i64>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_copy".into()))
        .expect("unwrap_copy MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("Copy i64 unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Option<i64>.unwrap execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn lowers_option_f64_unwrap_to_copy_variant_projection() {
    let source = include_str!("../../../tests/fixtures/mir_native_option_f64_unwrap.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("typecheck");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<f64>.unwrap must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:unwrap_copy".into()))
        .expect("unwrap_copy MIR function");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::VariantProject {
                contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("Copy f64 unwrap must carry a read-only variant projection receipt");
    assert_eq!(receipt.variant_name, "Some");
    assert_eq!(receipt.projection.field_index, 0);
    assert_eq!(receipt.projection.arity, 1);
    assert_eq!(receipt.projection.ownership, MirOwnership::Copy);
    assert_eq!(receipt.projection.move_out_glue, MirGlueKind::Noop);
    let option_desc = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option)
                .then_some((id, descriptor))
        })
        .expect("Option<f64> TypeDesc");
    let crate::core::mir::types::MirLayout::Option { inner, .. } = &option_desc.1.layout else {
        panic!("Option<f64> must retain its Option layout");
    };
    let inner_desc = program
        .type_catalog()
        .get(inner)
        .expect("Option<f64> inner TypeDesc");
    assert_eq!(
        inner_desc.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(
        inner_desc.layout,
        crate::core::mir::types::MirLayout::Scalar
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:unwrap_copy".into()), &[])
        .expect("reference Option<f64>.unwrap execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits(41.5f64.to_bits())
    );
}

#[test]
fn lowers_f64_unary_negate_with_explicit_copy_float_contract() {
    let source = include_str!("../../../tests/fixtures/mir_native_f64_negate.mimi");
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("f64 unary negate must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:negate".into()))
        .expect("negate MIR function");
    let (result_ty, operand_ty) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Unary {
                result,
                op: crate::core::ir::ResolvedUnaryOp::Negate,
                operand,
            } => Some((
                function.values.get(result)?.ty.clone(),
                function.values.get(operand)?.ty.clone(),
            )),
            _ => None,
        })
        .expect("canonical f64 negate instruction");
    assert_eq!(result_ty, operand_ty);
    program
        .type_catalog()
        .validate_copy_float_unary(
            &result_ty,
            &operand_ty,
            crate::core::ir::ResolvedUnaryOp::Negate,
        )
        .expect("f64 negate TypeDesc contract");
    let descriptor = program
        .type_catalog()
        .get(&result_ty)
        .expect("f64 negate TypeDesc");
    assert_eq!(
        descriptor.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(descriptor.ownership, MirOwnership::Copy);
    assert_eq!(descriptor.glue.move_out, MirGlueKind::Noop);

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &crate::core::NodeId("function:negate".into()),
            &[crate::core::mir::reference::MirRuntimeValue::FloatBits(
                2.5f64.to_bits(),
            )],
        )
        .expect("reference f64 negate execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits((-2.5f64).to_bits())
    );
    assert!(program
        .type_catalog()
        .validate_copy_float_unary(
            &result_ty,
            &operand_ty,
            crate::core::ir::ResolvedUnaryOp::Not,
        )
        .is_err());
}

#[test]
fn lowers_f64_add_with_finite_only_copy_float_contract() {
    let source = include_str!("../../../tests/fixtures/mir_native_f64_add.mimi");
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("f64 add must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:add".into()))
        .expect("add MIR function");
    let (result_ty, left_ty, right_ty) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Binary {
                result,
                op: crate::core::ir::ResolvedBinaryOp::Add,
                left,
                right,
            } => Some((
                function.values.get(result)?.ty.clone(),
                function.values.get(left)?.ty.clone(),
                function.values.get(right)?.ty.clone(),
            )),
            _ => None,
        })
        .expect("canonical f64 add instruction");
    program
        .type_catalog()
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Add,
        )
        .expect("f64 add TypeDesc contract");
    let descriptor = program
        .type_catalog()
        .get(&result_ty)
        .expect("f64 add TypeDesc");
    assert_eq!(
        descriptor.abi,
        crate::core::mir::types::MirAbiClass::Float { bits: 64 }
    );
    assert_eq!(descriptor.ownership, MirOwnership::Copy);
    assert_eq!(descriptor.glue.move_out, MirGlueKind::Noop);

    let owner = crate::core::NodeId("function:add".into());
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program);
    let value = reference
        .execute(
            &owner,
            &[
                crate::core::mir::reference::MirRuntimeValue::FloatBits(1.25f64.to_bits()),
                crate::core::mir::reference::MirRuntimeValue::FloatBits(2.75f64.to_bits()),
            ],
        )
        .expect("reference f64 add execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits(4.0f64.to_bits())
    );
    for (left, right, label) in [
        (f64::NAN, 1.0, "NaN operand"),
        (f64::INFINITY, 1.0, "Inf operand"),
        (f64::MAX, f64::MAX, "non-finite result"),
    ] {
        let error = reference
            .execute(
                &owner,
                &[
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(left.to_bits()),
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(right.to_bits()),
                ],
            )
            .expect_err(label);
        assert!(
            error
                .message
                .contains(crate::core::mir::types::MIR_FLOAT_NOT_FINITE_TRAP_CODE),
            "{label} must retain E0813 classification: {error}"
        );
    }
    assert!(program
        .type_catalog()
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Multiply,
        )
        .is_err());
}

#[test]
fn lowers_f64_subtract_with_finite_only_copy_float_contract() {
    let source = include_str!("../../../tests/fixtures/mir_native_f64_subtract.mimi");
    let program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("f64 subtract must lower to canonical MIR");
    let function = program
        .functions()
        .get(&crate::core::NodeId("function:subtract".into()))
        .expect("subtract MIR function");
    let (result_ty, left_ty, right_ty) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Binary {
                result,
                op: crate::core::ir::ResolvedBinaryOp::Subtract,
                left,
                right,
            } => Some((
                function.values.get(result)?.ty.clone(),
                function.values.get(left)?.ty.clone(),
                function.values.get(right)?.ty.clone(),
            )),
            _ => None,
        })
        .expect("canonical f64 subtract instruction");
    program
        .type_catalog()
        .validate_copy_float_binary(
            &result_ty,
            &left_ty,
            &right_ty,
            crate::core::ir::ResolvedBinaryOp::Subtract,
        )
        .expect("f64 subtract TypeDesc contract");
    let owner = crate::core::NodeId("function:subtract".into());
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&program);
    let value = reference
        .execute(
            &owner,
            &[
                crate::core::mir::reference::MirRuntimeValue::FloatBits(5.5f64.to_bits()),
                crate::core::mir::reference::MirRuntimeValue::FloatBits(2.25f64.to_bits()),
            ],
        )
        .expect("reference f64 subtract execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::FloatBits(3.25f64.to_bits())
    );
    for (left, right, label) in [
        (f64::NAN, 1.0, "NaN operand"),
        (f64::INFINITY, 1.0, "Inf operand"),
        (f64::MAX, -f64::MAX, "non-finite result"),
    ] {
        let error = reference
            .execute(
                &owner,
                &[
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(left.to_bits()),
                    crate::core::mir::reference::MirRuntimeValue::FloatBits(right.to_bits()),
                ],
            )
            .expect_err(label);
        assert!(
            error
                .message
                .contains(crate::core::mir::types::MIR_FLOAT_NOT_FINITE_TRAP_CODE),
            "{label} must retain E0813 classification: {error}"
        );
    }
}

#[test]
fn copy_option_i32_typedesc_contract_rejects_other_payloads() {
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
        include_str!("../../../tests/fixtures/mir_native_option_i32_unwrap.mimi"),
    ))
    .expect("Option<i32> MIR");
    let option_i32 = program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option).then_some(id.clone())
        })
        .expect("Option<i32> TypeDesc");
    assert!(program
        .type_catalog()
        .validate_copy_option_i32_variant(&option_i32)
        .is_ok());

    let i64_program = crate::core::mir::reference::MirProgram::from_checked_program(
        &checked_program("func main() -> i64 { let value: Option<i64> = Some(7); drop(value); 7 }"),
    )
    .expect("Option<i64> construction MIR");
    let option_i64 = i64_program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option).then_some(id.clone())
        })
        .expect("Option<i64> TypeDesc");
    let error = i64_program
        .type_catalog()
        .validate_copy_option_i32_variant(&option_i64)
        .expect_err("Option<i64> must stay outside the i32 island");
    assert!(error.contains("expected I32"), "{error}");

    let bool_program =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            "func main() -> i32 { let value: Option<bool> = Some(true); drop(value); 0 }",
        ))
        .expect("Option<bool> construction MIR");
    let option_bool = bool_program
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.kind == crate::core::mir::types::MirTypeKind::Option).then_some(id.clone())
        })
        .expect("Option<bool> TypeDesc");
    assert!(bool_program
        .type_catalog()
        .validate_copy_option_variant(&option_bool, PrimitiveType::Bool)
        .is_ok());
    let error = bool_program
        .type_catalog()
        .validate_copy_option_i32_variant(&option_bool)
        .expect_err("Option<bool> must not satisfy the i32 island");
    assert!(error.contains("expected I32"), "{error}");
}

#[test]
fn option_string_unwrap_none_preserves_canonical_trap_class() {
    let source = r#"
        func main() -> i32 {
            let value: Option<string> = None
            let text = value.unwrap()
            drop(text)
            41
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("Option<string>.unwrap None must still be canonical MIR");
    let error = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect_err("unwrap on None must trap");
    assert!(
        error.to_string().contains("E0800"),
        "unexpected trap: {error}"
    );
}

#[test]
fn result_i64_unwrap_remains_fail_closed_outside_copy_result_i32_island() {
    let source = r#"
        func main() -> i64 {
            let value: Result<i64, i32> = Ok(41)
            value.unwrap()
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked).expect_err(
        "Result<i64, i32>::unwrap must remain outside the Copy Result<i32, i32> island",
    );
    let crate::core::mir::reference::MirProgramBuildError::Lowering(errors) = error else {
        panic!("Result::unwrap must fail during MIR lowering");
    };
    assert!(
        errors.iter().any(|error| error.message.contains(
            "Option/Result unwrap shape is outside the canonical variant projection contract"
        )),
        "unexpected fail-closed diagnostics: {errors:?}"
    );
}

#[test]
fn option_f64_unwrap_or_remains_fail_closed_outside_copy_projection_island() {
    let source = r#"
        func main() -> f64 {
            let value: Option<f64> = Some(41.0)
            value.unwrap_or(0.0)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("Copy Option<f64>::unwrap_or must remain outside the copy projection island");
    let crate::core::mir::reference::MirProgramBuildError::Lowering(errors) = error else {
        panic!("Copy Option<f64>::unwrap_or must fail during MIR lowering");
    };
    assert!(
        errors.iter().any(|error| error.message.contains(
            "Option/Result unwrap shape is outside the canonical variant projection contract"
        )),
        "unexpected fail-closed diagnostics: {errors:?}"
    );
}

#[test]
fn materializes_generic_scalar_list_len_as_a_canonical_facade() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_len.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List.len must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List.len instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListFacade {
            operation: crate::core::mir::MirListOperation::Len
        }
    ));
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List.len target");
    let list_operations = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Len,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(list_operations.len(), 1);
    assert!(matches!(
        program
            .type_catalog()
            .get(&list_operations[0].element_ty)
            .map(|descriptor| &descriptor.kind),
        Some(crate::core::mir::types::MirTypeKind::Primitive(
            PrimitiveType::I32
        ))
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List.len facade execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_generic_scalar_list_reverse_as_a_canonical_facade() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_reverse.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List.reverse must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List.reverse instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListFacade {
            operation: crate::core::mir::MirListOperation::Reverse
        }
    ));
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List.reverse target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].list_ty, receipts[0].result_ty,
        "List.reverse must preserve one canonical List TypeDesc"
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List.reverse facade execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_generic_scalar_list_concat_as_a_two_input_move_facade() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_concat.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List.concat must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List.concat instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListFacade {
            operation: crate::core::mir::MirListOperation::Concat
        }
    ));
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List.concat target");
    let mut move_results = Vec::new();
    let mut concat_receipt = None;
    for instruction in target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
    {
        match &instruction.kind {
            MirInstructionKind::Move { result, .. } => move_results.push(result.clone()),
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Concat,
                list,
                argument: Some(argument),
                list_operation_contract: Some(receipt),
                ..
            } => concat_receipt = Some((list.clone(), argument.clone(), receipt.clone())),
            _ => {}
        }
    }
    assert_eq!(
        move_results.len(),
        2,
        "Concat must move both callable inputs"
    );
    let (list, argument, receipt) = concat_receipt.expect("canonical concat receipt");
    assert!(move_results.contains(&list));
    assert!(move_results.contains(&argument));
    assert_eq!(receipt.argument_ty, Some(receipt.list_ty.clone()));
    assert_eq!(
        receipt.operation,
        crate::core::mir::MirListOperation::Concat
    );
    assert_eq!(receipt.result_ty, receipt.list_ty);
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List.concat facade execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(5));
}

#[test]
fn materializes_generic_scalar_list_construct_with_a_type_desc_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_construct.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List construction must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List construction instance");
    let MirGenericInstanceContract::ScalarListConstruct { contract } = &instance.contract else {
        panic!("expected a ScalarListConstruct instance contract");
    };
    assert_eq!(contract.element_count, 1);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List construction target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::ConstructList {
                list_construct_contract: Some(receipt),
                elements,
                ..
            } => Some((receipt, elements.len())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].0, contract);
    assert_eq!(receipts[0].1, 1);
    assert!(target.canonical_text().contains("list_construct_contract"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List construction execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(1));
}

#[test]
fn materializes_generic_nested_list_construct_with_recursive_list_glue() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_nested_owned.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("one-level nested List construction must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .find(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarListConstruct { .. }
            ) && instance.arguments.len() == 1
        })
        .expect("nested List construction instance");
    let argument = &instance.arguments[0];
    let argument_desc = program
        .type_catalog()
        .get(argument)
        .expect("nested List argument TypeDesc");
    assert!(matches!(
        argument_desc.layout,
        crate::core::mir::types::MirLayout::List { .. }
    ));
    program
        .type_catalog()
        .validate_nested_list_payload(argument)
        .expect("nested List child glue must be fully materialized");
    let nested_list_ty = program
        .type_catalog()
        .iter()
        .find_map(|(ty, descriptor)| {
            matches!(descriptor.layout, crate::core::mir::types::MirLayout::List { ref element }
                if program
                    .type_catalog()
                    .get(element)
                    .is_some_and(|child| matches!(child.layout, crate::core::mir::types::MirLayout::List { .. })))
            .then(|| ty.clone())
        })
        .expect("nested List result TypeDesc");
    assert!(
        program
            .type_catalog()
            .validate_move_owned_payload(&nested_list_ty)
            .is_err(),
        "nested List must not widen managed variant payloads"
    );
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized nested List construction target");
    assert!(target.canonical_text().contains("list_construct_contract"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List construction execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_list_construct_beyond_one_nested_level_at_the_mir_gate() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_nested_deep_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("deep nested List construction must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("one-level nested List contract")
            || message.contains("outside the canonical Copy scalar"),
        "unexpected deep nested List rejection: {message}"
    );
}

#[test]
fn rejects_generic_list_construct_for_non_copy_elements_at_the_mir_gate() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_construct_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List<string> generic construction must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside the canonical Copy scalar contract")
            || message.contains("generic List facade candidate did not materialize"),
        "unexpected non-Copy generic List construction rejection: {message}"
    );
}

#[test]
fn materializes_generic_scalar_list_projection_with_a_constant_zero_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_generic_list_projection.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List projection instance");
    let MirGenericInstanceContract::ScalarListProjection {
        contract,
        index_value,
    } = &instance.contract
    else {
        panic!("expected a ScalarListProjection instance contract");
    };
    assert_eq!(*index_value, 0);
    let target = program
        .functions()
        .get(&instance.function)
        .expect("materialized List projection target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                projection: MirProjection::Index(_),
                list_index_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0], contract);
    assert_eq!(contract.element_ty, contract.result_ty);
    assert!(target
        .canonical_text()
        .contains("list_index=MirListIndexProjectionContract"));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn materializes_nested_list_index_with_deep_clone_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_index.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("nested List index must lower to canonical MIR");
    let target = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("nested List index target");
    let receipts = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                projection: MirProjection::Index(_),
                list_index_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().any(|receipt| {
        receipt.mode == crate::core::mir::types::MirListIndexProjectionMode::CloneNestedList
    }));
    assert!(receipts.iter().any(|receipt| {
        receipt.mode == crate::core::mir::types::MirListIndexProjectionMode::CopyScalar
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List index execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_nested_list_reverse_with_recursive_clone_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_reverse.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("nested List.reverse must lower to canonical MIR");
    let target = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("nested List.reverse target");
    let receipt = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("nested List.reverse receipt");
    assert_eq!(
        receipt.mode,
        crate::core::mir::types::MirListOperationMode::Nested
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List.reverse execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_nested_list_concat_with_child_move_receipt() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_concat.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("nested List.concat must lower to canonical MIR");
    let target = program
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("nested List.concat target");
    let receipt = target
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: crate::core::mir::MirListOperation::Concat,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt),
            _ => None,
        })
        .expect("nested List.concat receipt");
    assert_eq!(
        receipt.mode,
        crate::core::mir::types::MirListOperationMode::Nested
    );
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference nested List.concat execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(3));
}

#[test]
fn materializes_generic_scalar_list_projection_with_a_constant_one_receipt() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_projection_index_one.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("generic List index-one projection must lower to canonical MIR");
    let instance = program
        .instances()
        .values()
        .next()
        .expect("generic List index-one projection instance");
    assert!(matches!(
        instance.contract,
        MirGenericInstanceContract::ScalarListProjection { index_value: 1, .. }
    ));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference List index-one projection execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));
}

#[test]
fn rejects_generic_list_projection_for_managed_elements_at_the_mir_gate() {
    let source =
        include_str!("../../../tests/fixtures/mir_native_generic_list_projection_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("managed generic List projection must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside scalar contract")
            || message.contains("generic List facade candidate did not materialize"),
        "unexpected managed generic List projection rejection: {message}"
    );
}

#[test]
fn rejects_generic_list_projection_for_nonzero_constant_index_at_the_mir_gate() {
    let source = r#"
        func first<T>(values: List<T>) -> T {
            values[2]
        }

        func main() -> i32 {
            let values = [41, 42]
            let picked = first(values)
            drop(values)
            picked
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("nonzero generic List projection must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("constant index literal 0 or 1")
            || message.contains("literal-zero")
            || message.contains("constant zero"),
        "unexpected nonzero generic List projection rejection: {message}"
    );
}

#[test]
fn rejects_generic_list_concat_for_non_copy_elements_at_the_mir_gate() {
    let source = r#"
        func list_concat<T>(left: List<T>, right: List<T>) -> List<T> {
            left.concat(right)
        }

        func main() -> i32 {
            let left: List<string> = ["a"]
            let right: List<string> = ["b"]
            let joined = list_concat(left, right)
            let count = len(joined)
            drop(left)
            drop(right)
            drop(joined)
            count
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List<string> generic concat must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside the canonical Copy scalar contract"),
        "unexpected non-Copy generic List.concat rejection: {message}"
    );
}

#[test]
fn rejects_generic_list_concat_body_without_a_canonical_operation() {
    let source = r#"
        func bad<T>(left: List<T>, right: List<T>) -> List<T> { left }

        func main() -> i32 {
            let left: List<i32> = [1]
            let right: List<i32> = [2]
            let joined = bad(left, right)
            let count = len(joined)
            drop(left)
            drop(right)
            drop(joined)
            count
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic List.concat body without ListOp must fail closed");
    let message = format!("{error:?}");
    assert!(message.contains("generic List facade must lower to exactly one canonical ListOp"));
}

#[test]
fn rejects_generic_list_facade_with_multiple_operations_before_backends() {
    let source = r#"
        func bad<T>(values: List<T>) -> i32 {
            let first = len(values)
            let second = len(values)
            first + second
        }

        func main() -> i32 {
            let values: List<i32> = [1, 2]
            bad(values)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("unsupported generic List body must fail closed");
    let message = format!("{error:?}");
    assert!(message.contains("generic List facade must lower to exactly one canonical ListOp"));
}

#[test]
fn rejects_generic_list_reverse_body_without_a_canonical_operation() {
    let source = r#"
        func bad<T>(values: List<T>) -> List<T> { values }

        func main() -> i32 {
            let values: List<i32> = [1, 2]
            let result = bad(values)
            drop(result)
            0
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("generic List body without reverse operation must fail closed");
    let message = format!("{error:?}");
    assert!(message.contains("generic List facade must lower to exactly one canonical ListOp"));
}

#[test]
fn rejects_generic_list_len_for_non_copy_elements_at_the_mir_gate() {
    let source = r#"
        func list_len<T>(values: List<T>) -> i32 { len(values) }

        func main() -> i32 {
            let values: List<string> = ["not-copy"]
            list_len(values)
        }
    "#;
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy generic List.len must fail closed");
    let message = format!("{error:?}");
    assert!(
        message.contains("outside scalar contract"),
        "unexpected non-Copy generic List.len rejection: {message}"
    );
}

fn fixture() -> MirFunction {
    let mut types = ResolvedTypeTable::new();
    let i64_ty = type_id(&mut types, ResolvedType::Primitive(PrimitiveType::I64));
    let entry = MirBlockId::new("bb.entry").unwrap();
    let exit = MirBlockId::new("bb.exit").unwrap();
    let arg = MirValueId::new("v.arg").unwrap();
    let one = MirValueId::new("v.one").unwrap();
    let sum = MirValueId::new("v.sum").unwrap();
    let result = MirValueId::new("v.result").unwrap();
    let mut values = BTreeMap::new();
    for id in [&arg, &one, &sum, &result] {
        values.insert(
            id.clone(),
            MirValue {
                id: id.clone(),
                ty: i64_ty.clone(),
            },
        );
    }
    MirFunction {
        owner: NodeId("func:test".into()),
        parameters: vec![arg.clone()],
        parameter_permissions: None,
        result: i64_ty.clone(),
        entry: entry.clone(),
        values,
        blocks: BTreeMap::from([
            (
                entry.clone(),
                MirBlock {
                    id: entry,
                    parameters: vec![],
                    instructions: vec![
                        MirInstruction {
                            id: MirInstructionId::new("i.const").unwrap(),
                            kind: MirInstructionKind::Const {
                                result: one.clone(),
                                literal: ResolvedLiteral::Int(1),
                            },
                        },
                        MirInstruction {
                            id: MirInstructionId::new("i.add").unwrap(),
                            kind: MirInstructionKind::Binary {
                                result: sum.clone(),
                                op: ResolvedBinaryOp::Add,
                                left: arg,
                                right: one,
                            },
                        },
                    ],
                    terminator: MirTerminator::Goto {
                        edge: MirEdgeId::new("e.return").unwrap(),
                        target: exit.clone(),
                        arguments: vec![sum],
                    },
                },
            ),
            (
                exit.clone(),
                MirBlock {
                    id: exit,
                    parameters: vec![MirBlockParameter {
                        value: result.clone(),
                    }],
                    instructions: vec![],
                    terminator: MirTerminator::Return {
                        value: Some(result),
                    },
                },
            ),
        ]),
        contracts: Vec::new(),
        ownership: MirOwnershipSummary::default(),
    }
}

#[test]
fn valid_function_passes_structural_validation() {
    let function = fixture();
    assert!(function.validate().is_ok(), "{:?}", function.validate());
}

#[test]
fn direct_variant_projection_reference_checks_active_tag_and_returns_payload() {
    let fixture = crate::core::mir::test_support::direct_variant_projection_fixture();
    let nominal =
        crate::core::ir::NominalTypeId::new("builtin:type:Option").expect("Option nominal");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&fixture.program);
    let value = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal: nominal.clone(),
                variant: fixture.some.clone(),
                payload: vec![crate::core::mir::reference::MirRuntimeValue::Int(41)],
            }],
        )
        .expect("direct Some projection");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(41));

    let error = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal,
                variant: fixture.none.clone(),
                payload: Vec::new(),
            }],
        )
        .expect_err("wrong active variant must trap");
    assert!(error.message.contains("E0800"), "{error}");
}

#[test]
fn direct_variant_projection_receipt_is_checked_before_consumers() {
    let fixture = crate::core::mir::test_support::direct_variant_projection_fixture();
    assert_eq!(fixture.receipt.source_ty, fixture.source_ty);
    assert_eq!(fixture.receipt.result_ty, fixture.result_ty);
    assert_eq!(fixture.receipt.projection.variant, fixture.some);
    assert_eq!(fixture.receipt.projection.field, fixture.field);
    assert_eq!(fixture.receipt.discriminant, 1);
    assert_eq!(
        fixture.receipt.trap_code,
        crate::core::mir::types::MIR_VARIANT_PROJECTION_TRAP_CODE
    );
    assert!(fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .canonical_text()
        .contains("variant_project"));

    let mut function = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::VariantProject { .. }))
        .expect("variant projection instruction");
    let MirInstructionKind::VariantProject {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    receipt.discriminant = 0;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), function)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged active-tag receipt must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant projection trap receipt disagrees with TypeDesc")
    }));
}

#[test]
fn consuming_variant_projection_moves_owned_payload_and_traps_on_wrong_tag() {
    let fixture = crate::core::mir::test_support::direct_variant_move_projection_fixture();
    assert_eq!(fixture.receipt.source_ty, fixture.source_ty);
    assert_eq!(fixture.receipt.result_ty, fixture.result_ty);
    let nominal =
        crate::core::ir::NominalTypeId::new("builtin:type:Option").expect("Option nominal");
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&fixture.program);
    let value = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal: nominal.clone(),
                variant: fixture.some.clone(),
                payload: vec![crate::core::mir::reference::MirRuntimeValue::String(
                    "owned".into(),
                )],
            }],
        )
        .expect("consuming Some projection");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("owned".into())
    );

    let error = reference
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Variant {
                nominal,
                variant: fixture.none.clone(),
                payload: Vec::new(),
            }],
        )
        .expect_err("wrong active variant must trap before payload extraction");
    assert!(error.message.contains("E0800"), "{error}");
}

#[test]
fn consuming_variant_projection_receipt_is_move_owned_and_fail_closed() {
    let fixture = crate::core::mir::test_support::direct_variant_move_projection_fixture();
    assert_eq!(fixture.receipt.projection.ownership, MirOwnership::Move);
    assert_eq!(
        fixture.receipt.projection.move_out_glue,
        crate::core::mir::types::MirGlueKind::OwnedString
    );
    assert_eq!(fixture.receipt.projection.variant, fixture.some);
    assert_eq!(fixture.receipt.projection.field, fixture.field);
    assert_eq!(fixture.receipt.discriminant, 1);

    let mut function = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::VariantProjectMove { .. }
            )
        })
        .expect("variant move projection instruction");
    let MirInstructionKind::VariantProjectMove {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    receipt.projection.ownership = MirOwnership::Copy;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), function)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged Copy move receipt must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant move projection trap receipt disagrees with TypeDesc")
    }));

    let mut double_use = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    double_use
        .blocks
        .values_mut()
        .next()
        .expect("entry block")
        .instructions
        .push(MirInstruction {
            id: MirInstructionId::new("i.after-variant-project-move").expect("instruction id"),
            kind: MirInstructionKind::Drop {
                value: MirValueId::new("v.input").expect("input value id"),
            },
        });
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), double_use)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("consumed variant source must not be used again");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("use after consuming non-Copy value")));
}

#[test]
fn record_move_drop_projection_moves_selected_string_and_drops_residual() {
    let fixture = crate::core::mir::test_support::direct_record_move_drop_fixture();
    assert_eq!(fixture.receipt.source_ty, fixture.source_ty);
    assert_eq!(fixture.receipt.result_ty, fixture.result_ty);
    assert_eq!(fixture.receipt.projection.field, fixture.selected_field);
    assert_eq!(fixture.receipt.projection.field_index, 0);
    assert_eq!(fixture.receipt.residual.len(), 1);
    assert_eq!(fixture.receipt.residual[0].name, "right");
    assert_eq!(
        fixture.receipt.residual[0].glue,
        crate::core::mir::types::MirGlueKind::OwnedString
    );

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&fixture.program)
        .execute(
            &fixture.function,
            &[crate::core::mir::reference::MirRuntimeValue::Record {
                nominal: fixture.receipt.projection.nominal.clone(),
                fields: vec![
                    crate::core::mir::reference::MirRuntimeValue::String("left".into()),
                    crate::core::mir::reference::MirRuntimeValue::String("right".into()),
                ],
            }],
        )
        .expect("record move/drop projection");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("left".into())
    );
}

#[test]
fn record_move_drop_projection_receipt_and_source_use_fail_closed() {
    let fixture = crate::core::mir::test_support::direct_record_move_drop_fixture();
    let mut function = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::MoveProjectDrop { .. }))
        .expect("record move/drop instruction");
    let MirInstructionKind::MoveProjectDrop {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!();
    };
    receipt.residual.clear();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), function)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("incomplete residual receipt must fail before consumers");
    assert!(errors.iter().any(|error| error
        .message
        .contains("record move/drop projection receipt disagrees")));

    let mut double_use = fixture
        .program
        .functions()
        .get(&fixture.function)
        .expect("project function")
        .clone();
    double_use
        .blocks
        .values_mut()
        .next()
        .expect("entry block")
        .instructions
        .push(MirInstruction {
            id: MirInstructionId::new("i.after-record-move-drop").expect("instruction id"),
            kind: MirInstructionKind::Drop {
                value: MirValueId::new("v.input").expect("input value id"),
            },
        });
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(fixture.function.clone(), double_use)]),
        fixture.program.type_catalog().clone(),
    )
    .expect_err("consumed record source must not be used again");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("use after consuming non-Copy value")));
}

#[test]
fn canonical_text_is_deterministic_and_contains_contract_shapes() {
    let function = fixture();
    let first = function.canonical_text();
    let second = function.canonical_text();
    assert_eq!(first, second);
    assert!(first.contains("mir.function func:test"));
    assert!(first.contains("binary v.sum = Add v.arg, v.one"));
    assert!(first.contains("goto e.return bb.exit(v.sum)"));
}

#[test]
fn missing_target_and_arity_are_rejected_before_backend() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Goto {
        edge: MirEdgeId::new("e.bad").unwrap(),
        target: MirBlockId::new("bb.missing").unwrap(),
        arguments: vec![],
    };
    let errors = function
        .validate()
        .expect_err("invalid target must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("missing block")));
}

#[test]
fn malformed_trap_identity_is_rejected_before_backend() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Trap { code: "\n".into() };
    let errors = function
        .validate()
        .expect_err("malformed trap must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("trap code is empty")));
}

#[test]
fn oversized_or_controlled_trap_identity_is_rejected_before_backend() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Trap {
        code: format!(
            "bad{}",
            "x".repeat(crate::core::mir::types::MIR_TRAP_CODE_MAX_LEN)
        ),
    };
    let errors = function
        .validate()
        .expect_err("oversized trap must fail closed");
    assert!(errors.iter().any(|error| error.message.contains("exceeds")));

    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.terminator = MirTerminator::Trap {
        code: "trap\u{0007}".into(),
    };
    let errors = function
        .validate()
        .expect_err("control character in trap must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("control character")));
}

#[test]
fn duplicate_definition_is_rejected() {
    let mut function = fixture();
    let entry = function
        .blocks
        .get_mut(&MirBlockId::new("bb.entry").unwrap())
        .unwrap();
    entry.instructions.push(MirInstruction {
        id: MirInstructionId::new("i.duplicate").unwrap(),
        kind: MirInstructionKind::Const {
            result: MirValueId::new("v.one").unwrap(),
            literal: ResolvedLiteral::Int(2),
        },
    });
    let errors = function
        .validate()
        .expect_err("duplicate value definition must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("defined more than once")));
}

#[test]
fn value_catalog_identity_is_checked() {
    let mut function = fixture();
    let key = MirValueId::new("v.one").unwrap();
    let value = function.values.get_mut(&key).unwrap();
    value.id = MirValueId::new("v.other").unwrap();
    let errors = function
        .validate()
        .expect_err("catalog identity mismatch must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("catalog key disagrees")));
}

#[test]
fn use_before_definition_is_rejected_even_when_catalog_is_complete() {
    let mut function = fixture();
    let entry_id = MirBlockId::new("bb.entry").unwrap();
    let entry = function.blocks.get_mut(&entry_id).unwrap();
    entry.instructions.swap(0, 1);
    let errors = function
        .validate()
        .expect_err("an instruction cannot read a later definition");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("used before its definition")));
}

#[test]
fn ownership_events_are_part_of_the_canonical_function_contract() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Move,
        resource: "resource:token".into(),
        value: None,
        source: Some("token".into()),
        target: Some("consumed".into()),
        point: NodeId("node:consume".into()),
    });
    assert!(
        function.validate().is_ok(),
        "ownership event should validate"
    );
    let text = function.canonical_text();
    assert!(text.contains("ownership[0] move resource=resource:token"));
    assert!(text.contains("source=token target=consumed point=node:consume"));
}

#[test]
fn ownership_event_without_source_is_rejected_for_consuming_kinds() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Drop,
        resource: "resource:token".into(),
        value: None,
        source: None,
        target: None,
        point: NodeId("node:drop".into()),
    });
    let errors = function
        .validate()
        .expect_err("drop without a source must fail closed");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("drop event has no source")));
}

#[test]
fn ownership_event_value_must_be_declared_by_the_function() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Read,
        resource: "resource:token".into(),
        value: Some(MirValueId::new("local:missing").unwrap()),
        source: Some("token".into()),
        target: None,
        point: NodeId("node:read".into()),
    });
    let errors = function
        .validate()
        .expect_err("ownership values must be backed by the value catalog");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("event value 'local:missing' is absent")
    }));
}

#[test]
fn ownership_return_event_must_match_a_mir_return_value() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Return,
        resource: "resource:result".into(),
        value: Some(MirValueId::new("v.result").unwrap()),
        source: Some("result".into()),
        target: None,
        point: NodeId("node:return".into()),
    });
    assert!(validate_ownership_event_receipts(&function).is_empty());
}

#[test]
fn ownership_move_event_without_a_mir_transfer_is_rejected() {
    let mut function = fixture();
    function.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Move,
        resource: "resource:arg".into(),
        value: Some(MirValueId::new("v.arg").unwrap()),
        source: Some("arg".into()),
        target: Some("consumed".into()),
        point: NodeId("node:move".into()),
    });
    let errors = validate_ownership_event_receipts(&function);
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("move event value 'v.arg' has no matching canonical MIR transfer boundary")
    }));
}

#[test]
fn consuming_variant_payload_uses_the_canonical_drop_plan() {
    let source = "func main() -> Option<string> { Some(\"owned\") }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned variant glue must be materialized");
    let main = canonical
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    let instruction = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => Some((result, nominal, variant, fields)),
            _ => None,
        })
        .expect("move variant construction");
    let result_ty = &main.values.get(instruction.0).expect("result value").ty;
    let field_ids = instruction
        .3
        .iter()
        .map(|(field, _)| field.clone())
        .collect::<Vec<_>>();
    let field_types = instruction
        .3
        .iter()
        .map(|(_, value)| main.values.get(value).expect("payload value").ty.clone())
        .collect::<Vec<_>>();
    canonical
        .type_catalog()
        .validate_variant_move_construct(
            result_ty,
            instruction.1,
            instruction.2,
            &field_ids,
            &field_types,
        )
        .expect("variant payload/drop plan must agree");
}

#[test]
fn consuming_variant_payload_identity_drift_is_rejected_before_consumers() {
    let source = "func main() -> Option<string> { Some(\"owned\") }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned variant glue must be materialized");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let instruction = forged
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
            )
        })
        .expect("move variant construction");
    let crate::core::mir::MirInstructionKind::ConstructVariantMove { fields, .. } =
        &mut instruction.kind
    else {
        unreachable!();
    };
    fields[0].0 = crate::core::NodeId("builtin:variant:Option::Some/payload:drift".into());
    let errors = validate_variant_move_payloads(&forged, canonical.type_catalog());
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("canonical variant move payload contract failed")
    }));
}

#[test]
fn flow_move_receipt_reaches_the_consuming_transition_argument() {
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            include_str!("../../../tests/fixtures/mir_native_flow_transition.mimi"),
        ))
        .expect("silent-local Flow transition must remain admitted");
    let owner = crate::core::NodeId("function:main".into());
    let main = canonical.functions().get(&owner).expect("main MIR");
    assert!(validate_transfer_event_boundaries(main, canonical.transitions()).is_empty());
    let flow = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::FlowTransition { .. }
            )
        })
        .expect("FlowTransition instruction");
    let crate::core::mir::MirInstructionKind::FlowTransition { arguments, .. } = &flow.kind else {
        unreachable!();
    };
    assert!(main.values.contains_key(&arguments[0]));
}

#[test]
fn flow_move_receipt_point_drift_is_rejected_before_consumers() {
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            include_str!("../../../tests/fixtures/mir_native_flow_transition.mimi"),
        ))
        .expect("silent-local Flow transition must remain admitted");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let event = forged
        .ownership
        .events
        .iter_mut()
        .find(|event| event.kind == MirOwnershipEventKind::Move)
        .expect("Flow move receipt");
    event.point = crate::core::NodeId("function:main/node:forged-flow-point".into());
    let errors = validate_transfer_event_boundaries(&forged, canonical.transitions());
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("has no canonical call/flow boundary at point")
    }));
}

#[test]
fn flow_session_effect_receipt_cannot_be_attached_to_silent_local_transition() {
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(
            include_str!("../../../tests/fixtures/mir_native_flow_transition.mimi"),
        ))
        .expect("silent-local Flow transition must remain admitted");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let source_event = forged
        .ownership
        .events
        .iter()
        .find(|event| event.kind == MirOwnershipEventKind::Move)
        .cloned()
        .expect("Flow move receipt");
    forged.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::TransferSession,
        resource: source_event.resource,
        value: source_event.value,
        source: source_event.source,
        target: source_event.target,
        point: source_event.point,
    });
    let errors = validate_transfer_event_boundaries(&forged, canonical.transitions());
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("disagrees with SilentLocal FlowTransition effect")
    }));
}

#[test]
fn concrete_owned_call_argument_uses_a_canonical_move_boundary() {
    let source = r#"
func take(value: Option<string>) -> i32 {
    drop(value)
    41
}
func main() -> i32 {
    let value = Some("owned")
    take(value)
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("concrete owned call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let main = canonical.functions().get(&owner).expect("main MIR");
    let call_argument = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => arguments.first().cloned(),
            _ => None,
        })
        .expect("direct call argument");
    assert!(main.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &call_argument
            )
        })
    }));
    crate::interp::bytecode::compile_mir_program(&canonical)
        .expect("bytecode adapter must consume the admitted call receipt");
}

#[test]
fn borrowed_function_argument_keeps_the_source_outside_the_move_boundary() {
    let source = r#"
func inspect(data: view List<i32>) -> i32 {
    len(data)
}
func main() -> i32 {
    let data = [1, 2]
    let n = inspect(data)
    drop(data)
    n
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("borrowed direct call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let main = canonical.functions().get(&owner).expect("main MIR");
    let (call_argument, call_point) = main
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => Some((
                arguments.first().cloned().expect("call argument"),
                instruction.id.clone(),
            )),
            _ => None,
        })
        .expect("borrowed direct call");
    let has_move = main.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &call_argument
            )
        })
    });
    assert!(
        !has_move,
        "view parameter must not consume the source local"
    );
    assert!(call_point.as_str().starts_with("inst:call:"));
}

#[test]
fn owned_call_direction_receipt_rejects_a_forged_clone() {
    let source = r#"
func take(value: Option<string>) -> i32 {
    drop(value)
    41
}
func main() -> i32 {
    let value = Some("owned")
    take(value)
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("concrete owned call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let call_argument = forged
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => arguments.first().cloned(),
            _ => None,
        })
        .expect("direct call argument");
    for block in forged.blocks.values_mut() {
        for instruction in &mut block.instructions {
            if let MirInstructionKind::Move { result, source } = &instruction.kind {
                if result == &call_argument {
                    instruction.kind = MirInstructionKind::Clone {
                        result: result.clone(),
                        source: source.clone(),
                    };
                }
            }
        }
    }
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            BTreeMap::from([(owner, forged)]),
            canonical.type_catalog().clone(),
            BTreeMap::new(),
            canonical.transitions().clone(),
        )
        .expect_err("owned call Clone must fail the direction receipt");
    assert!(errors
        .iter()
        .any(|error| { error.message.contains("lacks a canonical Move producer") }));
}

#[test]
fn call_move_receipt_rejects_argument_identity_drift() {
    let source = r#"
func take(value: Option<string>) -> i32 {
    drop(value)
    41
}
func main() -> i32 {
    let value = Some("owned")
    take(value)
}
"#;
    let canonical =
        crate::core::mir::reference::MirProgram::from_checked_program(&checked_program(source))
            .expect("concrete owned call must lower to canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let (call_instruction, original_argument) = forged
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Call { arguments, .. } => Some((
                instruction.id.clone(),
                arguments.first().cloned().expect("call argument"),
            )),
            _ => None,
        })
        .expect("direct call");
    let call_point = call_instruction
        .as_str()
        .split_once(':')
        .and_then(|(_, rest)| rest.split_once(':'))
        .map(|(_, point)| crate::core::NodeId(point.to_owned()))
        .expect("call instruction point");
    let replacement = forged
        .values
        .keys()
        .find(|value| **value != original_argument)
        .cloned()
        .expect("replacement value");
    for block in forged.blocks.values_mut() {
        for instruction in &mut block.instructions {
            if instruction.id == call_instruction {
                let MirInstructionKind::Call { arguments, .. } = &mut instruction.kind else {
                    unreachable!("call instruction identity changed");
                };
                arguments[0] = replacement.clone();
            }
        }
    }
    let source_value = forged
        .values
        .keys()
        .find(|value| {
            value
                .as_str()
                .starts_with("local:function:main/node:pattern.variable:value")
        })
        .cloned()
        .expect("source local identity");
    forged.ownership.events.push(MirOwnershipEvent {
        kind: MirOwnershipEventKind::Move,
        resource: "function:main/node:pattern.variable:value/local".into(),
        value: Some(source_value),
        source: Some("value".into()),
        target: None,
        point: call_point,
    });
    let errors =
        crate::core::mir::reference::MirProgram::with_type_catalog_and_instances_and_transitions(
            BTreeMap::from([(owner, forged)]),
            canonical.type_catalog().clone(),
            BTreeMap::new(),
            canonical.transitions().clone(),
        )
        .expect_err("call argument identity drift must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("does not reach a canonical call/flow argument at point")
    }));
}

#[test]
fn record_projection_contract_rejects_unknown_field_and_wrong_result_type() {
    let source = "type Point { x: i32, y: bool }\nfunc main() -> i32 { Point { x: 1, y: true }.x }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());

    let mut unknown = canonical.functions().get(&owner).cloned().expect("main");
    let projection = unknown
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find_map(|instruction| match &mut instruction.kind {
            MirInstructionKind::Project { projection, .. } => Some(projection),
            _ => None,
        })
        .expect("record projection");
    *projection = MirProjection::Field(crate::core::NodeId("field:missing".into()));
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner.clone(), unknown)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("unknown record field must fail before a backend");
    assert!(errors.iter().any(|error| error.message.contains("absent")));

    let bool_ty = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi == crate::core::mir::types::MirAbiClass::Bool).then(|| id.clone())
        })
        .expect("bool type");
    let mut wrong_type = canonical.functions().get(&owner).cloned().expect("main");
    let projection_result = wrong_type
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project { result, .. } => Some(result.clone()),
            _ => None,
        })
        .expect("record projection result");
    wrong_type
        .values
        .get_mut(&projection_result)
        .expect("projection value")
        .ty = bool_ty.clone();
    wrong_type.result = bool_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, wrong_type)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("wrong record projection type must fail before a backend");
    assert!(errors.iter().any(|error| {
        error.message.contains("projection") && error.message.contains("disagrees")
    }));
}

#[test]
fn list_index_contract_rejects_non_integer_operand_before_backend() {
    let source = "func main() -> i32 { let values = [10, 20]; values[0] }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical.functions().get(&owner).cloned().expect("main");
    let index = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                projection: MirProjection::Index(index),
                ..
            } => Some(index.clone()),
            _ => None,
        })
        .expect("List index projection");
    let bool_ty = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi == crate::core::mir::types::MirAbiClass::Bool).then(|| id.clone())
        })
        .expect("bool type");
    function.values.get_mut(&index).expect("index value").ty = bool_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("non-integer List index must fail before a backend");
    assert!(errors.iter().any(|error| {
        error.message.contains("List index operand") && error.message.contains("Copy scalar")
    }));
}

#[test]
fn list_index_projection_materializes_type_desc_receipt() {
    let source = "func main() -> i32 { let values = [10, 20, 30]; values[1] }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                projection: MirProjection::Index(index),
                list_index_contract: Some(receipt),
                ..
            } => Some((index.clone(), receipt.clone())),
            _ => None,
        })
        .expect("canonical List index receipt");
    let index_ty = &function.values.get(&receipt.0).expect("index value").ty;
    let result_ty = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::Project {
                result,
                projection: MirProjection::Index(_),
                ..
            } => Some(&function.values.get(result).expect("result value").ty),
            _ => None,
        })
        .expect("List result type");
    assert_eq!(&receipt.1.index_ty, index_ty);
    assert_eq!(&receipt.1.result_ty, result_ty);
    assert_eq!(&receipt.1.element_ty, result_ty);
    assert!(canonical.functions().values().any(|function| function
        .canonical_text()
        .contains("list_index=MirListIndexProjectionContract")));
}

#[test]
fn canonical_program_gate_rejects_missing_or_stale_list_index_receipt() {
    let source = "func main() -> i32 { let values = [10, 20]; values[0] }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());

    let mut missing = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = missing
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::Project {
                    projection: MirProjection::Index(_),
                    ..
                }
            )
        })
        .expect("List projection");
    let MirInstructionKind::Project {
        list_index_contract,
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    list_index_contract.take();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner.clone(), missing)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("missing List index receipt must fail before backend");
    assert!(errors
        .iter()
        .any(|error| error.message.contains("no canonical receipt")));

    let bool_ty = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            (descriptor.abi == crate::core::mir::types::MirAbiClass::Bool).then(|| id.clone())
        })
        .expect("bool TypeDesc");
    let mut stale = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = stale
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::Project {
                    projection: MirProjection::Index(_),
                    ..
                }
            )
        })
        .expect("List projection");
    let MirInstructionKind::Project {
        list_index_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.element_ty = bool_ty;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, stale)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("stale List index receipt must fail before backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List index projection receipt disagrees with TypeDesc")
    }));
}

#[test]
fn list_len_operation_materializes_type_desc_receipt() {
    let source =
        "func main() -> i32 { let values = [10, 20, 30]; let count = len(values); drop(values); count }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let (list_ty, result_ty, receipt) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                result,
                list,
                operation: MirListOperation::Len,
                list_operation_contract: Some(receipt),
                ..
            } => Some((
                function.values.get(list).expect("List value").ty.clone(),
                function.values.get(result).expect("count value").ty.clone(),
                receipt.clone(),
            )),
            _ => None,
        })
        .expect("canonical List.len receipt");
    assert_eq!(receipt.list_ty, list_ty);
    assert!(!receipt.element_ty.as_str().is_empty());
    assert_eq!(receipt.result_ty, result_ty);
    assert_eq!(receipt.operation, MirListOperation::Len);
    assert!(function
        .canonical_text()
        .contains("list_contract=MirListOperationContract"));
}

#[test]
fn list_reverse_operation_materializes_clone_based_type_desc_receipt() {
    let source = "func main() -> List<i32> { let values = [1, 2, 3]; let reversed = reverse(values); drop(values); reversed }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let (list_ty, result_ty, receipt) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                result,
                list,
                operation: MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some((
                function.values.get(list).expect("List value").ty.clone(),
                function
                    .values
                    .get(result)
                    .expect("reversed value")
                    .ty
                    .clone(),
                receipt.clone(),
            )),
            _ => None,
        })
        .expect("canonical List.reverse receipt");
    assert_eq!(receipt.list_ty, list_ty);
    assert_eq!(receipt.result_ty, result_ty);
    assert_eq!(receipt.result_ty, receipt.list_ty);
    assert!(!receipt.element_ty.as_str().is_empty());
    assert_eq!(receipt.operation, MirListOperation::Reverse);

    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference List.reverse execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::List(vec![
            crate::core::mir::reference::MirRuntimeValue::Int(3),
            crate::core::mir::reference::MirRuntimeValue::Int(2),
            crate::core::mir::reference::MirRuntimeValue::Int(1),
        ])
    );
}

#[test]
fn list_reverse_method_materializes_the_same_canonical_operation_receipt() {
    let source = "func main() -> List<i32> { let values = [1, 2, 3]; let reversed = values.reverse(); drop(values); reversed }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let receipt = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                operation: MirListOperation::Reverse,
                list_operation_contract: Some(receipt),
                ..
            } => Some(receipt.clone()),
            _ => None,
        })
        .expect("method call must lower to canonical List.reverse");
    assert_eq!(receipt.operation, MirListOperation::Reverse);
    assert_eq!(receipt.list_ty, receipt.result_ty);
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference List.reverse method execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::List(vec![
            crate::core::mir::reference::MirRuntimeValue::Int(3),
            crate::core::mir::reference::MirRuntimeValue::Int(2),
            crate::core::mir::reference::MirRuntimeValue::Int(1),
        ])
    );
}

#[test]
fn list_concat_method_materializes_two_input_move_receipt_and_consumes_both_sources() {
    let source = "func main() -> List<i32> { let left = [1, 2]; let right = [3, 4]; let joined = left.concat(right); joined }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    let (left, right, receipt) = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .find_map(|instruction| match &instruction.kind {
            MirInstructionKind::ListOp {
                list,
                argument: Some(argument),
                operation: MirListOperation::Concat,
                list_operation_contract: Some(receipt),
                ..
            } => Some((list.clone(), argument.clone(), receipt.clone())),
            _ => None,
        })
        .expect("canonical List.concat receipt");
    assert_eq!(receipt.operation, MirListOperation::Concat);
    assert_eq!(receipt.list_ty, receipt.result_ty);
    assert_eq!(receipt.argument_ty, Some(receipt.list_ty.clone()));
    // Scalar List containers are not checker-linear merely because their
    // element is Copy, so the resource ledger has no linear events to attach
    // here.  The operation's two explicit MoveOut inputs are the ownership
    // proof for this canonical heap-handle transform.
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &left
            )
        })
    }));
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                &instruction.kind,
                MirInstructionKind::Move { result, .. } if result == &right
            )
        })
    }));
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference List.concat method execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::List(vec![
            crate::core::mir::reference::MirRuntimeValue::Int(1),
            crate::core::mir::reference::MirRuntimeValue::Int(2),
            crate::core::mir::reference::MirRuntimeValue::Int(3),
            crate::core::mir::reference::MirRuntimeValue::Int(4),
        ])
    );
}

#[test]
fn canonical_list_concat_receipt_rejects_missing_second_input_type() {
    let source = "func main() -> List<i32> { let left = [1]; let right = [2]; let joined = left.concat(right); joined }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = forged
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::ListOp {
                    operation: MirListOperation::Concat,
                    ..
                }
            )
        })
        .expect("List.concat operation");
    let MirInstructionKind::ListOp {
        list_operation_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.argument_ty = None;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, forged)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("missing List.concat argument TypeDesc must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List operation receipt disagrees with TypeDesc")
    }));
}

#[test]
fn canonical_list_concat_rejects_aliasing_move_inputs_before_consumers() {
    let source =
        "func main() -> List<i32> { let values = [1, 2]; let joined = values.concat(values); joined }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("List.concat must not consume one List value twice");
    let debug = format!("{error:?}");
    assert!(
        debug.contains("use after consuming non-Copy value"),
        "unexpected aliasing diagnostic: {debug}"
    );
}

#[test]
fn canonical_program_gate_rejects_missing_or_stale_list_operation_receipt() {
    let source = "func main() -> i32 { let values = [10, 20]; len(values) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());

    let mut missing = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = missing
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::ListOp { .. }))
        .expect("List operation");
    let MirInstructionKind::ListOp {
        list_operation_contract,
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    list_operation_contract.take();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner.clone(), missing)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("missing List operation receipt must fail before backend");
    assert!(errors.iter().any(|error| error
        .message
        .contains("List operation has no canonical receipt")));

    let mut stale = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = stale
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| matches!(instruction.kind, MirInstructionKind::ListOp { .. }))
        .expect("List operation");
    let MirInstructionKind::ListOp {
        list_operation_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.result_ty = receipt.list_ty.clone();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, stale)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("stale List operation receipt must fail before backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List operation receipt disagrees with TypeDesc")
    }));
}

#[test]
fn canonical_nested_concat_receipt_rejects_forged_scalar_mode() {
    let source = include_str!("../../../tests/fixtures/mir_native_nested_list_concat.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical nested List.concat MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical.functions().get(&owner).cloned().expect("main");
    let instruction = forged
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                MirInstructionKind::ListOp {
                    operation: MirListOperation::Concat,
                    ..
                }
            )
        })
        .expect("nested List.concat operation");
    let MirInstructionKind::ListOp {
        list_operation_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!()
    };
    receipt.mode = crate::core::mir::types::MirListOperationMode::Scalar;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, forged)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("nested List.concat scalar mode must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("List operation receipt disagrees with TypeDesc")
    }));
}

#[test]
fn non_copy_record_projection_lowers_to_explicit_move_project() {
    let source = "type Named { name: string, count: i32 }\nfunc main() -> string { let p = Named { name: \"owned\", count: 41 }; p.name }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main");
    assert!(function.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| matches!(&instruction.kind, MirInstructionKind::MoveProject { .. }))
    }));
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference move projection");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("owned".into())
    );
}

#[test]
fn owned_string_return_lowers_to_move_and_reference_preserves_transfer() {
    let source = include_str!("../../../tests/fixtures/mir_native_owned_string_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical owned String return MIR");
    let owner = crate::core::NodeId("function:echo".into());
    let function = canonical
        .functions()
        .get(&owner)
        .expect("echo MIR function");
    let block = function
        .blocks
        .get(&function.entry)
        .expect("echo entry block");
    let [instruction] = block.instructions.as_slice() else {
        panic!("owned String return must have one ownership instruction");
    };
    let MirInstructionKind::Move { result, source } = &instruction.kind else {
        panic!("direct owned String return must move its source");
    };
    assert_eq!(source, &function.parameters[0]);
    assert!(matches!(
        &block.terminator,
        MirTerminator::Return { value: Some(value) } if value == result
    ));

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(
            &owner,
            &[crate::core::mir::reference::MirRuntimeValue::String(
                "oracle".into(),
            )],
        )
        .expect("reference direct owned String return");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("oracle".into())
    );
}

#[test]
fn direct_owned_string_calls_remain_canonical_and_reference_transfers_arguments() {
    let source = include_str!("../../../tests/fixtures/mir_verifier_owned_string_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let program = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("direct owned String calls must lower to canonical MIR");
    let forward = program
        .functions()
        .get(&crate::core::NodeId("function:forward".into()))
        .expect("forward MIR function");
    assert!(forward
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .any(|instruction| matches!(
            instruction.kind,
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(ref owner),
                result: Some(_),
                ..
            } if owner.0 == "function:echo"
        )));

    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&program)
        .execute(
            &crate::core::NodeId("function:forward".into()),
            &[crate::core::mir::reference::MirRuntimeValue::String(
                "oracle".into(),
            )],
        )
        .expect("reference direct owned String call execution");
    assert_eq!(
        value,
        crate::core::mir::reference::MirRuntimeValue::String("oracle".into())
    );
}

#[test]
fn non_copy_record_projection_with_non_copy_sibling_fails_closed() {
    let source = "type Pair { left: string, right: string }\nfunc main() -> string { let p = Pair { left: \"left\", right: \"right\" }; p.left }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("record with a non-Copy sibling must not invent a residual");
    let text = format!("{error:?}");
    assert!(
        text.contains("non-Copy") || text.contains("move projection"),
        "unexpected fail-closed error: {text}"
    );
}

#[test]
fn non_copy_tuple_materializes_field_drop_schedule_before_backend() {
    let source = "func main() -> (string, i32) { (\"owned\", 41) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("tuple glue must be materialized");
    let tuple_id = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            matches!(descriptor.layout, crate::core::mir::types::MirLayout::Tuple(ref fields) if fields.len() == 2)
                .then(|| id.clone())
        })
        .expect("tuple descriptor");
    let descriptor = canonical
        .type_catalog()
        .get(&tuple_id)
        .expect("tuple TypeDesc");
    assert_eq!(
        descriptor.glue,
        crate::core::mir::types::MirGlueContract {
            move_out: crate::core::mir::types::MirGlueKind::Aggregate,
            clone: crate::core::mir::types::MirGlueKind::Aggregate,
            drop: crate::core::mir::types::MirGlueKind::Aggregate,
        }
    );
    assert_eq!(
        descriptor
            .drop_plan
            .as_ref()
            .expect("drop plan")
            .fields
            .iter()
            .map(|field| field.index)
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
}

#[test]
fn lowers_move_owned_option_payload_to_explicit_mir_node() {
    let source = "func main() -> Option<string> { Some(\"owned\") }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned variant glue must be materialized");
    let main = canonical
        .functions()
        .get(&crate::core::NodeId("function:main".into()))
        .expect("main MIR");
    assert!(main.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
            )
        })
    }));
}

#[test]
fn rejects_shallow_variant_construction_before_any_backend() {
    let source = "func main() -> Option<string> { Some(\"owned\") }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariantMove { .. }
            )
        })
        .expect("move variant construction");
    let crate::core::mir::MirInstructionKind::ConstructVariantMove {
        result,
        nominal,
        variant,
        fields,
    } = instruction.kind.clone()
    else {
        unreachable!();
    };
    instruction.kind = crate::core::mir::MirInstructionKind::ConstructVariant {
        result,
        nominal,
        variant,
        fields,
    };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("shallow construction must fail closed");
    assert!(errors.iter().any(|error| {
        error.message.contains("ConstructVariantMove") || error.message.contains("non-Copy")
    }));
}

#[test]
fn flat_copy_user_enum_construction_rejects_forged_field_before_consumers() {
    let fixture = crate::core::mir::test_support::direct_flat_copy_enum_construct_fixture();
    let owner = fixture.function.clone();
    let mut function = fixture
        .program
        .functions()
        .get(&owner)
        .cloned()
        .expect("construct_signal MIR");
    let instruction = function
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariant { .. }
            )
        })
        .expect("flat Copy ConstructVariant");
    let crate::core::mir::MirInstructionKind::ConstructVariant { fields, .. } =
        &mut instruction.kind
    else {
        unreachable!();
    };
    fields[0].0 = crate::core::NodeId("variant:forged-field".into());

    let mut functions = fixture.program.functions().clone();
    functions.insert(owner, function);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        functions,
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged flat Copy construction field must fail at canonical MIR");
    assert!(errors.iter().any(|error| {
        error.message.contains("variant payload field") && error.message.contains("absent")
    }));
}

#[test]
fn surface_flat_copy_user_enum_constructor_materializes_construct_variant() {
    let source = include_str!("../../../tests/fixtures/mir_custom_enum_flat_copy.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy user-enum constructor must materialize to MIR");
    let function = canonical
        .functions()
        .get(&crate::core::NodeId("function:make_signal".into()))
        .expect("make_signal MIR");
    assert!(function.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::ConstructVariant { .. }
            )
        })
    }));
}

#[test]
fn surface_flat_copy_user_enum_match_materializes_switch() {
    let source = include_str!("../../../tests/fixtures/mir_custom_enum_flat_copy.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy user-enum match must materialize to MIR");
    let function = canonical
        .functions()
        .get(&crate::core::NodeId("function:read_signal".into()))
        .expect("read_signal MIR");
    let switches = function
        .blocks
        .values()
        .filter_map(|block| match &block.terminator {
            crate::core::mir::MirTerminator::Switch { scrutinee, arms } => Some((scrutinee, arms)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(switches.len(), 1);
    let (scrutinee, arms) = switches[0];
    assert_eq!(arms.len(), 2);
    let scrutinee_ty = &function.values.get(scrutinee).expect("switch scrutinee").ty;
    assert!(canonical
        .type_catalog()
        .validate_flat_copy_variant(scrutinee_ty)
        .is_ok());
    assert!(arms.iter().any(|arm| !arm.bindings.is_empty()));
}

#[test]
fn surface_non_flat_user_enum_constructor_stays_outside_canonical_mir() {
    let source = include_str!("../../../tests/real_world/custom_enum_string_payload.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-flat user-enum constructor must remain outside this MIR slice");
    let text = format!("{error:?}");
    assert!(text.contains("Constructor") && text.contains("not a materialized MIR function"));
}

#[test]
fn surface_mixed_copy_user_enum_match_stays_outside_canonical_mir() {
    let source = "type Mixed { Number(i32) | Wide(i64) | Empty }\nfunc inspect(value: Mixed) -> i32 { match value { Number(n) => n, Wide(_) => 0, Empty => 0 } }\nfunc main() -> i32 { 0 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("mixed Copy user-enum match must fail before consumers");
    let text = format!("{error:?}");
    assert!(text.contains("flat Copy variant contract"), "{text}");
}

#[test]
fn non_copy_record_materializes_field_drop_schedule_before_backend() {
    let source = "type Named { name: string, count: i32 }\nfunc main() -> i32 { let p = Named { count: 41, name: \"owned\" }; drop(p); 42 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("record glue must be materialized");
    let (record_id, descriptor) = canonical
        .type_catalog()
        .iter()
        .find_map(|(id, descriptor)| {
            matches!(
                descriptor.layout,
                crate::core::mir::types::MirLayout::Record { ref nominal, .. }
                    if nominal.as_str().ends_with("Named")
            )
            .then(|| (id.clone(), descriptor))
        })
        .expect("record descriptor");
    assert_eq!(
        descriptor.ownership,
        crate::core::mir::types::MirOwnership::Move
    );
    assert_eq!(
        descriptor.glue,
        crate::core::mir::types::MirGlueContract {
            move_out: crate::core::mir::types::MirGlueKind::Aggregate,
            clone: crate::core::mir::types::MirGlueKind::Aggregate,
            drop: crate::core::mir::types::MirGlueKind::Aggregate,
        }
    );
    assert_eq!(
        descriptor
            .drop_plan
            .as_ref()
            .expect("record drop plan")
            .fields
            .iter()
            .map(|field| field.index)
            .collect::<Vec<_>>(),
        vec![1, 0]
    );
    canonical
        .type_catalog()
        .validate_aggregate_glue(&record_id, crate::core::mir::types::MirGlueOperation::Drop)
        .expect("record drop schedule");
}

#[test]
fn rejects_reuse_of_record_field_after_aggregate_construction() {
    let source = "type Named { name: string, count: i32 }\nfunc main() -> i32 { let p = Named { count: 41, name: \"owned\" }; drop(p); 42 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("record MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical.functions().get(&owner).cloned().expect("main");
    let block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Construct { .. }
                )
            })
        })
        .expect("record construction block");
    let field = block
        .instructions
        .iter()
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Construct { fields, .. } => fields
                .iter()
                .find(|field| {
                    function
                        .values
                        .get(*field)
                        .and_then(|value| canonical.type_catalog().get(&value.ty))
                        .is_some_and(|descriptor| {
                            descriptor.ownership == crate::core::mir::types::MirOwnership::Move
                        })
                })
                .cloned(),
            _ => None,
        })
        .expect("owned record field");
    block.instructions.push(crate::core::mir::MirInstruction {
        id: crate::core::mir::MirInstructionId::new("synthetic/reuse-record-field")
            .expect("instruction id"),
        kind: crate::core::mir::MirInstructionKind::Drop { value: field },
    });
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("record field reuse must fail before a backend");
    assert!(errors.iter().any(|error| {
        error.message.contains("use after consuming")
            || error.message.contains("already consumed")
            || error.message.contains("multiple")
            || error.message.contains("reuse")
    }));
}

#[test]
fn materializes_record_with_move_owned_variant_field() {
    let source = "type Bad { value: Option<string> }\nfunc main() -> i32 { let p = Bad { value: Some(\"owned\") }; drop(p); 42 }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("variant field glue is materialized recursively");
    assert!(canonical.type_catalog().iter().any(|(_, descriptor)| {
        matches!(
            descriptor.layout,
            crate::core::mir::types::MirLayout::Record { .. }
        ) && descriptor.glue.move_out == crate::core::mir::types::MirGlueKind::Aggregate
    }));
}

#[test]
fn malformed_aggregate_drop_schedule_is_rejected_before_backend() {
    let source = "func main() -> (string, i32) { (\"owned\", 41) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).cloned().expect("main");
    let tuple_id = function
        .values
        .values()
        .map(|value| value.ty.clone())
        .find(|ty| {
            canonical
                .type_catalog()
                .get(ty)
                .is_some_and(|descriptor| descriptor.drop_plan.is_some())
        })
        .expect("tuple value");
    let mut catalog = canonical.type_catalog().clone();
    let descriptor = catalog
        .iter()
        .find_map(|(id, descriptor)| (id == &tuple_id).then(|| descriptor.clone()))
        .expect("tuple descriptor");
    let mut malformed = descriptor.drop_plan.clone().expect("tuple drop plan");
    malformed.fields.reverse();
    catalog.replace_for_test_only(
        tuple_id,
        crate::core::mir::types::MirTypeDesc {
            drop_plan: Some(malformed),
            variant_drop_plan: descriptor.variant_drop_plan.clone(),
            ..descriptor
        },
    );
    let error = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        catalog,
    )
    .expect_err("malformed drop plan must fail closed");
    assert!(error
        .iter()
        .any(|error| error.message.contains("drop plan")));
}

#[test]
fn rejects_reuse_of_tuple_field_after_aggregate_construction() {
    let source = "func main() -> (string, i32) { (\"owned\", 41) }";
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut function = canonical.functions().get(&owner).cloned().expect("main");
    let block = function
        .blocks
        .values_mut()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    crate::core::mir::MirInstructionKind::Construct { .. }
                )
            })
        })
        .expect("tuple construction block");
    let field = block
        .instructions
        .iter()
        .find_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Construct { fields, .. } => fields
                .iter()
                .find(|field| {
                    function
                        .values
                        .get(*field)
                        .and_then(|value| canonical.type_catalog().get(&value.ty))
                        .is_some_and(|descriptor| {
                            descriptor.ownership == crate::core::mir::types::MirOwnership::Move
                        })
                })
                .cloned(),
            _ => None,
        })
        .expect("owned tuple field");
    block.instructions.push(crate::core::mir::MirInstruction {
        id: crate::core::mir::MirInstructionId::new("synthetic/reuse-tuple-field")
            .expect("instruction id"),
        kind: crate::core::mir::MirInstructionKind::Drop { value: field },
    });
    let error = crate::core::mir::reference::MirProgram::with_type_catalog(
        std::collections::BTreeMap::from([(owner, function)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("an aggregate construction must consume each owned field once");
    assert!(error
        .iter()
        .any(|error| error.message.contains("use after consuming non-Copy value")));
}

#[test]
fn flat_copy_variant_predicates_materialize_checker_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy variant predicates must lower");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main MIR");
    let predicates = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::VariantPredicate {
                predicate,
                result,
                variant,
                contract: Some(receipt),
                ..
            } => Some((*predicate, result.clone(), variant.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(predicates.len(), 4);
    for (predicate, result, variant, receipt) in predicates {
        assert_eq!(receipt.variant_ty, function.values[&variant].ty);
        assert_eq!(receipt.result_ty, function.values[&result].ty);
        assert_eq!(
            canonical
                .type_catalog()
                .get(&receipt.result_ty)
                .map(|desc| &desc.kind),
            Some(&crate::core::mir::types::MirTypeKind::Primitive(
                crate::core::PrimitiveType::Bool
            ))
        );
        assert_eq!(receipt.predicate, predicate);
        assert_eq!(
            receipt.nominal.as_str(),
            match predicate {
                crate::core::mir::MirVariantPredicate::IsSome
                | crate::core::mir::MirVariantPredicate::IsNone => "builtin:type:Option",
                crate::core::mir::MirVariantPredicate::IsOk
                | crate::core::mir::MirVariantPredicate::IsErr => "builtin:type:Result",
            }
        );
        assert_eq!(
            receipt.variant_name,
            match predicate {
                crate::core::mir::MirVariantPredicate::IsSome => "Some",
                crate::core::mir::MirVariantPredicate::IsNone => "None",
                crate::core::mir::MirVariantPredicate::IsOk => "Ok",
                crate::core::mir::MirVariantPredicate::IsErr => "Err",
            }
        );
        assert!(receipt.discriminant <= u8::MAX as u16);
    }
    let text = function.canonical_text();
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("variant_predicate "))
            .count(),
        4
    );
    assert!(text.contains("variant_contract=MirVariantPredicateContract"));
}

#[test]
fn variant_predicate_receipt_drift_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_predicate.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical
        .functions()
        .get(&owner)
        .cloned()
        .expect("main MIR");
    let instruction = forged
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::VariantPredicate { .. }
            )
        })
        .expect("variant predicate");
    let crate::core::mir::MirInstructionKind::VariantPredicate {
        contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!("predicate receipt is mandatory in canonical MIR");
    };
    receipt.discriminant = receipt.discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        BTreeMap::from([(owner, forged)]),
        canonical.type_catalog().clone(),
    )
    .expect_err("stale variant predicate receipt must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant predicate receipt disagrees with TypeDesc")
    }));
}

#[test]
fn non_copy_variant_predicate_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_predicate_rejected.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let error = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect_err("non-Copy variant predicate must fail closed");
    let debug = format!("{error:?}");
    assert!(
        debug.contains("Aggregate/Copy") && debug.contains("canonical no-op glue"),
        "unexpected non-Copy predicate diagnostic: {debug}"
    );
}

#[test]
fn direct_flat_copy_variant_calls_materialize_signature_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_call.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("flat Copy variant calls must lower");
    let owner = crate::core::NodeId("function:main".into());
    let function = canonical.functions().get(&owner).expect("main MIR");
    let calls = function
        .blocks
        .values()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                variant_call_contract: Some(receipt),
                ..
            } => Some((callee.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for (callee, receipt) in calls {
        assert_eq!(callee, receipt.callee);
        assert_eq!(
            receipt.type_arguments,
            Vec::<crate::core::ResolvedTypeId>::new()
        );
        assert_eq!(receipt.parameter_types.len(), 1);
        assert_eq!(receipt.nominal.as_str(), "builtin:type:Option");
        assert_eq!(receipt.variants.len(), 2);
        assert!(receipt
            .variants
            .iter()
            .all(|variant| variant.discriminant <= u8::MAX as u16));
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("receipt must be TypeDesc-derived");
    }
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference direct variant call execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(4)
    );
}

#[test]
fn direct_move_owned_result_calls_materialize_signature_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_result_string_i32_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result calls must lower");
    let owner = crate::core::NodeId("function:main".into());
    let calls = canonical
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                variant_call_contract: Some(receipt),
                ..
            } => Some((callee.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for (callee, receipt) in calls {
        assert_eq!(callee, receipt.callee);
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        );
        assert_eq!(receipt.payload_types.len(), 2);
        assert_eq!(receipt.payload_ty, receipt.payload_types[0]);
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("move-owned call receipt must be TypeDesc-derived");
    }
    let reference = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&owner, &[])
        .expect("reference move-owned Result call execution");
    assert_eq!(
        reference,
        crate::core::mir::reference::MirRuntimeValue::Int(48)
    );
}

#[test]
fn direct_move_owned_result_list_calls_materialize_signature_receipts() {
    let source = include_str!("../../../tests/fixtures/mir_result_list_i32_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result<List<i32>, i32> calls must lower");
    let calls = canonical
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                variant_call_contract: Some(receipt),
                ..
            } => Some((callee.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for (callee, receipt) in calls {
        assert_eq!(callee, receipt.callee);
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        );
        assert_eq!(receipt.payload_types.len(), 2);
        assert_eq!(receipt.payload_ty, receipt.payload_types[0]);
        let payload = canonical
            .type_catalog()
            .get(&receipt.payload_types[0])
            .expect("List payload TypeDesc");
        assert!(matches!(
            payload.kind,
            crate::core::mir::types::MirTypeKind::List
        ));
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("List move-owned call receipt must be TypeDesc-derived");
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference move-owned Result<List<i32>, i32> call execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(48));
}

#[test]
fn direct_move_owned_result_list_i64_bool_calls_share_the_same_scalar_contract() {
    let source = include_str!("../../../tests/fixtures/mir_result_list_i64_bool_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result<List<i64|bool>, i32> calls must lower");
    let calls = canonical
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                variant_call_contract: Some(receipt),
                ..
            } => Some((callee.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    for (callee, receipt) in calls {
        assert_eq!(receipt.callee, callee);
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        );
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("List<i64|bool> call receipt must be TypeDesc-derived");
        let payload = canonical
            .type_catalog()
            .get(&receipt.payload_ty)
            .expect("managed List payload TypeDesc");
        assert!(matches!(
            payload.kind,
            crate::core::mir::types::MirTypeKind::List
        ));
    }
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Result<List<i64|bool>, i32> call execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(48));
}

#[test]
fn direct_move_owned_result_list_i64_bool_err_paths_preserve_residual_ownership() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_list_i64_bool_err_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("move-owned Result<List<i64|bool>, i32> Err paths must lower");
    let calls = canonical
        .functions()
        .values()
        .flat_map(|function| function.blocks.values())
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match &instruction.kind {
            crate::core::mir::MirInstructionKind::Call {
                callee: crate::core::ir::ResolvedCallee::Function(callee),
                variant_call_contract: Some(receipt),
                ..
            } => Some((callee.clone(), receipt.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 4);
    for (callee, receipt) in calls {
        assert_eq!(receipt.callee, callee);
        assert_eq!(
            receipt.mode,
            crate::core::mir::types::MirVariantCallAbiMode::MoveOwned
        );
        assert_eq!(
            receipt.return_mode,
            crate::core::mir::types::MirVariantCallReturnMode::OwnershipPathExclusiveMerge
        );
        canonical
            .type_catalog()
            .validate_variant_call_abi_receipt(
                &receipt.callee,
                &receipt.type_arguments,
                &receipt.parameter_types,
                &receipt.result_ty,
                &receipt,
            )
            .expect("Err-path receipt must be TypeDesc-derived");
    }
    crate::core::mir::validate_managed_result_call_island(&canonical)
        .expect("Err-path managed Result island validation");
    let value = crate::core::mir::reference::MirReferenceInterpreter::new(&canonical)
        .execute(&crate::core::NodeId("function:main".into()), &[])
        .expect("reference Err-path execution");
    assert_eq!(value, crate::core::mir::reference::MirRuntimeValue::Int(56));
}

#[test]
fn move_owned_result_call_receipt_drift_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_result_string_i32_call_return.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let mut forged = canonical.functions().clone();
    let instruction = forged
        .values_mut()
        .flat_map(|function| function.blocks.values_mut())
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(_),
                    ..
                }
            )
        })
        .expect("move-owned Result call");
    let crate::core::mir::MirInstructionKind::Call {
        variant_call_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!("receipt selected above");
    };
    receipt.payload_types[1] = receipt.payload_types[0].clone();
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("stale move-owned call ABI receipt must fail before a backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant call ABI receipt disagrees with TypeDesc")
    }));
}

#[test]
fn move_owned_result_return_merge_rejects_switch_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:choose".into());
    let mut forged = canonical.functions().clone();
    let choose = forged.get_mut(&owner).expect("choose MIR");
    let entry = choose.entry.clone();
    let old_terminator = choose
        .blocks
        .get(&entry)
        .expect("choose entry")
        .terminator
        .clone();
    let crate::core::mir::MirTerminator::Branch {
        condition,
        then_edge,
        then_target,
        then_arguments,
        else_edge,
        else_target,
        else_arguments,
    } = old_terminator
    else {
        panic!("choose must start with a canonical Branch");
    };
    choose
        .blocks
        .get_mut(&entry)
        .expect("choose entry")
        .terminator = crate::core::mir::MirTerminator::Switch {
        scrutinee: condition,
        arms: vec![
            crate::core::mir::MirSwitchArm {
                edge: then_edge,
                target: then_target,
                arguments: then_arguments,
                bindings: vec![],
                case: crate::core::mir::MirSwitchCase::Literal(crate::core::ResolvedLiteral::Bool(
                    true,
                )),
            },
            crate::core::mir::MirSwitchArm {
                edge: else_edge,
                target: else_target,
                arguments: else_arguments,
                bindings: vec![],
                case: crate::core::mir::MirSwitchCase::Default,
            },
        ],
    };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("unsupported ownership merge CFG must fail before consumers");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("MIR verifier direct variant call return merge only admits Goto/Branch CFG")
    }));
}

#[test]
fn result_switch_move_projection_receipt_carries_field_ownership_and_glue() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let checked_fn = canonical
        .functions()
        .get(&crate::core::NodeId("function:checked".into()))
        .expect("checked MIR");
    let arms = checked_fn
        .blocks
        .values()
        .find_map(|block| match &block.terminator {
            crate::core::mir::MirTerminator::SwitchMove { arms, .. } => Some(arms),
            _ => None,
        })
        .expect("Result SwitchMove");
    let mut projections = arms
        .iter()
        .filter_map(|arm| {
            let variant = match &arm.case {
                crate::core::mir::MirSwitchCase::Variant(variant) => variant,
                _ => return None,
            };
            Some((variant.clone(), arm.bindings.first()?.projection.clone()))
        })
        .collect::<Vec<_>>();
    projections.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(projections.len(), 2);
    let ok = projections
        .iter()
        .find(|(variant, _)| variant.0.ends_with("Result::Ok"))
        .expect("Ok projection");
    assert_eq!(ok.1.ownership, crate::core::mir::types::MirOwnership::Move);
    assert_eq!(
        ok.1.move_out_glue,
        crate::core::mir::types::MirGlueKind::OwnedString
    );
    let err = projections
        .iter()
        .find(|(variant, _)| variant.0.ends_with("Result::Err"))
        .expect("Err projection");
    assert_eq!(err.1.ownership, crate::core::mir::types::MirOwnership::Copy);
    assert_eq!(
        err.1.move_out_glue,
        crate::core::mir::types::MirGlueKind::Noop
    );
}

#[test]
fn result_switch_move_projection_receipt_drift_is_rejected_before_consumers() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let mut forged = canonical.functions().clone();
    let checked_fn = forged
        .get_mut(&crate::core::NodeId("function:checked".into()))
        .expect("checked MIR");
    let binding = checked_fn
        .blocks
        .values_mut()
        .flat_map(|block| match &mut block.terminator {
            crate::core::mir::MirTerminator::SwitchMove { arms, .. } => arms
                .iter_mut()
                .flat_map(|arm| arm.bindings.iter_mut())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .find(|binding| binding.projection.variant.0.ends_with("Result::Ok"))
        .expect("Ok projection binding");
    binding.projection.move_out_glue = crate::core::mir::types::MirGlueKind::Noop;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("forged projection glue must fail before a consumer");
    assert!(errors
        .iter()
        .any(|error| { error.message.contains("variant payload projection receipt") }));
}

#[test]
fn custom_enum_switch_move_receipt_drift_is_rejected_before_consumers() {
    let fixture = crate::core::mir::test_support::direct_enum_switch_move_fixture();
    let owner = crate::core::NodeId("function:take".into());
    let mut forged = fixture.program.functions().clone();
    let forged_fn = forged.get_mut(&owner).expect("enum take MIR");
    let binding = forged_fn
        .blocks
        .values_mut()
        .flat_map(|block| match &mut block.terminator {
            crate::core::mir::MirTerminator::SwitchMove { arms, .. } => arms
                .iter_mut()
                .flat_map(|arm| arm.bindings.iter_mut())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .next()
        .expect("Keep projection binding");
    binding.projection.move_out_glue = crate::core::mir::types::MirGlueKind::Noop;
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        fixture.program.type_catalog().clone(),
    )
    .expect_err("forged enum projection glue must fail before consumers");
    assert!(errors
        .iter()
        .any(|error| { error.message.contains("variant payload projection receipt") }));
}

#[test]
fn result_read_only_switch_rejects_move_owned_payload_projection() {
    let source =
        include_str!("../../../tests/fixtures/mir_result_string_i32_call_return_multipath.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let mut forged = canonical.functions().clone();
    let checked_fn = forged
        .get_mut(&crate::core::NodeId("function:checked".into()))
        .expect("checked MIR");
    let block = checked_fn
        .blocks
        .values_mut()
        .find(|block| {
            matches!(
                block.terminator,
                crate::core::mir::MirTerminator::SwitchMove { .. }
            )
        })
        .expect("Result SwitchMove");
    let crate::core::mir::MirTerminator::SwitchMove { scrutinee, arms } = block.terminator.clone()
    else {
        unreachable!("Result SwitchMove selected above");
    };
    block.terminator = crate::core::mir::MirTerminator::Switch { scrutinee, arms };
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("read-only Switch cannot transport an owned Result payload");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("read-only variant payload projection field")
    }));
}

#[test]
fn variant_call_abi_receipt_drift_is_rejected_before_consumers() {
    let source = include_str!("../../../tests/fixtures/mir_native_variant_call.mimi");
    let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
    let file = crate::parser::Parser::new(tokens)
        .parse_file()
        .expect("parse");
    let checked = crate::core::check_program(&file).expect("check");
    let canonical = crate::core::mir::reference::MirProgram::from_checked_program(&checked)
        .expect("canonical MIR");
    let owner = crate::core::NodeId("function:main".into());
    let mut forged = canonical.functions().clone();
    let main = forged.get_mut(&owner).expect("main MIR");
    let instruction = main
        .blocks
        .values_mut()
        .flat_map(|block| block.instructions.iter_mut())
        .find(|instruction| {
            matches!(
                instruction.kind,
                crate::core::mir::MirInstructionKind::Call {
                    variant_call_contract: Some(_),
                    ..
                }
            )
        })
        .expect("variant call");
    let crate::core::mir::MirInstructionKind::Call {
        variant_call_contract: Some(receipt),
        ..
    } = &mut instruction.kind
    else {
        unreachable!("receipt selected above");
    };
    receipt.variants[0].discriminant = receipt.variants[0].discriminant.wrapping_add(1);
    let errors = crate::core::mir::reference::MirProgram::with_type_catalog(
        forged,
        canonical.type_catalog().clone(),
    )
    .expect_err("stale call ABI receipt must fail before a backend");
    assert!(errors.iter().any(|error| {
        error
            .message
            .contains("variant call ABI receipt disagrees with TypeDesc")
    }));
}
