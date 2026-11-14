//! FFI-specific cross-check between checked call/declaration source and the
//! canonical MIR receipts consumed by the native, VM, reference, and verifier
//! backends. The general proof-source census is independent of the FFI table;
//! this adapter joins those two sealed views without rebuilding surface AST.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::ir::lower::{
    ResolvedDeclarationExpressionKind as RootKind, ResolvedDeclarationRootIdentity as RootIdentity,
};
use crate::core::ir::{ResolvedCallee, ResolvedFfiParameterMode};
use crate::core::NodeId;

use super::{
    MirEdgeId, MirFfiCallContract, MirFfiSourceArgument, MirInstructionId, MirInstructionKind,
    MirProofSourceCall, MirProofSourceCensus, MirProofSourceFfiParameterMode, MirValueId,
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

    let executable_call_ids = actual_calls.keys().cloned().collect::<BTreeSet<_>>();
    errors.extend(validate_ffi_source_root_census(
        census,
        &executable_call_ids,
    ));

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

/// Validate the sealed Checker declaration-root inventory without an
/// executable MIR graph. The root census intentionally records unsupported
/// ownership boundaries without creating calls that a backend could execute.
pub(crate) fn validate_ffi_source_root_census(
    census: &MirProofSourceCensus,
    executable_call_ids: &BTreeSet<MirInstructionId>,
) -> Vec<String> {
    let mut errors = census.ffi_source_root_errors().to_vec();
    let mut root_nodes = BTreeSet::new();
    let mut call_nodes = BTreeMap::<NodeId, NodeId>::new();
    for root in census.ffi_source_roots() {
        if !root_nodes.insert(root.root_node().clone()) {
            errors.push(format!(
                "Checker FFI source root '{}' is duplicated in the verifier census",
                root.root_node().0
            ));
        }
        if !root.call_census_valid() {
            errors.push(format!(
                "Checker FFI source root '{}' has no complete direct-call census",
                root.root_node().0
            ));
        }
        if root.body_fingerprint().len() != 64
            || !root
                .body_fingerprint()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            errors.push(format!(
                "Checker FFI source root '{}' has no canonical body fingerprint",
                root.root_node().0
            ));
        }
        if !root.calls().is_empty() && root.origin().is_none() {
            errors.push(format!(
                "Checker FFI source root '{}' has calls but no source origin",
                root.root_node().0
            ));
        }
        let identity_matches = match (root.kind(), root.identity(), root.execution()) {
            (
                RootKind::ConstantInitializer,
                Some(RootIdentity::Constant { declaration }),
                super::MirProofSourceFfiRootExecution::ConstantInitializer,
            ) => declaration == root.owner() && root.activations().is_empty(),
            (
                RootKind::ActorFieldInitializer,
                Some(RootIdentity::ActorField { actor, .. }),
                super::MirProofSourceFfiRootExecution::ActorConstructionPending,
            ) => actor == root.owner() && root.activations().is_empty(),
            (
                RootKind::TraitParameterDefault,
                Some(RootIdentity::TraitParameterDefault { method, .. }),
                super::MirProofSourceFfiRootExecution::TraitDefaultDeclarationOnly,
            ) => method == root.owner() && root.activations().is_empty(),
            (
                RootKind::CallableParameterDefault,
                Some(RootIdentity::CallableParameterDefault { callable, .. }),
                super::MirProofSourceFfiRootExecution::CallableDefaultDeclarationOnly,
            ) => callable == root.owner() && root.activations().is_empty(),
            (
                RootKind::CallableParameterDefault,
                Some(RootIdentity::CallableParameterDefault {
                    callable,
                    parameter,
                }),
                super::MirProofSourceFfiRootExecution::CallableDefaultActivatedUnsupported,
            ) => {
                callable == root.owner()
                    && !root.activations().is_empty()
                    && root.activations().iter().all(|activation| {
                        activation.callee_owner() == callable
                            && activation.parameter() == parameter
                            && census.checked_callable(activation.caller_owner()).is_some()
                    })
            }
            _ => false,
        };
        if !identity_matches {
            errors.push(format!(
                "Checker FFI source root '{}' has a kind/owner/execution identity mismatch",
                root.root_node().0
            ));
        }

        let mut activation_ids = BTreeSet::new();
        for activation in root.activations() {
            let activation_id = (
                activation.caller_owner().clone(),
                activation.call_node().clone(),
                activation.callee_owner().clone(),
                activation.parameter().clone(),
                activation.argument_node().clone(),
                activation.parameter_index(),
                activation.order(),
            );
            if !activation_ids.insert(activation_id) {
                errors.push(format!(
                    "Checker FFI default activation '{}' is duplicated in root '{}'",
                    activation.call_node().0,
                    root.root_node().0
                ));
            }
            if activation.call_node().0.is_empty()
                || activation.argument_node().0.is_empty()
                || activation.scope().iter().any(|frame| match frame {
                    super::MirProofSourceScopeFrame::Block(node)
                    | super::MirProofSourceScopeFrame::Loop(node) => node.0.is_empty(),
                    super::MirProofSourceScopeFrame::Branch {
                        statement,
                        expression,
                        ..
                    } => statement.0.is_empty() || expression.0.is_empty(),
                    super::MirProofSourceScopeFrame::Default {
                        parameter,
                        expression,
                    } => parameter.0 .0.is_empty() || expression.0.is_empty(),
                    super::MirProofSourceScopeFrame::PlaceInput(node) => node.0.is_empty(),
                })
                || activation
                    .guards()
                    .iter()
                    .any(|(condition, _)| condition.0.is_empty())
            {
                errors.push(format!(
                    "Checker FFI default activation '{}' has incomplete call/argument/scope identity",
                    activation.call_node().0
                ));
            }
            if !activation_matches_checked_call(census, root, activation) {
                errors.push(format!(
                    "Checker FFI default activation '{}' is not bound to one exact typed caller Call",
                    activation.call_node().0
                ));
            }
        }

        for call in root.calls() {
            if call.root_node() != root.root_node() {
                errors.push(format!(
                    "Checker FFI source call '{}' is cross-bound to root '{}' instead of '{}'",
                    call.call_node().0,
                    call.root_node().0,
                    root.root_node().0
                ));
            }
            if let Some(previous_root) =
                call_nodes.insert(call.call_node().clone(), root.root_node().clone())
            {
                errors.push(format!(
                    "Checker FFI source call '{}' is duplicated or cross-bound between roots '{}' and '{}'",
                    call.call_node().0,
                    previous_root.0,
                    root.root_node().0
                ));
            }
            let Some(declaration) = census.declaration(call.declaration()) else {
                errors.push(format!(
                    "Checker FFI source call '{}' has no checked extern declaration receipt",
                    call.call_node().0
                ));
                continue;
            };
            if declaration.symbol() != call.symbol() || !declaration.signature_ready() {
                errors.push(format!(
                    "Checker FFI source call '{}' symbol/signature disagrees with declaration '{}'",
                    call.call_node().0,
                    call.declaration().0
                ));
            }
            if call.call_fingerprint().len() != 64
                || !call
                    .call_fingerprint()
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                errors.push(format!(
                    "Checker FFI source call '{}' has no canonical typed-expression fingerprint",
                    call.call_node().0
                ));
            }
            let instruction = MirInstructionId::new(format!("inst:call:{}", call.call_node().0));
            if instruction
                .ok()
                .is_some_and(|instruction| executable_call_ids.contains(&instruction))
            {
                errors.push(format!(
                    "Checker FFI source call '{}' in {:?} root '{}' is duplicated as an executable MIR Call",
                    call.call_node().0,
                    root.execution(),
                    root.root_node().0
                ));
            }
        }
    }
    errors.sort();
    errors.dedup();
    errors
}

fn activation_matches_checked_call(
    census: &MirProofSourceCensus,
    root: &super::MirProofSourceFfiRoot,
    activation: &super::MirProofSourceFfiDefaultActivation,
) -> bool {
    let call_matches = census
        .checked_callable(activation.caller_owner())
        .map(|caller| {
            caller
                .calls()
                .iter()
                .filter(|call| call.call_node() == activation.call_node())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let [call] = call_matches.as_slice() else {
        return false;
    };
    let Some(argument) = call.arguments().get(activation.parameter_index()) else {
        return false;
    };
    let target_default_matches = census
        .checked_callable(activation.callee_owner())
        .is_some_and(|target| {
            target.defaults().iter().any(|default| {
                default.parameter() == activation.parameter()
                    && default.expression() == root.root_node()
            })
        });
    let unique_call_identity_count = census
        .checked_callables()
        .flat_map(|callable| callable.calls())
        .filter(|candidate| candidate.call_node() == activation.call_node())
        .count();
    let declaration_root_call_collision = census
        .ffi_source_roots()
        .iter()
        .flat_map(|source_root| source_root.calls())
        .any(|source_call| source_call.call_node() == activation.call_node());

    unique_call_identity_count == 1
        && !declaration_root_call_collision
        && target_default_matches
        && !call.caller_qualified_name().is_empty()
        && call.has_complete_source_shape()
        && call.callee() == &ResolvedCallee::Function(activation.callee_owner().clone())
        && call.callee_identity() == format!("{:?}", call.callee())
        && call.origin() == Some(activation.origin())
        && argument.index() == activation.parameter_index()
        && argument.parameter() == activation.parameter()
        && argument.source_node() == activation.argument_node()
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
    if let Err(message) = validate_mir_match_path(function, source.call_node(), source.match_path())
    {
        errors.push(format!("{prefix}: {message}"));
    }
    if contract.instruction.as_str() != format!("inst:call:{}", source.call_node().0)
        || contract.call_node != *source.call_node()
        || contract.caller != *caller
        || contract.callee != *declaration
        || contract.declaration != *declaration
        || contract.source_body_owner != *source.body_owner()
        || contract.caller_qualified_name != source.caller_qualified_name()
        || contract.source_statement != *source.statement()
        || source.order() != Some(contract.source_order)
        || !typed_scope_matches_source(source.scope(), &contract.source_scope)
        || source.guards() != contract.source_guards.as_slice()
        || source.match_path() != contract.source_match_path.as_slice()
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
            || contract.requires_deferred != typed_declaration.requires_deferred()
            || contract.ensures_deferred != typed_declaration.ensures_deferred()
            || contract.requires.is_some()
                != (typed_declaration.requires_declared() && !typed_declaration.requires_deferred())
            || contract.ensures.is_some()
                != (typed_declaration.ensures_declared() && !typed_declaration.ensures_deferred())
            || &contract.requires_types != typed_declaration.requires_types()
            || &contract.ensures_types != typed_declaration.ensures_types()
        {
            errors.push(format!(
                "{prefix}: checked extern declaration ABI/contract metadata differs"
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
        let eager_nodes = source_argument.eager_source_nodes();
        if !eager_nodes.contains(source_argument.source_node())
            || eager_nodes.windows(2).any(|pair| pair[0] >= pair[1])
        {
            errors.push(format!(
                "{prefix}: checked source argument {index} has an incomplete or noncanonical eager-node provenance set"
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
            let Some(parameter) = contract.fixed_parameter_ids.get(index) else {
                errors.push(format!(
                    "{prefix}: fixed argument {index} has an incomplete declaration receipt"
                ));
                continue;
            };
            // A fixed FFI input has two distinct type edges.  The checked
            // source argument conversion is Identity and therefore ends at
            // the source expression's own TypeDesc.  The physical C ABI
            // conversion from that source TypeDesc to the declaration's
            // parameter TypeDesc is carried separately by
            // `parameter_conversions` and validated against `parameter_types`
            // by `validate_ffi_call_contract_receipt`.  Comparing the checked
            // source conversion target directly to the declaration TypeDesc
            // rejects valid i32 -> i64 (and other admitted ABI) conversions.
            if &receipt.parameter != parameter
                || receipt.target_type != receipt.source_type
                || source_argument.target_type() != &receipt.target_type
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

fn validate_mir_match_path(
    function: &super::MirFunction,
    call_node: &NodeId,
    path: &[crate::core::ir::TypedMatchPathReceipt],
) -> Result<(), String> {
    use super::{MirSwitchCase, MirTerminator};
    use crate::core::ir::{PrimitiveType, TypedMatchCase};

    if path.is_empty() {
        return Ok(());
    }
    let expected_call = MirInstructionId::new(format!("inst:call:{}", call_node.0))
        .map_err(|error| error.to_string())?;
    let mut call_blocks = function
        .blocks
        .values()
        .filter(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| instruction.id == expected_call)
        })
        .map(|block| block.id.clone());
    let call_block = call_blocks
        .next()
        .ok_or_else(|| "match path call NodeId has no MIR instruction".to_owned())?;
    if call_blocks.next().is_some() {
        return Err("match path call NodeId occurs in multiple MIR blocks".into());
    }
    let (reachable, dominators) = mir_reachable_and_dominators(function)?;
    if !reachable.contains(&call_block) {
        return Err("match path call NodeId is unreachable in the MIR CFG".into());
    }

    let mut enclosing_arm_targets = Vec::with_capacity(path.len());
    let mut seen_match_expressions = BTreeSet::new();
    let mut seen_cfg_dispatches = BTreeSet::new();
    for receipt in path {
        if receipt.arms.len() < 2
            || receipt.call_node.as_ref() != Some(call_node)
            || !seen_match_expressions.insert(receipt.match_expression.clone())
            || !seen_cfg_dispatches.insert(receipt.cfg_dispatch.clone())
            || receipt.match_expression.0.is_empty()
            || receipt.scrutinee_expression.0.is_empty()
            || receipt.scrutinee_local.0 .0.is_empty()
            || receipt.cfg_dispatch.0 .0.is_empty()
            || receipt.selected_arm.0.is_empty()
            || receipt.ownership_state_fingerprint == [0; 32]
        {
            return Err("match path receipt is incomplete".into());
        }
        let is_i32 =
            receipt.scrutinee_type == crate::core::ir::primitive_type_id(PrimitiveType::I32);
        let is_i64 =
            receipt.scrutinee_type == crate::core::ir::primitive_type_id(PrimitiveType::I64);
        if !is_i32 && !is_i64 {
            return Err("match path scrutinee TypeDesc is not signed i32/i64".into());
        }
        let mut source_arms = BTreeSet::new();
        let mut source_edges = BTreeSet::new();
        let mut source_targets = BTreeSet::new();
        let mut source_literals = BTreeSet::new();
        let mut selected_arms = 0usize;
        for (index, arm) in receipt.arms.iter().enumerate() {
            if !source_arms.insert(arm.arm.clone())
                || !source_edges.insert(arm.cfg_edge.clone())
                || !source_targets.insert(arm.cfg_target.clone())
            {
                return Err("match path Checker arm/edge/target identities are not unique".into());
            }
            let expected_cfg_edge = crate::core::cfg::EdgeId(crate::core::NodeId(format!(
                "{}/cfg:edge:match-arm",
                arm.arm.0
            )));
            let expected_cfg_target = crate::core::cfg::BasicBlockId(crate::core::NodeId(format!(
                "{}/cfg:block:match-arm",
                arm.arm.0
            )));
            if arm.cfg_edge != expected_cfg_edge || arm.cfg_target != expected_cfg_target {
                return Err(
                    "match path Checker edge/target is not the exact arm entry identity".into(),
                );
            }
            match &arm.case {
                TypedMatchCase::Integer(value)
                    if index + 1 < receipt.arms.len()
                        && (!is_i32 || i32::try_from(*value).is_ok())
                        && source_literals.insert(*value) => {}
                TypedMatchCase::Wildcard if index + 1 == receipt.arms.len() => {}
                _ => {
                    return Err(
                        "match path is not a unique literal list with a final wildcard".into(),
                    );
                }
            }
            selected_arms += usize::from(arm.arm == receipt.selected_arm);
        }
        if selected_arms != 1 {
            return Err("match path selected Checker arm is not unique".into());
        }
        let selector = MirValueId::new(format!("expr:{}", receipt.scrutinee_expression.0))
            .map_err(|error| error.to_string())?;
        let local = MirValueId::new(format!("local:{}", receipt.scrutinee_local.0 .0))
            .map_err(|error| error.to_string())?;
        if function.values.get(&selector).map(|value| &value.ty) != Some(&receipt.scrutinee_type) {
            return Err("match path scrutinee MIR value identity or TypeDesc differs".into());
        }
        let selector_producers = function
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| match &instruction.kind {
                MirInstructionKind::Clone { result, source } => {
                    result == &selector && source == &local
                }
                _ => false,
            })
            .count();
        if selector_producers != 1 {
            return Err("match path scrutinee is not one direct scalar local Load in MIR".into());
        }

        let expected_arm_edges = receipt
            .arms
            .iter()
            .map(|arm| {
                MirEdgeId::new(format!("edge:match.arm:{}", arm.arm.0))
                    .map(|edge| (edge, arm))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let candidates = function
            .blocks
            .values()
            .filter(|block| match &block.terminator {
                MirTerminator::Switch { arms, .. } => arms.iter().any(|actual| {
                    expected_arm_edges
                        .iter()
                        .any(|(expected, _)| expected == &actual.edge)
                }),
                _ => false,
            })
            .collect::<Vec<_>>();
        let [switch_block] = candidates.as_slice() else {
            return Err("match path does not map to one MIR Switch dispatch".into());
        };
        let MirTerminator::Switch {
            scrutinee: actual_selector,
            arms: actual_arms,
        } = &switch_block.terminator
        else {
            unreachable!("candidate switch was filtered by terminator kind")
        };
        if actual_selector != &selector || actual_arms.len() != receipt.arms.len() {
            return Err("match path MIR Switch selector or complete arm count differs".into());
        }

        let mut selected_target = None;
        let mut actual_targets = BTreeSet::new();
        for ((actual, (expected_edge, source_arm)), source_arm_receipt) in actual_arms
            .iter()
            .zip(&expected_arm_edges)
            .zip(&receipt.arms)
        {
            let expected_target =
                super::MirBlockId::new(format!("bb:match.arm:{}", source_arm.arm.0))
                    .map_err(|error| error.to_string())?;
            let case_matches = match (&source_arm_receipt.case, &actual.case) {
                (
                    TypedMatchCase::Integer(expected),
                    MirSwitchCase::Literal(crate::core::ir::ResolvedLiteral::Int(actual)),
                ) => expected == actual,
                (TypedMatchCase::Wildcard, MirSwitchCase::Default) => true,
                _ => false,
            };
            if &actual.edge != expected_edge
                || actual.target != expected_target
                || !case_matches
                || !actual.arguments.is_empty()
                || !actual.bindings.is_empty()
                || !actual_targets.insert(actual.target.clone())
                || !function.blocks.contains_key(&actual.target)
            {
                return Err(
                    "match path MIR Switch edge/target/case differs from the checked arm receipt"
                        .into(),
                );
            }
            if source_arm.arm != receipt.selected_arm {
                continue;
            }
            if !matches!(&source_arm.case, TypedMatchCase::Integer(_)) || selected_target.is_some()
            {
                return Err("match path selected arm is not one unique integer literal arm".into());
            }
            selected_target = Some(actual.target.clone());
        }
        let selected_target = selected_target
            .ok_or_else(|| "match path selected arm is absent from the MIR Switch".to_owned())?;
        let switch_dominators = dominators
            .get(&selected_target)
            .ok_or_else(|| "match path selected MIR arm target is unreachable".to_owned())?;
        if !switch_dominators.contains(&switch_block.id) {
            return Err("match path MIR Switch does not dominate its selected arm target".into());
        }
        if enclosing_arm_targets.iter().any(|outer| {
            !dominators
                .get(&switch_block.id)
                .is_some_and(|set| set.contains(outer))
        }) {
            return Err("nested match dispatch is outside its checked enclosing arm".into());
        }
        enclosing_arm_targets.push(selected_target);
    }
    if enclosing_arm_targets.iter().any(|target| {
        !dominators
            .get(&call_block)
            .is_some_and(|set| set.contains(target))
    }) {
        return Err("selected MIR match arm does not dominate the exact FFI call NodeId".into());
    }
    Ok(())
}

fn mir_reachable_and_dominators(
    function: &super::MirFunction,
) -> Result<
    (
        BTreeSet<super::MirBlockId>,
        BTreeMap<super::MirBlockId, BTreeSet<super::MirBlockId>>,
    ),
    String,
> {
    use super::MirTerminator;

    if !function.blocks.contains_key(&function.entry) {
        return Err("match path MIR CFG has no entry block".into());
    }
    let mut reachable = BTreeSet::new();
    let mut stack = vec![function.entry.clone()];
    let mut predecessors = BTreeMap::<super::MirBlockId, BTreeSet<super::MirBlockId>>::new();
    while let Some(block_id) = stack.pop() {
        if !reachable.insert(block_id.clone()) {
            continue;
        }
        let block = function
            .blocks
            .get(&block_id)
            .ok_or_else(|| format!("match path MIR successor block '{block_id}' is absent"))?;
        let targets = match &block.terminator {
            MirTerminator::Goto { target, .. } => vec![target.clone()],
            MirTerminator::Branch {
                then_target,
                else_target,
                ..
            } => vec![then_target.clone(), else_target.clone()],
            MirTerminator::Switch { arms, .. } | MirTerminator::SwitchMove { arms, .. } => {
                arms.iter().map(|arm| arm.target.clone()).collect()
            }
            MirTerminator::Return { .. }
            | MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => Vec::new(),
        };
        for target in targets {
            if !function.blocks.contains_key(&target) {
                return Err(format!(
                    "match path MIR edge targets absent block '{target}'"
                ));
            }
            predecessors
                .entry(target.clone())
                .or_default()
                .insert(block_id.clone());
            if !reachable.contains(&target) {
                stack.push(target);
            }
        }
    }

    let mut dominators = reachable
        .iter()
        .map(|block| {
            let initial = if block == &function.entry {
                BTreeSet::from([function.entry.clone()])
            } else {
                reachable.clone()
            };
            (block.clone(), initial)
        })
        .collect::<BTreeMap<_, _>>();
    loop {
        let mut changed = false;
        for block in reachable.iter().filter(|block| *block != &function.entry) {
            let incoming = predecessors
                .get(block)
                .into_iter()
                .flatten()
                .filter_map(|predecessor| dominators.get(predecessor));
            let mut common: Option<BTreeSet<super::MirBlockId>> = None;
            for predecessor_dominators in incoming {
                if let Some(common) = &mut common {
                    common.retain(|candidate| predecessor_dominators.contains(candidate));
                } else {
                    common = Some(predecessor_dominators.clone());
                }
            }
            let mut next = common.ok_or_else(|| {
                format!("reachable match path MIR block '{block}' has no predecessor")
            })?;
            next.insert(block.clone());
            if dominators.get(block) != Some(&next) {
                dominators.insert(block.clone(), next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok((reachable, dominators))
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
        && source.eager_source_nodes() == receipt.eager_source_nodes.as_slice()
}

#[cfg(test)]
mod match_path_tests {
    use super::*;
    use crate::core::ir::TypedMatchCase;

    #[test]
    fn mir_match_path_rejects_stale_case_and_wrong_callsite_receipts() {
        let source = r#"
            extern "C" {
                func foreign(value: i32) -> i32 requires: value > 0;
            }
            func caller(value: i32) -> i32 {
                match value {
                    1 => foreign(value),
                    _ => 0
                }
            }
            func main() -> i64 { 0 }
        "#;
        let tokens = crate::lexer::Lexer::new(source)
            .tokenize()
            .expect("tokenize MIR match path fixture");
        let file = crate::parser::Parser::new_memory(
            tokens,
            "mir-ffi-match-path-test",
            "mir-ffi-match-path-test",
            source,
        )
        .expect("create MIR match path parser")
        .parse_file()
        .expect("parse MIR match path fixture");
        let checked = crate::core::check_program(&file).expect("check MIR match path fixture");
        let program = MirProgram::from_checked_program(&checked).expect("lower canonical MIR");
        let owner = NodeId("function:caller".into());
        let source_call = program
            .proof_source_census()
            .owner(&owner)
            .expect("checked caller source census")
            .body_calls()
            .find(|call| !call.match_path().is_empty())
            .expect("literal-arm FFI call receipt");
        let function = program
            .functions()
            .get(&owner)
            .expect("MIR caller function");
        let mut receipt = source_call.match_path().to_vec();
        assert!(validate_mir_match_path(function, source_call.call_node(), &receipt).is_ok());

        let selected_arm = receipt[0].selected_arm.clone();
        let selected = receipt[0]
            .arms
            .iter_mut()
            .find(|arm| arm.arm == selected_arm)
            .expect("selected integer arm");
        let TypedMatchCase::Integer(value) = &mut selected.case else {
            panic!("selected arm must be a literal");
        };
        *value = value.wrapping_add(1);
        assert!(
            validate_mir_match_path(function, source_call.call_node(), &receipt).is_err(),
            "a changed source literal must not match the actual MIR Switch case"
        );

        receipt = source_call.match_path().to_vec();
        receipt[0].call_node = Some(NodeId("function:neighbor-call".into()));
        assert!(
            validate_mir_match_path(function, source_call.call_node(), &receipt).is_err(),
            "an arm receipt cannot be replayed at a different call NodeId"
        );
    }
}
