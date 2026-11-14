//! Narrow whole-program route for one concrete scalar generic identity call.
//!
//! MIR already materializes generic identity instances for several consumers.
//! These profiles admit only `main() -> i32/i64 { identity(literal) }`-shaped
//! programs: one direct generic identity callable (or its exact parameter
//! projection, one-local alias, or inert bool/integer-local form), one matching
//! concrete scalar specialization, and no other user executable callable.
//! The checker proof and MIR proof are independent; this module does not infer identity
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
    let argument_projection = scalar_generic_argument_projection(program, identity);
    let direct_identity = is_direct_generic_identity(program, identity);
    if template == &main_id || !(direct_identity || argument_projection.is_some()) {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }
    if call.result != scalar_id
        || call.type_arguments.as_slice() != [scalar_id.clone()]
        || call.arguments.len() != argument_projection.map_or(1, |(_, count)| count as usize)
        || call.permission.is_some()
        || !call.effects.is_empty()
        || !call.session.is_empty()
    {
        return ScalarGenericIdentityAdmission::OutsideProfile;
    }
    if call.arguments.iter().any(|argument| {
        argument.value.ty != scalar_id
            || argument.conversion.kind != crate::core::ir::CheckedConversionKind::Identity
            || argument.conversion.from != scalar_id
            || argument.conversion.to != scalar_id
            || !matches!(
                argument.value.kind,
                ResolvedExprKind::Literal(ResolvedLiteral::Int(_))
            )
    }) {
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

/// Derive the selected formal parameter from Checker-owned signature and body
/// identities. Every parameter must be the same generic T and the body must
/// either return an unprojected load of exactly one formal, bind one
/// immutable local directly from a formal and return that local, or bind one
/// unused immutable bool or i32/i64 integer literal before returning an
/// unprojected formal.
/// Concrete specialization is separately restricted to Copy scalars.
pub fn scalar_generic_argument_projection(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
) -> Option<(u32, u32)> {
    let [binder] = callable.signature.generic_parameters.as_slice() else {
        return None;
    };
    let generic_id = program.resolved_types().iter().find_map(|(id, ty)| {
        matches!(ty, ResolvedType::GenericParameter(found) if found == binder).then_some(id)
    })?;
    let parameters = &callable.signature.parameters;
    if parameters.len() < 2
        || parameters.len() != callable.body.parameters.len()
        || callable.signature.result != *generic_id
        || !callable.signature.effects.is_empty()
        || !callable.body.captures.is_empty()
        || !callable.body.default_values.is_empty()
        || !callable.contracts.is_empty()
    {
        return None;
    }
    let mut formal_ids = std::collections::BTreeSet::new();
    for (parameter, body_parameter) in parameters.iter().zip(&callable.body.parameters) {
        if parameter.ty != *generic_id
            || parameter.mutable
            || parameter.permission.is_some()
            || parameter.has_default
            || body_parameter.0 .0 != format!("{}/local", parameter.id.0 .0)
            || !formal_ids.insert(body_parameter)
        {
            return None;
        }
    }
    let result = callable.body.root.result.as_deref()?;
    let ResolvedExprKind::Load(result_place) = &result.kind else {
        return None;
    };
    if result.ty != *generic_id || !result_place.projections.is_empty() {
        return None;
    }
    if let Some(index) = callable
        .body
        .parameters
        .iter()
        .position(|formal| *formal == result_place.base)
    {
        let expected_local_count = if callable.body.root.statements.is_empty() {
            callable.body.parameters.len()
        } else if callable.body.root.statements.len() == 1
            && is_ignored_literal_binding(program, callable, &callable.body.root.statements[0])
        {
            callable.body.parameters.len() + 1
        } else {
            return None;
        };
        if callable.body.locals.len() != expected_local_count {
            return None;
        }
        let count = u32::try_from(parameters.len()).ok()?;
        super::validate_generic_argument_projection_arity(count).ok()?;
        return Some((u32::try_from(index).ok()?, count));
    }

    // Admit only the one statement form produced by `let chosen = right;
    // chosen`: a direct, immutable binding from one formal followed by an
    // unprojected load of that exact local. This preserves the Checker-owned
    // lexical identity and keeps arbitrary generic expressions outside this
    // profile.
    let [statement] = callable.body.root.statements.as_slice() else {
        return None;
    };
    let crate::core::ir::ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(initializer),
    } = &statement.kind
    else {
        return None;
    };
    let crate::core::ir::ResolvedPatternKind::Binding {
        local: alias_local,
        by_reference: None,
    } = &pattern.kind
    else {
        return None;
    };
    if result_place.base != *alias_local
        || pattern.ty != *generic_id
        || !statement.backend_requirements.is_empty()
        || initializer.ty != *generic_id
        || !initializer.effects.is_empty()
        || !initializer.backend_requirements.is_empty()
        || !result.effects.is_empty()
        || !result.backend_requirements.is_empty()
        || callable.body.parameters.contains(alias_local)
        || callable.body.captures.contains(alias_local)
        || callable.body.locals.len() != callable.body.parameters.len() + 1
    {
        return None;
    }
    let alias_definition = callable.body.locals.get(alias_local)?;
    if alias_definition.ty != *generic_id || alias_definition.mutable {
        return None;
    }
    let ResolvedExprKind::Load(initializer_place) = &initializer.kind else {
        return None;
    };
    if initializer_place.projections.len() != 0 {
        return None;
    }
    let index = callable
        .body
        .parameters
        .iter()
        .position(|formal| *formal == initializer_place.base)?;
    let count = u32::try_from(parameters.len()).ok()?;
    super::validate_generic_argument_projection_arity(count).ok()?;
    Some((u32::try_from(index).ok()?, count))
}

/// Accept the one effect-free, unused side binding required by the current
/// generic projection slice. Checking both the literal and its Copy scalar
/// resolved type keeps calls, mutable locals, and generic-valued moves outside
/// this receipt profile.
fn is_ignored_literal_binding(
    program: &CheckedProgram,
    callable: &crate::core::ir::ResolvedCallable,
    statement: &crate::core::ir::ResolvedStmt,
) -> bool {
    let crate::core::ir::ResolvedStmtKind::Bind {
        pattern,
        initializer: Some(initializer),
    } = &statement.kind
    else {
        return false;
    };
    let crate::core::ir::ResolvedPatternKind::Binding {
        local,
        by_reference: None,
    } = &pattern.kind
    else {
        return false;
    };
    let literal_matches_copy_scalar_type = matches!(
        (program.resolved_types().get(&pattern.ty), &initializer.kind),
        (
            Some(ResolvedType::Primitive(PrimitiveType::Bool)),
            ResolvedExprKind::Literal(ResolvedLiteral::Bool(_)),
        ) | (
            Some(ResolvedType::Primitive(
                PrimitiveType::I32 | PrimitiveType::I64
            )),
            ResolvedExprKind::Literal(ResolvedLiteral::Int(_)),
        )
    );
    if !literal_matches_copy_scalar_type
        || initializer.ty != pattern.ty
        || !statement.backend_requirements.is_empty()
        || !initializer.effects.is_empty()
        || !initializer.backend_requirements.is_empty()
        || callable.body.parameters.contains(local)
        || callable.body.captures.contains(local)
    {
        return false;
    }
    callable.body.locals.get(local).is_some_and(|definition| {
        definition.id == *local && definition.ty == pattern.ty && !definition.mutable
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
    let projection = match instance.contract {
        MirGenericInstanceContract::ScalarIdentity => None,
        MirGenericInstanceContract::ScalarArgumentProjection {
            parameter_index,
            parameter_count,
        } if super::validate_generic_argument_projection_arity(parameter_count).is_ok()
            && parameter_index < parameter_count =>
        {
            Some((parameter_index, parameter_count))
        }
        _ => {
            errors.push(format!(
                "{island} requires the ScalarIdentity or exact argument projection contract"
            ));
            return Err(errors);
        }
    };
    let expected_argument_count = projection.map_or(1, |(_, count)| count as usize);
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
        || main.values.len() != expected_argument_count + 1
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
    if block.instructions.len() != expected_argument_count + 1 {
        errors.push(format!(
            "{island} main must contain exactly {expected_argument_count} Const instructions and one Call"
        ));
        return Err(errors);
    }
    let (literal_instructions, calls) = block.instructions.split_at(expected_argument_count);
    let call_instruction = &calls[0];
    let mut argument_values = Vec::with_capacity(literal_instructions.len());
    for constant in literal_instructions {
        let MirInstructionKind::Const {
            result,
            literal: ResolvedLiteral::Int(value),
        } = &constant.kind
        else {
            errors.push(format!(
                "{island} main arguments must be in-range {primitive:?} integer literals"
            ));
            return Err(errors);
        };
        if primitive == PrimitiveType::I32 && i32::try_from(*value).is_err() {
            errors.push(format!(
                "{island} main integer literal is outside the i32 range"
            ));
            return Err(errors);
        }
        if !main
            .values
            .get(result)
            .is_some_and(|value| value.ty == *argument)
        {
            errors.push(format!(
                "{island} main literal TypeDesc disagrees with its generic substitution"
            ));
        }
        argument_values.push(result.clone());
    }
    let MirInstructionKind::Call {
        result: Some(call_result),
        callee: ResolvedCallee::Function(callee),
        type_arguments,
        arguments,
        effect_receipts,
        variant_call_contract,
    } = &call_instruction.kind
    else {
        errors.push(format!(
            "{island} main must call its materialized identity instance"
        ));
        return Err(errors);
    };
    if callee != &instance.function
        || type_arguments.as_slice() != [argument.clone()]
        || arguments != &argument_values
        || arguments.len() != expected_argument_count
        || !effect_receipts.is_empty()
        || variant_call_contract.is_some()
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
    let target_shape = if let Some((index, count)) = projection {
        super::validate_generic_argument_projection_shape(target, argument, index, count)
    } else {
        super::validate_generic_identity_shape(target, argument)
    };
    if let Err(message) = target_shape {
        errors.push(format!("{island} generic target shape rejected: {message}"));
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
    program.instances().values().any(|instance| {
        matches!(
            instance.contract,
            MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::ScalarArgumentProjection { .. }
        )
    })
}
