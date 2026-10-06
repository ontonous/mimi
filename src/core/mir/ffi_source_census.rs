//! FFI-specific cross-check between checked call/declaration source and the
//! canonical MIR receipts consumed by the native, VM, reference, and verifier
//! backends. The general proof-source census is independent of the FFI table;
//! this adapter joins those two sealed views without rebuilding surface AST.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::{ResolvedCallee, ResolvedFfiParameterMode};
use crate::core::NodeId;

use super::{
    MirFfiCallContract, MirFfiSourceArgument, MirInstructionId, MirInstructionKind,
    MirProofSourceCall, MirProofSourceFfiParameterMode, MirValueId,
};
use crate::core::mir::reference::MirProgram;

pub(crate) fn validate_ffi_source_census(program: &MirProgram) -> Vec<String> {
    let census = program.proof_source_census();
    let functions = program.functions();
    let contracts = program.ffi_calls();
    let mut errors = Vec::new();
    let mut source_calls =
        BTreeMap::<MirInstructionId, (NodeId, NodeId, &MirProofSourceCall)>::new();
    let mut actual_calls =
        BTreeMap::<MirInstructionId, (NodeId, NodeId, Vec<MirValueId>, Option<MirValueId>)>::new();

    for function in functions.values() {
        let mut function_has_extern = false;
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                let MirInstructionKind::Call {
                    callee: ResolvedCallee::Extern(callee),
                    arguments,
                    result,
                    ..
                } = &instruction.kind
                else {
                    continue;
                };
                function_has_extern = true;
                if actual_calls
                    .insert(
                        instruction.id.clone(),
                        (
                            function.owner.clone(),
                            callee.clone(),
                            arguments.clone(),
                            result.clone(),
                        ),
                    )
                    .is_some()
                {
                    errors.push(format!(
                        "extern MIR instruction '{}' occurs more than once in the FFI source join",
                        instruction.id
                    ));
                }
            }
        }
        let Some(owner) = census.owner(&function.owner) else {
            if function_has_extern {
                errors.push(format!(
                    "extern MIR owner '{}' has no independently checked source-call census",
                    function.owner.0
                ));
            }
            continue;
        };
        let Some(source_owner) = owner.source_owner() else {
            if function_has_extern {
                errors.push(format!(
                    "extern MIR owner '{}' has no canonical CheckedProgram source owner",
                    function.owner.0
                ));
            }
            continue;
        };
        for call in owner.body_calls() {
            let ResolvedCallee::Extern(declaration) = call.callee() else {
                continue;
            };
            if !call.has_complete_source_shape() {
                errors.push(format!(
                    "checked extern call '{}' has no complete typed source argument/result/effect shape",
                    call.call_node().0
                ));
                continue;
            }
            // `MirProofSourceCall::owner` is the selected callable owner at
            // the call point (the caller for an ordinary body call), not the
            // extern declaration identity.  Compare the callee itself with
            // the declaration node; conflating these two owner axes made
            // every checked extern call fail the source census even though
            // its typed caller and declaration were consistent.
            if call.body_owner() != source_owner
                || call.callee() != &ResolvedCallee::Extern(declaration.clone())
            {
                errors.push(format!(
                    "checked extern call '{}' has inconsistent caller/declaration owner identity",
                    call.call_node().0
                ));
                continue;
            }
            let instruction = match MirInstructionId::new(format!(
                "inst:call:{}",
                call.call_node().0
            )) {
                Ok(instruction) => instruction,
                Err(_) => {
                    errors.push(format!(
                        "checked extern call NodeId '{}' cannot form a canonical MIR instruction identity",
                        call.call_node().0
                    ));
                    continue;
                }
            };
            if source_calls
                .insert(
                    instruction.clone(),
                    (function.owner.clone(), declaration.clone(), call),
                )
                .is_some()
            {
                errors.push(format!(
                    "checked extern source call '{}' maps to multiple MIR caller owners",
                    instruction
                ));
            }
        }
    }

    for (instruction, (caller, declaration, source_call)) in &source_calls {
        let Some((actual_caller, actual_callee, actual_arguments, actual_result)) =
            actual_calls.get(instruction)
        else {
            errors.push(format!(
                "checked extern source call '{}' has no exact MIR Call instruction",
                source_call.call_node().0
            ));
            continue;
        };
        let Some(contract) = contracts.get(instruction) else {
            errors.push(format!(
                "checked extern source call '{}' has no MIR FFI call contract",
                source_call.call_node().0
            ));
            continue;
        };
        let Some(function) = functions.get(caller) else {
            errors.push(format!(
                "checked extern source call '{}' has no MIR caller function",
                source_call.call_node().0
            ));
            continue;
        };
        if actual_caller != caller || actual_callee != declaration {
            errors.push(format!(
                "checked extern source call '{}' disagrees with its MIR caller/callee",
                source_call.call_node().0
            ));
        }
        validate_call_join(
            source_call,
            contract,
            caller,
            declaration,
            function,
            actual_arguments,
            actual_result,
            census,
            &mut errors,
        );
    }

    for (instruction, (caller, declaration, _, _)) in &actual_calls {
        if !source_calls.contains_key(instruction) {
            errors.push(format!(
                "extern MIR call '{}' in owner '{}' to '{}' has no matching checked body call",
                instruction, caller.0, declaration.0
            ));
        }
        if !contracts.contains_key(instruction) {
            errors.push(format!(
                "extern MIR call '{}' has no canonical FFI call contract",
                instruction
            ));
        }
    }
    for instruction in contracts.keys() {
        if !actual_calls.contains_key(instruction) {
            errors.push(format!(
                "MIR FFI source receipt '{}' is orphaned from an extern MIR Call",
                instruction
            ));
        }
    }

    let mut checked_declarations = BTreeSet::new();
    for instruction in contracts.keys() {
        if let Some((_, declaration, _)) = source_calls.get(instruction) {
            checked_declarations.insert(declaration.clone());
        }
    }
    for declaration in checked_declarations {
        if census.declaration(&declaration).is_none() {
            errors.push(format!(
                "checked extern declaration '{}' is absent from the nonoptional source census",
                declaration.0
            ));
        }
    }

    errors
}

fn validate_call_join(
    source: &MirProofSourceCall,
    contract: &MirFfiCallContract,
    caller: &NodeId,
    declaration: &NodeId,
    function: &super::MirFunction,
    actual_arguments: &[MirValueId],
    actual_result: &Option<MirValueId>,
    census: &super::MirProofSourceCensus,
    errors: &mut Vec<String>,
) {
    let prefix = format!("extern source/MIR join '{}'", source.call_node().0);
    if contract.instruction.as_str() != format!("inst:call:{}", source.call_node().0)
        || contract.call_node != *source.call_node()
        || contract.caller != *caller
        || contract.callee != *declaration
        || contract.declaration != *declaration
        || contract.source_body_owner != *source.body_owner()
        || contract.source_statement != *source.statement()
        || source.order() != Some(contract.source_order)
        || !typed_scope_matches_source(source.scope(), &contract.source_scope)
        || source.guards() != contract.source_guards.as_slice()
        || source.disposition() != &contract.source_disposition
        || source.effect_authority() != contract.source_effect_authority
    {
        errors.push(format!(
            "{prefix}: call owner, ordered execution event, scope, guard, or effect authority differs"
        ));
    }
    if contract.symbol.is_empty()
        || source.callee_identity() != format!("{:?}", source.callee())
        || source.selected_signature() != contract.selected_signature
        || source.expression_type() != Some(&contract.source_expression_type)
        || source.type_arguments().unwrap_or_default() != contract.source_type_arguments
        || source.result_type() != Some(&contract.source_result_type)
        || source.effects().unwrap_or_default() != contract.source_effects
        || contract.source_result_type != contract.result_type
    {
        errors.push(format!(
            "{prefix}: typed call signature/effect facts differ"
        ));
    }
    if actual_arguments != contract.arguments || actual_result != &contract.result {
        errors.push(format!(
            "{prefix}: FFI receipt arguments/result differ from MIR Call"
        ));
    }
    if let Some(typed_declaration) = census.declaration(declaration) {
        let expected_modes = typed_declaration
            .fixed_parameter_modes()
            .iter()
            .map(|mode| match mode {
                MirProofSourceFfiParameterMode::Value => ResolvedFfiParameterMode::Value,
                MirProofSourceFfiParameterMode::Borrow => ResolvedFfiParameterMode::Borrow,
                MirProofSourceFfiParameterMode::Move => ResolvedFfiParameterMode::Move,
            })
            .collect::<Vec<_>>();
        if !typed_declaration.signature_ready()
            || contract.declaration != *typed_declaration.declaration()
            || contract.declaration_block != *typed_declaration.block()
            || contract.symbol != typed_declaration.symbol()
            || contract.abi != typed_declaration.abi()
            || contract.fixed_argument_count != typed_declaration.fixed_parameter_count()
            || contract.fixed_parameter_ids != typed_declaration.fixed_parameter_ids()
            || contract.fixed_parameter_modes != expected_modes
            || contract.fixed_parameter_modes.len() != contract.fixed_argument_count
            || contract.variadic != typed_declaration.variadic()
            || typed_declaration.result_type() != Some(&contract.result_type)
            || contract.block_no_panic != typed_declaration.block_no_panic()
            || contract.declaration_no_panic != typed_declaration.declaration_no_panic()
            || contract.block_returns_errno != typed_declaration.block_returns_errno()
            || contract.declaration_returns_errno != typed_declaration.declaration_returns_errno()
            || contract.unsafe_block != typed_declaration.unsafe_block()
            || contract.requires_root.as_ref() != typed_declaration.requires_root()
            || contract.ensures_root.as_ref() != typed_declaration.ensures_root()
            || contract.requires.is_some() != typed_declaration.requires_declared()
            || contract.ensures.is_some() != typed_declaration.ensures_declared()
            || typed_declaration.requires_deferred()
            || typed_declaration.ensures_deferred()
        {
            errors.push(format!(
                "{prefix}: checked extern declaration ABI/contract metadata differs"
            ));
        }
        if typed_declaration.block_no_panic()
            || typed_declaration.declaration_no_panic()
            || typed_declaration.block_returns_errno()
            || typed_declaration.declaration_returns_errno()
            || expected_modes
                .iter()
                .any(|mode| *mode != ResolvedFfiParameterMode::Value)
        {
            errors.push(format!(
                "{prefix}: declaration uses no-unwind, errno, or non-Value mode semantics without a canonical consumer receipt"
            ));
        }
        if contract.parameter_types.len() < contract.fixed_argument_count
            || contract.parameter_types[..contract.fixed_argument_count]
                != *typed_declaration.fixed_parameter_types()
        {
            errors.push(format!(
                "{prefix}: fixed TypeDesc prefix differs from checked declaration"
            ));
        }
    } else {
        errors.push(format!("{prefix}: checked extern declaration is absent"));
    }

    if source.arguments().len() != contract.source_arguments.len()
        || source.arguments().len() != actual_arguments.len()
        || contract.parameter_types.len() != actual_arguments.len()
        || contract.parameter_conversions.len() != actual_arguments.len()
    {
        errors.push(format!(
            "{prefix}: ordered source/physical argument census has the wrong arity"
        ));
    }
    for (index, source_argument) in source.arguments().iter().enumerate() {
        let Some(receipt) = contract.source_arguments.get(index) else {
            break;
        };
        if !source_argument_matches(source_argument, receipt) || source_argument.index() != index {
            errors.push(format!(
                "{prefix}: checked source argument {index} changed identity/conversion"
            ));
            continue;
        }
        let expected_value =
            MirValueId::new(format!("expr:{}", source_argument.source_node().0)).ok();
        let Some(actual_argument) = actual_arguments.get(index) else {
            continue;
        };
        if expected_value.as_ref() != Some(actual_argument)
            || function.values.get(actual_argument).map(|value| &value.ty)
                != Some(source_argument.source_type())
            || receipt.conversion_from != *source_argument.conversion_from()
            || receipt.conversion_from != receipt.source_type
        {
            errors.push(format!(
                "{prefix}: source argument {index} does not bind the exact MIR value and TypeDesc"
            ));
            continue;
        }
        if index < contract.fixed_argument_count {
            let (Some(parameter), Some(physical_type)) = (
                contract.fixed_parameter_ids.get(index),
                contract.parameter_types.get(index),
            ) else {
                errors.push(format!(
                    "{prefix}: fixed argument {index} has an incomplete declaration receipt"
                ));
                continue;
            };
            if &receipt.parameter != parameter
                || &receipt.target_type != physical_type
                || source_argument.target_type() != physical_type
            {
                errors.push(format!(
                    "{prefix}: fixed argument {index} differs from its declaration parameter"
                ));
            }
        } else {
            let tail_index = index - contract.fixed_argument_count;
            let Some(tail) = contract.variadic_arguments.get(tail_index) else {
                errors.push(format!(
                    "{prefix}: variadic argument {index} has no promotion receipt"
                ));
                continue;
            };
            if tail.argument_index != index
                || tail.parameter != receipt.parameter
                || tail.source_node != receipt.source_node
                || tail.source_type != receipt.source_type
                || contract.parameter_types.get(index) != Some(&tail.promoted_type)
                || receipt.target_type != receipt.source_type
            {
                errors.push(format!(
                    "{prefix}: variadic argument {index} source/promotion receipt differs"
                ));
            }
        }
    }
    if contract.variadic_arguments.len()
        != contract
            .arguments
            .len()
            .saturating_sub(contract.fixed_argument_count)
    {
        errors.push(format!(
            "{prefix}: variadic promotion receipts do not cover the full tail"
        ));
    }
}

fn typed_scope_matches_source(
    source: &[super::MirProofSourceScopeFrame],
    typed: &[crate::core::ir::TypedProofScopeFrame],
) -> bool {
    use super::MirProofSourceScopeFrame as Source;
    use crate::core::ir::TypedProofScopeFrame as Typed;

    source.len() == typed.len()
        && source
            .iter()
            .zip(typed)
            .all(|(source, typed)| match (source, typed) {
                (Source::Block(left), Typed::Block(right))
                | (Source::Loop(left), Typed::Loop(right))
                | (Source::PlaceInput(left), Typed::PlaceInput(right)) => left == right,
                (
                    Source::Branch {
                        statement: source_statement,
                        expression: source_expression,
                        then_arm: source_arm,
                    },
                    Typed::Branch {
                        statement: typed_statement,
                        expression: typed_expression,
                        then_arm: typed_arm,
                    },
                ) => {
                    source_statement == typed_statement
                        && source_expression == typed_expression
                        && source_arm == typed_arm
                }
                (
                    Source::Default {
                        parameter: source_parameter,
                        expression: source_expression,
                    },
                    Typed::Default {
                        parameter: typed_parameter,
                        expression: typed_expression,
                    },
                ) => source_parameter == typed_parameter && source_expression == typed_expression,
                _ => false,
            })
}

fn source_argument_matches(
    source: &super::MirProofSourceCallArgument,
    receipt: &MirFfiSourceArgument,
) -> bool {
    source.index() == receipt.index
        && source.parameter() == &receipt.parameter
        && source.source_node() == &receipt.source_node
        && source.source_type() == &receipt.source_type
        && source.conversion() == receipt.conversion
        && source.conversion_from() == &receipt.conversion_from
        && source.target_type() == &receipt.target_type
}
