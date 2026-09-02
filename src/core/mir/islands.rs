//! Whole-program Canonical MIR production-island contracts.
//!
//! This module contains route eligibility, not backend lowering.  The first
//! island is intentionally narrower than the individual List/Set adapters:
//! every executable function in the materialized MIR graph must use only
//! Copy scalar values, move-owned scalar Lists/Sets, synchronous scalar CFG,
//! and canonical scalar calls.  The checker type catalog may contain many
//! unrelated declarations; only types and operations that actually cross the
//! executable MIR graph are inspected here.

use std::collections::BTreeSet;

use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedType, ResolvedUnaryOp,
};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirGlueContract, MirGlueKind, MirGlueOperation, MirLayout, MirOwnership,
    MirTypeKind,
};
use crate::core::{CheckedProgram, NodeId, PrimitiveType, ResolvedTypeId};

use super::{
    MirFunction, MirGenericInstanceContract, MirInstructionKind, MirListOperation, MirTerminator,
    MirValueId,
};

/// Name of the finite whole-program island closed by this contract.
pub const SCALAR_COLLECTION_ISLAND: &str = "copy-scalar-collection-v1";

/// Checker-owned admission state for the flat Copy-record verifier island.
///
/// This is intentionally computed before MIR materialization.  A materialized
/// candidate is not enough for a public verifier API: the API verifies a
/// whole checked program, so a generic, imported, effectful, or otherwise
/// mixed sibling must not be silently omitted from the MIR graph and thereby
/// receive a partial green result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatCopyRecordAdmission {
    /// No executable typed body or signature uses a user record.
    OutsideProfile,
    /// A record is used, but the complete program is outside the currently
    /// closed verifier island.  This is an explicit compatibility boundary;
    /// it is not a MIR construction failure.
    MixedCoverage,
    /// The checker-owned typed program is closed enough that construction
    /// failure is a hard MIR materialization error rather than a fallback.
    CompleteCoverage,
}

/// Classify flat Copy-record verifier admission from checker-owned artifacts.
///
/// The predicate deliberately does not build MIR and never consults the
/// retained surface AST.  Its conservative mixed-coverage checks protect the
/// public whole-program verifier from returning a partial MIR subgraph for a
/// program containing generic templates, imports, effects, or other semantic
/// consumers not yet covered by this island.
pub fn classify_flat_copy_record_admission(program: &CheckedProgram) -> FlatCopyRecordAdmission {
    let record_ids = program
        .type_defs()
        .values()
        .filter(|definition| definition.kind == crate::core::ResolvedTypeKind::Record)
        .map(|definition| definition.node_id.0.clone())
        .collect::<BTreeSet<_>>();
    if record_ids.is_empty() {
        return FlatCopyRecordAdmission::OutsideProfile;
    }

    let uses_record = program_uses_record(program, &record_ids);
    if !uses_record {
        return FlatCopyRecordAdmission::OutsideProfile;
    }

    if has_mixed_coverage(program) {
        FlatCopyRecordAdmission::MixedCoverage
    } else {
        FlatCopyRecordAdmission::CompleteCoverage
    }
}

fn has_mixed_coverage(program: &CheckedProgram) -> bool {
    fn is_runtime_origin(origin: &crate::core::Origin) -> bool {
        matches!(origin, crate::core::Origin::RuntimeSystem { .. })
    }

    let mixed = program.has_imports()
        || program
            .flows()
            .values()
            .any(|flow| !is_runtime_origin(&flow.origin))
        || !program.sessions().is_empty()
        || !program.actors().is_empty()
        || !program.capabilities().is_empty()
        || !program.traits().is_empty()
        || !program.impls().is_empty()
        || !program.extern_blocks().is_empty()
        || program
            .transitions()
            .values()
            .any(|transition| !is_runtime_origin(&transition.origin))
        || !program.backend_requirements().is_empty()
        || program.type_defs().values().any(|definition| {
            matches!(definition.origin, crate::core::Origin::User(_))
                && (!definition.generic_parameters.is_empty()
                    || definition.kind != crate::core::ResolvedTypeKind::Record
                        && definition.kind != crate::core::ResolvedTypeKind::Alias
                        && definition.kind != crate::core::ResolvedTypeKind::Newtype)
        })
        || program.functions().values().any(|function| {
            !function.generics.is_empty()
                || !function.generic_binders.is_empty()
                || !function.effects.is_empty()
                || function.is_async
                || function.is_comptime
                || function.extern_abi.is_some()
        })
        || program.callables().values().any(|callable| {
            !callable.signature.generic_parameters.is_empty()
                || !callable.signature.effects.is_empty()
                || !callable.body.captures.is_empty()
                || !callable.body.default_values.is_empty()
        })
        // The closed record verifier island proves value semantics for Copy
        // records. A view/mutate parameter is a borrow/effect contract with a
        // separate ownership proof and must remain on the compatibility
        // verifier until that contract has its own MIR consumer island.
        || program.resolved_signatures().values().any(|signature| {
            signature.parameters.iter().any(|parameter| {
                matches!(
                    parameter.permission,
                    Some(crate::core::ir::Permission::View | crate::core::ir::Permission::Mutate)
                )
            })
        });
    mixed
}

/// Scan the checker-owned type references that make up a whole program.
/// `resolved_node_types` is populated for every typed body node; the other
/// maps cover declaration and generated type edges that do not have an
/// expression node.  This keeps admission independent of both source AST and
/// MIR materialization.
fn program_uses_record(program: &CheckedProgram, record_ids: &BTreeSet<String>) -> bool {
    fn contains(
        program: &CheckedProgram,
        ty: &ResolvedTypeId,
        record_ids: &BTreeSet<String>,
        visited: &mut BTreeSet<ResolvedTypeId>,
    ) -> bool {
        if !visited.insert(ty.clone()) {
            return false;
        }
        let Some(resolved) = program.resolved_types().get(ty) else {
            return false;
        };
        match resolved {
            ResolvedType::Nominal {
                item, arguments, ..
            } => {
                record_ids.contains(item.as_str())
                    || arguments
                        .iter()
                        .any(|argument| contains(program, argument, record_ids, visited))
            }
            ResolvedType::Reference { target, .. }
            | ResolvedType::CBuffer(target)
            | ResolvedType::Ownership { target, .. }
            | ResolvedType::Newtype { inner: target, .. }
            | ResolvedType::Slice(target)
            | ResolvedType::RawPointer { target, .. }
            | ResolvedType::Option(target) => contains(program, target, record_ids, visited),
            ResolvedType::Result { ok, error } => {
                contains(program, ok, record_ids, visited)
                    || contains(program, error, record_ids, visited)
            }
            ResolvedType::Tuple(elements) => elements
                .iter()
                .any(|element| contains(program, element, record_ids, visited)),
            ResolvedType::Function {
                parameters, result, ..
            } => {
                parameters
                    .iter()
                    .any(|parameter| contains(program, parameter, record_ids, visited))
                    || contains(program, result, record_ids, visited)
            }
            ResolvedType::Array { element, .. } => contains(program, element, record_ids, visited),
            ResolvedType::FlowStateSet { .. }
            | ResolvedType::Primitive(_)
            | ResolvedType::GenericParameter(_)
            | ResolvedType::Capability(_)
            | ResolvedType::Trait { .. }
            | ResolvedType::DynamicAny { .. } => false,
        }
    }

    let mut visited = BTreeSet::new();
    let mut check = |ty: &ResolvedTypeId| contains(program, ty, record_ids, &mut visited);

    program.resolved_node_types().values().any(&mut check)
        || program.resolved_field_types().values().any(&mut check)
        || program.resolved_type_operands().values().any(&mut check)
        || program
            .resolved_type_arguments()
            .values()
            .flatten()
            .any(&mut check)
        || program.resolved_type_targets().values().any(&mut check)
        || program.resolved_signatures().values().any(|signature| {
            signature
                .parameters
                .iter()
                .any(|parameter| check(&parameter.ty))
                || check(&signature.result)
        })
        || program
            .resolved_bodies()
            .values()
            .any(|body| body.locals.values().any(|local| check(&local.ty)) || check(&body.root.ty))
}

/// Return whether the canonical graph contains an operation that the default
/// scalar collection selector recognizes as a migrated production candidate.
///
/// This is intentionally narrower than "the graph mentions a List/Set".  A
/// plain collection value is still a compatibility input; only a materialized
/// `ListOp::Len` or a checker-owned `ScalarSetFacade` instance has crossed the
/// S11 production boundary.  Keeping this fact next to the island contract
/// prevents the CLI and direct native entry points from growing independent
/// candidate predicates.
pub fn contains_scalar_collection_candidate(program: &MirProgram) -> bool {
    let has_list_len = program.functions().values().any(|function| {
        function.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.kind,
                    MirInstructionKind::ListOp {
                        operation: MirListOperation::Len,
                        ..
                    }
                )
            })
        })
    });
    has_list_len
        || program.instances().values().any(|instance| {
            matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarSetFacade { .. }
            )
        })
}

/// Return whether the canonical executable graph contains a flat Copy record
/// value at a consumer boundary.
///
/// This is the MIR-side counterpart of the default route's front-end record
/// hint.  It deliberately examines only materialized values, parameters, and
/// results: a declaration in the checker catalog is not executable evidence.
/// Keeping the predicate with the island contract lets direct native callers
/// and the CLI make the same admission decision without re-reading surface
/// record names or duplicating the TypeDesc rule.
pub fn contains_flat_copy_record_candidate(program: &MirProgram) -> bool {
    program.functions().values().any(|function| {
        // The current flat-record native contract emits only simple function
        // symbols.  A qualified trait/impl method may carry an implicit
        // receiver whose type is a flat record, but that declaration is not a
        // record value consumed by this production island.  Treating it as a
        // candidate would make unrelated metadata-only programs cross the
        // default route boundary.
        let Some(owner) = function.owner.0.strip_prefix("function:") else {
            return false;
        };
        if owner.contains(':') {
            return false;
        }
        function
            .parameters
            .iter()
            .filter_map(|parameter| function.values.get(parameter))
            .any(|value| {
                program
                    .type_catalog()
                    .validate_flat_copy_record(&value.ty)
                    .is_ok()
            })
            || program
                .type_catalog()
                .validate_flat_copy_record(&function.result)
                .is_ok()
            || function.values.values().any(|value| {
                program
                    .type_catalog()
                    .validate_flat_copy_record(&value.ty)
                    .is_ok()
            })
    })
}

/// Validate the current scalar List/Set whole-program island.
///
/// This is deliberately a second, island-level gate above the generic MIR
/// validator.  The generic validator proves that each instruction is legal;
/// this gate proves that the *entire executable graph* belongs to the same
/// finite consumer envelope.  It never reads `CheckedProgram`, `ResolvedBody`,
/// source names, or a backend ABI.
pub fn validate_scalar_collection_island(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut validator = ScalarCollectionValidator {
        program,
        errors: BTreeSet::new(),
        checked_types: BTreeSet::new(),
    };
    validator.validate();
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(validator.errors.into_iter().collect())
    }
}

struct ScalarCollectionValidator<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    checked_types: BTreeSet<crate::core::ResolvedTypeId>,
}

impl<'a> ScalarCollectionValidator<'a> {
    fn validate(&mut self) {
        let main = NodeId("function:main".into());
        if !self.program.functions().contains_key(&main) {
            self.error("program has no canonical function:main".into());
        }
        if !self.program.transitions().is_empty() {
            self.error(format!(
                "{SCALAR_COLLECTION_ISLAND} does not admit Flow transition contracts"
            ));
        }

        // `MirProgram` is the executable graph handed to every current
        // consumer.  Inspecting every materialized function is therefore the
        // sound whole-program boundary; unmaterialized checker declarations
        // are intentionally not part of this scan.
        for function in self.program.functions().values() {
            self.validate_function(function);
        }
        for instance in self.program.instances().values() {
            let Some(function) = self.program.functions().get(&instance.function) else {
                self.error(format!(
                    "instance '{}' executable '{}' is absent",
                    instance.id, instance.function.0
                ));
                continue;
            };
            if instance.arguments.len() != 1 {
                self.error(format!(
                    "instance '{}' has {} type arguments; the scalar island requires one",
                    instance.id,
                    instance.arguments.len()
                ));
            } else if let Err(message) = self
                .program
                .type_catalog()
                .validate_copy_scalar(&instance.arguments[0])
            {
                self.error(format!(
                    "instance '{}' argument is outside the Copy scalar contract: {message}",
                    instance.id
                ));
            }
            match instance.contract {
                MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::ScalarSetFacade { .. } => {}
            }
            // The program constructor and the generic MIR validator already
            // prove the exact instance body.  Keep the island gate explicit
            // about the allowed contract family so a future enum extension
            // cannot silently widen this route.
            if matches!(
                instance.contract,
                MirGenericInstanceContract::ScalarSetFacade { .. }
            ) && !function
                .values
                .values()
                .any(|value| self.is_set_type(&value.ty))
            {
                self.error(format!(
                    "instance '{}' Set facade has no Set value in its canonical body",
                    instance.id
                ));
            }
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        for value in function.values.values() {
            self.validate_type(&value.ty, &format!("function '{}' value", function.owner.0));
        }
        self.validate_type(
            &function.result,
            &format!("function '{}' result", function.owner.0),
        );
        if function
            .contracts
            .iter()
            .any(|contract| contract.kind == super::MirContractKind::Invariant)
        {
            self.error(format!(
                "function '{}' invariant contract is outside {SCALAR_COLLECTION_ISLAND}",
                function.owner.0
            ));
        }
        for event in &function.ownership.events {
            if matches!(
                event.kind,
                super::MirOwnershipEventKind::TransferSession
                    | super::MirOwnershipEventKind::TransferChild
                    | super::MirOwnershipEventKind::BorrowShared
                    | super::MirOwnershipEventKind::BorrowMut
                    | super::MirOwnershipEventKind::BorrowEnd
            ) {
                self.error(format!(
                    "function '{}' ownership effect '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                    function.owner.0,
                    event.kind.as_str()
                ));
            }
        }
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                self.validate_instruction(function, &instruction.kind, instruction.id.as_str());
            }
            self.validate_terminator(function, &block.terminator, block.id.as_str());
        }
    }

    fn validate_type(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if !self.checked_types.insert(ty.clone()) {
            return;
        }
        let Some(descriptor) = self.program.type_catalog().get(ty).cloned() else {
            self.error(format!("{subject} TypeDesc '{}' is absent", ty.as_str()));
            return;
        };
        let result = match descriptor.layout {
            MirLayout::Unit => {
                if descriptor.kind == MirTypeKind::Primitive(PrimitiveType::Unit)
                    && descriptor.abi == MirAbiClass::Unit
                    && descriptor.ownership == MirOwnership::Copy
                    && is_noop_glue(descriptor.glue)
                {
                    Ok(())
                } else {
                    Err("Unit TypeDesc has an inconsistent ABI/ownership/glue contract".into())
                }
            }
            MirLayout::Scalar => self.program.type_catalog().validate_copy_scalar(ty),
            MirLayout::List { element } => self
                .program
                .type_catalog()
                .validate_list_glue(ty, MirGlueOperation::MoveOut)
                .and_then(|()| self.validate_copy_scalar_element(&element)),
            MirLayout::Set { element } => self
                .program
                .type_catalog()
                .validate_set_glue(ty, MirGlueOperation::MoveOut)
                .and_then(|()| self.validate_copy_scalar_element(&element)),
            layout => Err(format!(
                "layout {layout:?} is outside {SCALAR_COLLECTION_ISLAND}"
            )),
        };
        if let Err(message) = result {
            self.error(format!(
                "{subject} type '{}' rejected: {message}",
                ty.as_str()
            ));
        }
    }

    fn validate_copy_scalar_element(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
    ) -> Result<(), String> {
        self.program.type_catalog().validate_copy_scalar(ty)
    }

    fn validate_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        match instruction {
            MirInstructionKind::Const { result, literal } => {
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                match literal {
                    ResolvedLiteral::Int(_) | ResolvedLiteral::Bool(_) => {
                        self.require_copy_scalar(&result_ty, subject, "constant result");
                    }
                    ResolvedLiteral::Unit => self.require_unit(&result_ty, subject),
                    ResolvedLiteral::FloatBits(_) | ResolvedLiteral::String(_) => {
                        self.error(format!(
                            "{subject} literal {literal:?} is outside {SCALAR_COLLECTION_ISLAND}"
                        ))
                    }
                }
            }
            MirInstructionKind::Load { result, place } => {
                if !place.projections.is_empty() {
                    self.error(format!(
                        "{subject} projected Load is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
                if let Some(result_ty) = self.value_type(function, result, subject) {
                    self.require_admitted_type(&result_ty, subject, "Load result");
                }
            }
            MirInstructionKind::Copy { result, source }
            | MirInstructionKind::Move { result, source } => {
                let (Some(result_ty), Some(source_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, source, subject),
                ) else {
                    return;
                };
                self.require_same_type(&result_ty, &source_ty, subject);
                if matches!(instruction, MirInstructionKind::Copy { .. }) {
                    self.require_copy_scalar(&source_ty, subject, "Copy source");
                } else {
                    self.require_move_or_copy(&source_ty, subject, "Move source");
                }
            }
            MirInstructionKind::Clone { result, source } => {
                let (Some(result_ty), Some(source_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, source, subject),
                ) else {
                    return;
                };
                self.require_same_type(&result_ty, &source_ty, subject);
                if self
                    .program
                    .type_catalog()
                    .validate_copy_scalar(&source_ty)
                    .is_err()
                    && !self.is_list_type(&source_ty)
                    && !self.is_set_type(&source_ty)
                {
                    self.error(format!(
                        "{subject} Clone source '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                        source_ty.as_str()
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_glue(&source_ty, MirGlueOperation::Clone)
                {
                    self.error(format!("{subject} Clone glue rejected: {message}"));
                }
            }
            MirInstructionKind::Drop { value } => {
                let Some(ty) = self.value_type(function, value, subject) else {
                    return;
                };
                self.require_move_or_copy(&ty, subject, "Drop value");
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_glue(&ty, MirGlueOperation::Drop)
                {
                    self.error(format!("{subject} Drop glue rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructList { result, elements } => {
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| self.value_type(function, value, subject))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_construct(&result_ty, &element_types)
                {
                    self.error(format!("{subject} List construction rejected: {message}"));
                }
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
            } => {
                let (Some(result_ty), Some(list_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, list, subject),
                ) else {
                    return;
                };
                if *operation != MirListOperation::Len {
                    self.error(format!(
                        "{subject} List operation {operation:?} is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_operation(&result_ty, &list_ty, *operation)
                {
                    self.error(format!("{subject} List operation rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let Some(result_ty) = self.value_type(function, result, subject) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| self.value_type(function, value, subject))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_set_construct(&result_ty, &element_types)
                {
                    self.error(format!("{subject} Set construction rejected: {message}"));
                }
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                let (Some(result_ty), Some(set_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, set, subject),
                ) else {
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| self.value_type(function, value, subject));
                if argument.is_some() && argument_ty.is_none() {
                    return;
                }
                if let Err(message) = self.program.type_catalog().validate_set_operation(
                    &result_ty,
                    &set_ty,
                    argument_ty.as_ref(),
                    *operation,
                ) {
                    self.error(format!("{subject} Set operation rejected: {message}"));
                }
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                let (Some(result_ty), Some(left_ty), Some(right_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, left, subject),
                    self.value_type(function, right, subject),
                ) else {
                    return;
                };
                self.require_copy_scalar(&left_ty, subject, "binary left operand");
                self.require_copy_scalar(&right_ty, subject, "binary right operand");
                self.require_copy_scalar(&result_ty, subject, "binary result");
                if left_ty != right_ty || !binary_supported(*op, &left_ty, &result_ty, self) {
                    self.error(format!(
                        "{subject} binary operator {op:?} is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                let (Some(result_ty), Some(operand_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, operand, subject),
                ) else {
                    return;
                };
                match op {
                    ResolvedUnaryOp::Negate => {
                        self.require_copy_scalar(&operand_ty, subject, "negate operand");
                        self.require_copy_scalar(&result_ty, subject, "negate result");
                        if result_ty != operand_ty
                            || !is_signed_integer(&self.program.type_catalog(), &operand_ty)
                        {
                            self.error(format!(
                                "{subject} negate is outside {SCALAR_COLLECTION_ISLAND}"
                            ));
                        }
                    }
                    ResolvedUnaryOp::Not => {
                        self.require_copy_scalar(&operand_ty, subject, "Not operand");
                        self.require_copy_scalar(&result_ty, subject, "Not result");
                        if !is_bool(&self.program.type_catalog(), &operand_ty)
                            || !is_bool(&self.program.type_catalog(), &result_ty)
                        {
                            self.error(format!(
                                "{subject} Not is outside {SCALAR_COLLECTION_ISLAND}"
                            ));
                        }
                    }
                    ResolvedUnaryOp::BorrowShared
                    | ResolvedUnaryOp::BorrowMutable
                    | ResolvedUnaryOp::Dereference => self.error(format!(
                        "{subject} unary {op:?} is outside {SCALAR_COLLECTION_ISLAND}"
                    )),
                }
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
            } => self.validate_call(
                function,
                result.clone(),
                callee,
                type_arguments,
                arguments,
                subject,
            ),
            MirInstructionKind::Convert { result, source } => {
                let (Some(result_ty), Some(source_ty)) = (
                    self.value_type(function, result, subject),
                    self.value_type(function, source, subject),
                ) else {
                    return;
                };
                self.require_copy_scalar(&source_ty, subject, "conversion source");
                self.require_copy_scalar(&result_ty, subject, "conversion result");
                if self
                    .program
                    .type_catalog()
                    .validate_conversion(&source_ty, &result_ty)
                    .is_err()
                {
                    self.error(format!(
                        "{subject} conversion is outside {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
            }
            MirInstructionKind::Nop => {}
            MirInstructionKind::Borrow { .. }
            | MirInstructionKind::EndBorrow { .. }
            | MirInstructionKind::Project { .. }
            | MirInstructionKind::MoveProject { .. }
            | MirInstructionKind::Construct { .. }
            | MirInstructionKind::ConstructVariant { .. }
            | MirInstructionKind::ConstructVariantMove { .. }
            | MirInstructionKind::UpdateRecord { .. }
            | MirInstructionKind::FlowTransition { .. }
            | MirInstructionKind::BuiltinCall { .. } => self.error(format!(
                "{subject} MIR operation is outside {SCALAR_COLLECTION_ISLAND}"
            )),
        }
    }

    fn validate_call(
        &mut self,
        caller: &MirFunction,
        result: Option<MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let ResolvedCallee::Function(owner) = callee else {
            self.error(format!(
                "{subject} callee {callee:?} is outside {SCALAR_COLLECTION_ISLAND}"
            ));
            return;
        };
        let Some(target) = self.program.functions().get(owner) else {
            self.error(format!("{subject} callee '{}' is absent", owner.0));
            return;
        };
        let instance = self
            .program
            .instances()
            .values()
            .find(|instance| instance.function == *owner);
        if let Some(instance) = instance {
            if instance.arguments != type_arguments {
                self.error(format!(
                    "{subject} generic arguments disagree with instance '{}'",
                    instance.id
                ));
            }
        } else if !type_arguments.is_empty() {
            self.error(format!(
                "{subject} generic arguments target a non-instance function"
            ));
        }
        if arguments.len() != target.parameters.len() {
            self.error(format!("{subject} call arity disagrees with callee"));
        }
        for (index, (argument, parameter)) in arguments.iter().zip(&target.parameters).enumerate() {
            let (Some(argument_ty), Some(parameter_ty)) = (
                self.value_type(caller, argument, subject),
                self.value_type(target, parameter, subject),
            ) else {
                continue;
            };
            if argument_ty != parameter_ty {
                self.error(format!(
                    "{subject} call argument {index} TypeDesc disagrees with callee"
                ));
            }
        }
        match result {
            Some(result) => {
                let Some(result_ty) = self.value_type(caller, &result, subject) else {
                    return;
                };
                if result_ty != target.result {
                    self.error(format!(
                        "{subject} call result TypeDesc disagrees with callee"
                    ));
                }
            }
            None => {
                if !self.is_unit_type(&target.result) {
                    self.error(format!(
                        "{subject} non-unit call has no result in {SCALAR_COLLECTION_ISLAND}"
                    ));
                }
            }
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &str,
    ) {
        match terminator {
            MirTerminator::Goto { .. } => {}
            MirTerminator::Branch { condition, .. } => {
                if let Some(ty) = self.value_type(function, condition, subject) {
                    if !is_bool(&self.program.type_catalog(), &ty) {
                        self.error(format!(
                            "{subject} branch condition is outside {SCALAR_COLLECTION_ISLAND}"
                        ));
                    }
                }
            }
            MirTerminator::Return { value } => match value {
                Some(value) => {
                    if let Some(ty) = self.value_type(function, value, subject) {
                        if ty != function.result {
                            self.error(format!(
                                "{subject} return TypeDesc disagrees with function result"
                            ));
                        }
                    }
                }
                None if !self.is_unit_type(&function.result) => self.error(format!(
                    "{subject} missing non-unit return value in {SCALAR_COLLECTION_ISLAND}"
                )),
                None => {}
            },
            MirTerminator::Trap { .. } => {}
            MirTerminator::Switch { .. }
            | MirTerminator::SwitchMove { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => self.error(format!(
                "{subject} terminator is outside {SCALAR_COLLECTION_ISLAND}"
            )),
        }
    }

    fn value_type(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) -> Option<crate::core::ResolvedTypeId> {
        function
            .values
            .get(value)
            .map(|value| value.ty.clone())
            .or_else(|| {
                self.error(format!("{subject} value '{}' is absent", value));
                None
            })
    }

    fn require_admitted_type(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        role: &str,
    ) {
        let valid = self.program.type_catalog().validate_copy_scalar(ty).is_ok()
            || self.is_list_type(ty)
            || self.is_set_type(ty)
            || self.is_unit_type(ty);
        if !valid {
            self.error(format!(
                "{subject} {role} type '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                ty.as_str()
            ));
        }
    }

    fn require_copy_scalar(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str, role: &str) {
        if let Err(message) = self.program.type_catalog().validate_copy_scalar(ty) {
            self.error(format!("{subject} {role} rejected: {message}"));
        }
    }

    fn require_move_or_copy(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        role: &str,
    ) {
        if self.program.type_catalog().validate_copy_scalar(ty).is_ok() || self.is_unit_type(ty) {
            return;
        }
        if !self.is_list_type(ty) && !self.is_set_type(ty) {
            self.error(format!(
                "{subject} {role} type '{}' is outside {SCALAR_COLLECTION_ISLAND}",
                ty.as_str()
            ));
        }
    }

    fn require_same_type(
        &mut self,
        result: &crate::core::ResolvedTypeId,
        source: &crate::core::ResolvedTypeId,
        subject: &str,
    ) {
        if result != source {
            self.error(format!(
                "{subject} result/source TypeDesc identities disagree"
            ));
        }
    }

    fn require_unit(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if !self.is_unit_type(ty) {
            self.error(format!(
                "{subject} unit literal has non-unit TypeDesc '{}'",
                ty.as_str()
            ));
        }
    }

    fn is_unit_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Primitive(PrimitiveType::Unit)
                    && descriptor.abi == MirAbiClass::Unit
                    && descriptor.ownership == MirOwnership::Copy
                    && is_noop_glue(descriptor.glue)
            })
    }

    fn is_list_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| {
                matches!(&descriptor.layout, MirLayout::List { .. })
                    && descriptor.kind == MirTypeKind::List
                    && descriptor.abi == MirAbiClass::OpaqueHandle
                    && descriptor.ownership == MirOwnership::Move
                    && descriptor.glue
                        == (MirGlueContract {
                            move_out: MirGlueKind::List,
                            clone: MirGlueKind::List,
                            drop: MirGlueKind::List,
                        })
            })
    }

    fn is_set_type(&self, ty: &crate::core::ResolvedTypeId) -> bool {
        self.program
            .type_catalog()
            .get(ty)
            .is_some_and(|descriptor| {
                matches!(&descriptor.layout, MirLayout::Set { .. })
                    && descriptor.kind == MirTypeKind::Set
                    && descriptor.abi == MirAbiClass::SetHandle
                    && descriptor.ownership == MirOwnership::Move
                    && descriptor.glue
                        == (MirGlueContract {
                            move_out: MirGlueKind::Set,
                            clone: MirGlueKind::Set,
                            drop: MirGlueKind::Set,
                        })
            })
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

fn is_noop_glue(glue: MirGlueContract) -> bool {
    glue == MirGlueContract {
        move_out: MirGlueKind::Noop,
        clone: MirGlueKind::Noop,
        drop: MirGlueKind::Noop,
    }
}

fn is_signed_integer(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    catalog.get(ty).is_some_and(|descriptor| {
        matches!(
            descriptor.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true
            }
        )
    })
}

fn is_bool(
    catalog: &crate::core::mir::types::MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> bool {
    catalog
        .get(ty)
        .is_some_and(|descriptor| descriptor.abi == MirAbiClass::Bool)
}

fn binary_supported(
    op: ResolvedBinaryOp,
    left: &crate::core::ResolvedTypeId,
    result: &crate::core::ResolvedTypeId,
    validator: &ScalarCollectionValidator<'_>,
) -> bool {
    let integer = is_signed_integer(&validator.program.type_catalog(), left);
    let boolean = is_bool(&validator.program.type_catalog(), left);
    let result_is_bool = is_bool(&validator.program.type_catalog(), result);
    match op {
        // Keep this matrix identical to the native MIR validator.  The
        // island must be an intersection of consumer capabilities; accepting
        // an operation that only reference/VM can execute would recreate the
        // native-only eligibility drift this gate is meant to prevent.
        ResolvedBinaryOp::Add | ResolvedBinaryOp::Subtract => integer && left == result,
        ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual => {
            (integer || boolean) && result_is_bool
        }
        ResolvedBinaryOp::Less
        | ResolvedBinaryOp::Greater
        | ResolvedBinaryOp::LessEqual
        | ResolvedBinaryOp::GreaterEqual => integer && result_is_bool,
        ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr => boolean && result_is_bool,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_scalar_collection_island, SCALAR_COLLECTION_ISLAND};
    use crate::core::mir::reference::MirProgram;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn canonical(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    #[test]
    fn accepts_the_complete_scalar_list_set_graph() {
        let program = canonical(include_str!(
            "../../../tests/fixtures/mir_native_list_len.mimi"
        ));
        validate_scalar_collection_island(&program).expect("scalar collection island");
    }

    #[test]
    fn rejects_a_managed_value_mixed_into_the_collection_graph() {
        let program = canonical(
            "func main() -> i32 { let values = [1, 2, 3] let count = len(values) drop(values) let text = \"outside\" drop(text) count }",
        );
        let errors = validate_scalar_collection_island(&program)
            .expect_err("managed values must stay outside the scalar collection island");
        assert!(
            errors.iter().any(|error| {
                error.contains("outside") || error.contains("String") || error.contains("Handle")
            }),
            "{SCALAR_COLLECTION_ISLAND}: {errors:?}"
        );
    }

    #[test]
    fn rejects_flow_effects_even_when_the_other_values_are_scalar() {
        let program = canonical(
            "flow Counter { state Zero { n: i32 } transition inc(Zero) -> Zero { return Zero { n: self.n + 1 } } } func main() -> i32 { let c = Zero { n: 1 } let c2 = Counter::inc(c) c2.n }",
        );
        let errors = validate_scalar_collection_island(&program)
            .expect_err("Flow must not enter the synchronous collection island");
        assert!(errors
            .iter()
            .any(|error| error.contains("Flow transition contracts")));
    }
}
