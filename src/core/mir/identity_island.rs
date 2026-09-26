//! Narrow whole-program route for one concrete scalar generic identity call.
//!
//! MIR already materializes generic identity instances for several consumers.
//! These profiles admit only `main() -> i32/i64 { identity(literal) }`-shaped
//! programs: one direct generic identity callable, one matching concrete
//! scalar specialization, and no other user executable callable. The checker
//! proof and MIR proof are independent; this module does not infer identity
//! from a symbol spelling or backend ABI.

use crate::core::ir::{
    ResolvedCallee, ResolvedExprKind, ResolvedLiteral, ResolvedType, ResolvedTypeId,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirAbiClass, MirLayout, MirTypeKind};
use crate::core::{CheckedProgram, NodeId, PrimitiveType};

use super::{MirGenericInstanceContract, MirInstructionKind, MirTerminator};

pub const SCALAR_GENERIC_IDENTITY_I32_ISLAND: &str = "generic-scalar-identity-i32-v1";
pub const SCALAR_GENERIC_IDENTITY_I64_ISLAND: &str = "generic-scalar-identity-i64-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarGenericIdentityAdmission {
    OutsideProfile,
    CompleteCoverage,
}

/// Admit a generic identity specialization using only checker-owned facts.
///
/// The source-level callable may have any name. Its signature, body, call
/// target, inferred type argument, literal argument, and complete user
/// callable set must all match the bounded i32 profile.
pub fn classify_scalar_generic_identity_admission(
    program: &CheckedProgram,
) -> ScalarGenericIdentityAdmission {
    classify_scalar_generic_identity_admission_for(program, PrimitiveType::I32)
}

pub fn classify_scalar_generic_identity_i64_admission(
    program: &CheckedProgram,
) -> ScalarGenericIdentityAdmission {
    classify_scalar_generic_identity_admission_for(program, PrimitiveType::I64)
}

fn classify_scalar_generic_identity_admission_for(
    program: &CheckedProgram,
    primitive: PrimitiveType,
) -> ScalarGenericIdentityAdmission {
    let main_id = NodeId("function:main".into());
    let Some(main) = program.callable(&main_id) else {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    };
    if is_prelude_callable(program, main)
        || !main.signature.generic_parameters.is_empty()
        || !main.signature.parameters.is_empty()
        || !main.signature.effects.is_empty()
        || !main.body.captures.is_empty()
        || !main.body.default_values.is_empty()
        || !main.contracts.is_empty()
        || !main.body.root.statements.is_empty()
    {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }
    let Some(scalar_id) = checked_scalar_type(program, primitive) else {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    };
    if main.signature.result != scalar_id {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }
    let Some(expression) = main.body.root.result.as_deref() else {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    };
    if expression.ty != scalar_id {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }
    let ResolvedExprKind::Call(call) = &expression.kind else {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    };
    let ResolvedCallee::Function(template) = &call.callee else {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    };
    let Some(identity) = program.callable(template) else {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    };
    if template == &main_id || !is_direct_generic_identity(program, identity) {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }
    if call.result != scalar_id
        || call.type_arguments.as_slice() != [scalar_id.clone()]
        || call.arguments.len() != 1
        || call.permission.is_some()
        || !call.effects.is_empty()
        || !call.session.is_empty()
    {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }
    let Some(argument) = call.arguments.first() else {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    };
    if argument.value.ty != scalar_id
        || argument.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
        || argument.conversion.from != scalar_id
        || argument.conversion.to != scalar_id
        || !matches!(
            argument.value.kind,
            ResolvedExprKind::Literal(ResolvedLiteral::Int(_))
        )
    {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }

    let user_callables = program
        .callables()
        .values()
        .filter(|callable| !is_prelude_callable(program, callable))
        .collect::<Vec<_>>();
    let user_functions = program
        .functions()
        .values()
        .filter(|function| !is_prelude_origin(program, &function.origin))
        .collect::<Vec<_>>();
    if user_callables.len() != 2
        || user_functions.len() != 2
        || !user_callables
            .iter()
            .any(|callable| callable.owner == main_id)
        || !user_callables
            .iter()
            .any(|callable| callable.owner == *template)
        || !user_functions
            .iter()
            .any(|function| function.node_id == main_id)
        || !user_functions
            .iter()
            .any(|function| function.node_id == *template)
    {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }

    ScalarGenericIdentityAdmission::CompleteCoverage
}

fn checked_scalar_type(
    program: &CheckedProgram,
    primitive: PrimitiveType,
) -> Option<ResolvedTypeId> {
    program.resolved_types().iter().find_map(|(id, ty)| {
        matches!(ty, ResolvedType::Primitive(found) if *found == primitive).then(|| id.clone())
    })
}

fn is_direct_generic_identity(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    let [binder] = callable.signature.generic_parameters.as_slice() else {
        return false;
    };
    let [parameter] = callable.signature.parameters.as_slice() else {
        return false;
    };
    let [body_parameter] = callable.body.parameters.as_slice() else {
        return false;
    };
    if !matches!(
        program.resolved_types().get(&parameter.ty),
        Some(ResolvedType::GenericParameter(found)) if found == binder
    ) || parameter.ty != callable.signature.result
        || parameter.mutable
        || parameter.permission.is_some()
        || parameter.has_default
        || !callable.signature.effects.is_empty()
        || !callable.body.captures.is_empty()
        || !callable.body.default_values.is_empty()
        || !callable.contracts.is_empty()
        || !callable.body.root.statements.is_empty()
    {
        return false;
    }
    callable.body.root.result.as_deref().is_some_and(|result| {
        result.ty == parameter.ty
            && matches!(
                &result.kind,
                ResolvedExprKind::Load(place)
                    if place.base == *body_parameter && place.projections.is_empty()
            )
    })
}

fn is_prelude_callable(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> bool {
    is_prelude_origin(program, &callable.body.root.origin)
}

fn is_prelude_origin(program: &CheckedProgram, origin: &crate::core::Origin) -> bool {
    program
        .source_registry()
        .key(origin.user_span().source_id)
        .is_some_and(|key| key.as_str() == "stdlib:prelude.mimi")
}

/// Re-prove the complete executable graph after canonical construction.
/// This rejects extra entry operations, instances, functions, altered ABI
/// facts, or any unsupported body shape before a backend sees the program.
pub fn validate_scalar_generic_identity_island(program: &MirProgram) -> Result<(), Vec<String>> {
    validate_scalar_generic_identity_island_for(
        program,
        PrimitiveType::I32,
        SCALAR_GENERIC_IDENTITY_I32_ISLAND,
    )
}

pub fn validate_scalar_generic_identity_i64_island(
    program: &MirProgram,
) -> Result<(), Vec<String>> {
    validate_scalar_generic_identity_island_for(
        program,
        PrimitiveType::I64,
        SCALAR_GENERIC_IDENTITY_I64_ISLAND,
    )
}

fn validate_scalar_generic_identity_island_for(
    program: &MirProgram,
    primitive: PrimitiveType,
    island: &str,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    let main_id = NodeId("function:main".into());
    if program.functions().len() != 2 {
        errors.push(format!(
            "{island} requires exactly main and one identity instance, got {} functions",
            program.functions().len()
        ));
    }
    if !program.transitions().is_empty() {
        errors.push(format!("{island} does not admit Flow transitions"));
    }
    if !program.ffi_calls().is_empty() {
        errors.push(format!("{island} does not admit FFI calls"));
    }
    let instances = program.instances().values().collect::<Vec<_>>();
    let [instance] = instances.as_slice() else {
        errors.push(format!(
            "{island} requires exactly one generic instance, got {}",
            instances.len()
        ));
        return Err(errors);
    };
    let [argument] = instance.arguments.as_slice() else {
        errors.push(format!(
            "{island} identity instance requires exactly one type argument"
        ));
        return Err(errors);
    };
    if instance.contract != MirGenericInstanceContract::ScalarIdentity {
        errors.push(format!("{island} requires the ScalarIdentity contract"));
    }
    let Some(descriptor) = program.type_catalog().get(argument) else {
        errors.push(format!("{island} identity argument TypeDesc is absent"));
        return Err(errors);
    };
    let bits = match primitive {
        PrimitiveType::I32 => 32,
        PrimitiveType::I64 => 64,
        _ => {
            return Err(vec![format!(
                "{island} internal route requested an unsupported integer primitive"
            )]);
        }
    };
    if descriptor.kind != MirTypeKind::Primitive(primitive)
        || descriptor.layout != MirLayout::Scalar
        || descriptor.abi != (MirAbiClass::Integer { bits, signed: true })
        || descriptor.ownership != crate::core::mir::types::MirOwnership::Copy
        || descriptor.needs_drop_glue
        || descriptor.needs_clone_glue
    {
        errors.push(format!("{island} requires a Copy {primitive:?} TypeDesc"));
    }
    if program
        .type_catalog()
        .validate_copy_scalar(argument)
        .is_err()
    {
        errors.push(format!(
            "{island} identity argument is outside the Copy scalar contract"
        ));
    }

    let Some(main) = program.functions().get(&main_id) else {
        errors.push(format!("{island} is missing canonical function:main"));
        return Err(errors);
    };
    if main.result != *argument
        || !main.parameters.is_empty()
        || !main.contracts.is_empty()
        || main.blocks.len() != 1
        || main.values.len() != 2
    {
        errors.push(format!(
            "{island} main ABI/body is outside the closed shape"
        ));
    }
    let Some(block) = main.blocks.get(&main.entry) else {
        errors.push(format!("{island} main entry block is absent"));
        return Err(errors);
    };
    if !block.parameters.is_empty() {
        errors.push(format!(
            "{island} main entry block has unexpected parameters"
        ));
    }
    let [constant, call] = block.instructions.as_slice() else {
        errors.push(format!("{island} main must contain exactly Const and Call"));
        return Err(errors);
    };
    let MirInstructionKind::Const {
        result: argument_value,
        literal: ResolvedLiteral::Int(value),
    } = &constant.kind
    else {
        errors.push(format!(
            "{island} main argument must be an in-range {primitive:?} integer literal"
        ));
        return Err(errors);
    };
    if primitive == PrimitiveType::I32 && i32::try_from(*value).is_err() {
        errors.push(format!(
            "{island} main integer literal is outside the i32 range"
        ));
        return Err(errors);
    }
    let MirInstructionKind::Call {
        result: Some(call_result),
        callee: ResolvedCallee::Function(callee),
        type_arguments,
        arguments,
        effect_receipts,
        variant_call_contract,
    } = &call.kind
    else {
        errors.push(format!(
            "{island} main must call its materialized identity instance"
        ));
        return Err(errors);
    };
    if callee != &instance.function
        || type_arguments.as_slice() != [argument.clone()]
        || arguments.as_slice() != [argument_value.clone()]
        || !effect_receipts.is_empty()
        || variant_call_contract.is_some()
        || !main
            .values
            .get(argument_value)
            .is_some_and(|value| value.ty == *argument)
        || !main
            .values
            .get(call_result)
            .is_some_and(|value| value.ty == *argument)
        || !matches!(
            &block.terminator,
            MirTerminator::Return { value: Some(value) } if value == call_result
        )
    {
        errors.push(format!(
            "{island} call target, arguments, result, or return disagree"
        ));
    }
    let Some(target) = program.functions().get(&instance.function) else {
        errors.push(format!("{island} identity target is absent"));
        return Err(errors);
    };
    if !target.contracts.is_empty() {
        errors.push(format!("{island} identity target does not admit contracts"));
    }
    if let Err(message) = super::validate_generic_identity_shape(target, argument) {
        errors.push(format!(
            "{island} identity target shape rejected: {message}"
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Return whether canonical MIR contains a generic scalar identity instance.
/// The route validator separately narrows its concrete type and full graph.
pub fn contains_scalar_generic_identity_candidate(program: &MirProgram) -> bool {
    program
        .instances()
        .values()
        .any(|instance| instance.contract == MirGenericInstanceContract::ScalarIdentity)
}
