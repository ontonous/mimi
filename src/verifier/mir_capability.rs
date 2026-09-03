//! Static capability gate for the verifier's Canonical MIR consumer.
//!
//! `verify_mir` is intentionally a contract engine: functions without an
//! obligation are not symbolically executed.  That is useful for keeping the
//! proof engine small, but it must not be mistaken for whole-program
//! capability.  The default route therefore runs this structural gate over
//! every function before admitting an island.  The gate consumes only the
//! canonical MIR program and its TypeDesc catalog; it never asks the frontend
//! to rediscover a type, ownership, or call fact.

use std::collections::{BTreeSet, HashSet};

use crate::core::ir::{ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{MirAbiClass, MirGlueKind, MirLayout, MirOwnership, MirTypeKind};
use crate::core::mir::{
    MirContractKind, MirFunction, MirGenericInstanceContract, MirInstructionKind, MirProjection,
    MirSwitchCase, MirTerminator, MirValueId,
};

/// Validate the subset of canonical MIR that the MIR verifier can safely
/// coexist with on the default route.
///
/// This is deliberately a capability result rather than a verification
/// result.  It answers whether every compiled MIR shape has a verifier
/// interpretation.  A function with no contracts is still scanned; otherwise
/// an unsupported body could silently enter a native/bytecode island merely
/// because the verifier had no obligation to print.
pub fn validate_mir_capabilities(program: &MirProgram) -> Result<(), Vec<String>> {
    let mut gate = CapabilityGate::new(program);
    gate.validate();
    if gate.errors.is_empty() {
        Ok(())
    } else {
        Err(gate.errors.into_iter().collect())
    }
}

struct CapabilityGate<'a> {
    program: &'a MirProgram,
    errors: BTreeSet<String>,
    checked_types: HashSet<crate::core::ResolvedTypeId>,
}

impl<'a> CapabilityGate<'a> {
    fn new(program: &'a MirProgram) -> Self {
        Self {
            program,
            errors: BTreeSet::new(),
            checked_types: HashSet::new(),
        }
    }

    fn validate(&mut self) {
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
            match instance.contract {
                MirGenericInstanceContract::ScalarIdentity => {
                    if let Err(message) = self
                        .program
                        .type_catalog()
                        .validate_scalar_generic_arguments(&instance.arguments)
                    {
                        self.error(format!(
                            "instance '{}' identity TypeDesc contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                    self.validate_identity_instance(function, instance.id.as_str());
                }
                MirGenericInstanceContract::ScalarSetFacade { operation } => {
                    if let Err(message) = crate::core::mir::lower::validate_scalar_set_facade_mir(
                        function,
                        self.program.type_catalog(),
                        operation,
                    ) {
                        self.error(format!(
                            "instance '{}' Set facade contract is unsupported: {message}",
                            instance.id
                        ));
                    }
                }
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
            .any(|contract| contract.kind == MirContractKind::Invariant)
        {
            self.error(format!(
                "function '{}' has an invariant contract outside the MIR verifier capability",
                function.owner.0
            ));
        }
        self.validate_acyclic_cfg(function);
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                self.validate_instruction(function, &instruction.kind, instruction.id.as_str());
            }
            self.validate_terminator(function, &block.terminator, block.id.as_str());
        }
        for event in &function.ownership.events {
            if matches!(
                event.kind,
                crate::core::mir::MirOwnershipEventKind::TransferSession
                    | crate::core::mir::MirOwnershipEventKind::TransferChild
                    | crate::core::mir::MirOwnershipEventKind::BorrowMut
            ) {
                self.error(format!(
                    "function '{}' ownership effect '{}' is outside the MIR verifier capability",
                    function.owner.0,
                    event.kind.as_str()
                ));
            }
        }
    }

    fn validate_type(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if !self.checked_types.insert(ty.clone()) {
            return;
        }
        let result = self.validate_type_contract(ty);
        if let Err(message) = result {
            self.error(format!(
                "{subject} type '{}' rejected: {message}",
                ty.as_str()
            ));
        }
    }

    fn validate_type_contract(&mut self, ty: &crate::core::ResolvedTypeId) -> Result<(), String> {
        let catalog = self.program.type_catalog();
        let descriptor = catalog
            .get(ty)
            .ok_or_else(|| format!("TypeDesc '{}' is absent", ty.as_str()))?
            .clone();
        match &descriptor.layout {
            MirLayout::Unit => {
                if descriptor.kind == MirTypeKind::Primitive(crate::core::PrimitiveType::Unit)
                    && descriptor.abi == MirAbiClass::Unit
                    && descriptor.ownership == MirOwnership::Copy
                    && is_noop_glue(descriptor.glue)
                {
                    Ok(())
                } else {
                    Err("Unit TypeDesc has an inconsistent ABI/ownership/glue contract".into())
                }
            }
            MirLayout::Scalar => {
                if catalog.validate_copy_scalar(ty).is_ok()
                    || catalog.validate_owned_string(ty).is_ok()
                {
                    Ok(())
                } else {
                    Err("scalar TypeDesc is outside the verifier scalar contract".into())
                }
            }
            MirLayout::Handle => {
                if catalog.validate_owned_string(ty).is_ok() {
                    Ok(())
                } else {
                    Err("handle TypeDesc is not the canonical owned String contract".into())
                }
            }
            MirLayout::Pointer { .. } => {
                if catalog.validate_reference_type(ty).is_ok() {
                    Ok(())
                } else {
                    Err(
                        "pointer TypeDesc is outside the immutable scalar reference contract"
                            .into(),
                    )
                }
            }
            MirLayout::List { element } => {
                catalog
                    .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                self.validate_type(element, "List element");
                Ok(())
            }
            MirLayout::Set { element } => {
                catalog
                    .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)?;
                self.validate_type(element, "Set element");
                Ok(())
            }
            MirLayout::Tuple(elements) => {
                catalog.validate_recursive_tuple_abi(ty)?;
                for element in elements {
                    self.validate_type(element, "tuple element");
                }
                Ok(())
            }
            MirLayout::Record { fields, .. } => {
                if descriptor.ownership == MirOwnership::Linear {
                    self.require_linear_aggregate(ty, &descriptor)?;
                } else {
                    self.require_copy_aggregate(ty, &descriptor)?;
                }
                for field in fields {
                    self.validate_type(&field.ty, "record field");
                }
                Ok(())
            }
            MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => {
                if descriptor.ownership == MirOwnership::Copy {
                    self.require_copy_aggregate(ty, &descriptor)?;
                } else {
                    catalog.validate_option_string_variant(ty).map_err(|message| {
                        format!(
                            "non-Copy variant TypeDesc is outside the verifier capability: {message}"
                        )
                    })?;
                }
                for variant in variants {
                    for field in &variant.fields {
                        self.validate_type(&field.ty, "variant field");
                    }
                }
                Ok(())
            }
            layout => Err(format!(
                "layout {layout:?} is outside the verifier capability"
            )),
        }
    }

    fn require_copy_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.ownership != MirOwnership::Copy
            || descriptor.abi != MirAbiClass::Aggregate
            || !is_noop_glue(descriptor.glue)
        {
            return Err(format!(
                "aggregate '{}' is not Copy with canonical no-op glue",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn require_linear_aggregate(
        &self,
        ty: &crate::core::ResolvedTypeId,
        descriptor: &crate::core::mir::types::MirTypeDesc,
    ) -> Result<(), String> {
        if descriptor.ownership != MirOwnership::Linear
            || descriptor.abi != MirAbiClass::Aggregate
            || descriptor.glue.move_out != MirGlueKind::Aggregate
            || descriptor.glue.clone != MirGlueKind::Aggregate
            || descriptor.glue.drop != MirGlueKind::Aggregate
            || descriptor.drop_plan.is_none()
        {
            return Err(format!(
                "linear aggregate '{}' lacks the canonical aggregate Move/Clone/Drop glue plan",
                ty.as_str()
            ));
        }
        Ok(())
    }

    fn validate_instruction(
        &mut self,
        function: &MirFunction,
        instruction: &MirInstructionKind,
        subject: &str,
    ) {
        let catalog = self.program.type_catalog();
        match instruction {
            MirInstructionKind::Const { result, literal } => {
                let Some(ty) = value_type(function, result) else {
                    self.error(format!("{subject} constant result '{}' is absent", result));
                    return;
                };
                let supported = match literal {
                    ResolvedLiteral::Int(_) | ResolvedLiteral::Bool(_) => {
                        catalog.validate_copy_scalar(&ty).is_ok()
                    }
                    ResolvedLiteral::String(_) => catalog.validate_owned_string(&ty).is_ok(),
                    ResolvedLiteral::FloatBits(_) | ResolvedLiteral::Unit => false,
                };
                if !supported {
                    self.error(format!(
                        "{subject} literal {literal:?} is outside the verifier TypeDesc contract"
                    ));
                }
            }
            MirInstructionKind::Load { result, place } => {
                if place.projections.iter().any(|projection| {
                    matches!(
                        projection,
                        crate::core::ir::ResolvedProjection::Index { .. }
                    )
                }) {
                    self.error(format!(
                        "{subject} indexed Load is outside the verifier capability"
                    ));
                }
                if place.projections.iter().any(|projection| {
                    matches!(
                        projection,
                        crate::core::ir::ResolvedProjection::Deref { .. }
                    )
                }) {
                    self.error(format!(
                        "{subject} dereference Load is outside the verifier capability"
                    ));
                }
                let _ = result;
            }
            MirInstructionKind::Copy { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    if catalog.validate_copy(&ty).is_err() {
                        self.error(format!("{subject} Copy source '{}' is not Copy", source));
                    }
                }
            }
            MirInstructionKind::Move { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    let valid = catalog
                        .get(&ty)
                        .is_some_and(|descriptor| descriptor.ownership == MirOwnership::Copy)
                        || catalog
                            .validate_glue(&ty, crate::core::mir::types::MirGlueOperation::MoveOut)
                            .is_ok();
                    if !valid {
                        self.error(format!(
                            "{subject} Move source '{}' has no verifier glue",
                            source
                        ));
                    }
                }
            }
            MirInstructionKind::Clone { result, source } => {
                self.require_same_type(function, result, source, subject);
                if let Some(ty) = value_type(function, source) {
                    let valid = catalog.validate_copy(&ty).is_ok()
                        || catalog
                            .validate_glue(&ty, crate::core::mir::types::MirGlueOperation::Clone)
                            .is_ok();
                    if !valid {
                        self.error(format!(
                            "{subject} Clone source '{}' has no verifier glue",
                            source
                        ));
                    }
                }
            }
            MirInstructionKind::Drop { value } => {
                if let Some(ty) = value_type(function, value) {
                    let valid = catalog.validate_copy(&ty).is_ok()
                        || catalog
                            .validate_glue(&ty, crate::core::mir::types::MirGlueOperation::Drop)
                            .is_ok();
                    if !valid {
                        self.error(format!(
                            "{subject} Drop value '{}' has no verifier glue",
                            value
                        ));
                    }
                }
            }
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
            } => {
                let (Some(source_ty), Some(result_ty)) =
                    (value_type(function, source), value_type(function, result))
                else {
                    return;
                };
                if let Err(message) = catalog.validate_borrow(&source_ty, &result_ty, *mutable) {
                    self.error(format!("{subject} borrow rejected: {message}"));
                }
            }
            MirInstructionKind::EndBorrow { borrow } => {
                if let Some(ty) = value_type(function, borrow) {
                    if let Err(message) = catalog.validate_reference_type(&ty) {
                        self.error(format!("{subject} end-borrow rejected: {message}"));
                    }
                }
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
                list_index_contract,
            } => {
                let (Some(base_ty), Some(result_ty)) =
                    (value_type(function, base), value_type(function, result))
                else {
                    return;
                };
                match projection {
                    MirProjection::Index(index) => {
                        let Some(index_ty) = value_type(function, index) else {
                            return;
                        };
                        let Some(receipt) = list_index_contract else {
                            self.error(format!(
                                "{subject} List index projection has no canonical receipt"
                            ));
                            return;
                        };
                        if let Err(message) = catalog.validate_list_index_projection_receipt(
                            &base_ty, &index_ty, &result_ty, receipt,
                        ) {
                            self.error(format!("{subject} indexed projection rejected: {message}"));
                        }
                    }
                    MirProjection::Dereference => {
                        if let Err(message) = catalog.validate_dereference(&base_ty, &result_ty) {
                            self.error(format!("{subject} dereference rejected: {message}"));
                        }
                    }
                    MirProjection::Field(_) | MirProjection::Tuple(_) => {
                        if list_index_contract.is_some() {
                            self.error(format!(
                                "{subject} List index receipt is attached to a non-index projection"
                            ));
                            return;
                        }
                        if let Err(message) =
                            catalog.validate_projection(&base_ty, &result_ty, projection)
                        {
                            self.error(format!("{subject} projection rejected: {message}"));
                        }
                    }
                }
            }
            MirInstructionKind::MoveProject { .. } => {
                self.error(format!(
                    "{subject} MoveProject is outside the verifier capability"
                ));
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let field_types = fields
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} aggregate field is absent"));
                } else if let Err(message) =
                    catalog.validate_aggregate(&result_ty, kind, &field_types)
                {
                    self.error(format!("{subject} aggregate rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructList { result, elements } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.error(format!("{subject} List element is absent"));
                } else if let Err(message) =
                    catalog.validate_list_construct(&result_ty, &element_types)
                {
                    self.error(format!("{subject} List construction rejected: {message}"));
                }
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                let (Some(result_ty), Some(list_ty)) =
                    (value_type(function, result), value_type(function, list))
                else {
                    return;
                };
                let Some(receipt) = list_operation_contract.as_ref() else {
                    self.error(format!("{subject} List operation has no canonical receipt"));
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                if let Err(message) = catalog.validate_list_operation_receipt_with_argument(
                    &result_ty,
                    &list_ty,
                    argument_ty.as_ref(),
                    *operation,
                    receipt,
                ) {
                    self.error(format!("{subject} List operation rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let element_types = elements
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.error(format!("{subject} Set element is absent"));
                } else if let Err(message) =
                    catalog.validate_set_construct(&result_ty, &element_types)
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
                let (Some(result_ty), Some(set_ty)) =
                    (value_type(function, result), value_type(function, set))
                else {
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| value_type(function, value));
                if argument.is_some() && argument_ty.is_none() {
                    self.error(format!("{subject} Set argument is absent"));
                } else if let Err(message) = catalog.validate_set_operation(
                    &result_ty,
                    &set_ty,
                    argument_ty.as_ref(),
                    *operation,
                ) {
                    self.error(format!("{subject} Set operation rejected: {message}"));
                }
            }
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    return;
                };
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_types = fields
                    .iter()
                    .filter_map(|(_, value)| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} variant payload is absent"));
                } else if let Err(message) = catalog.validate_variant_construct(
                    &result_ty,
                    nominal,
                    variant,
                    &field_ids,
                    &field_types,
                ) {
                    self.error(format!(
                        "{subject} variant construction rejected: {message}"
                    ));
                } else if catalog
                    .get(&result_ty)
                    .is_none_or(|descriptor| descriptor.ownership != MirOwnership::Copy)
                {
                    self.error(format!("{subject} non-Copy variant construction is outside the verifier capability"));
                }
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => {
                let Some(result_ty) = value_type(function, result) else {
                    self.error(format!("{subject} variant result is absent"));
                    return;
                };
                if let Err(message) = catalog.validate_option_string_variant(&result_ty) {
                    self.error(format!(
                        "{subject} ConstructVariantMove rejected: {message}"
                    ));
                    return;
                }
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_types = fields
                    .iter()
                    .filter_map(|(_, value)| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} variant payload is absent"));
                } else if let Err(message) = catalog.validate_variant_construct(
                    &result_ty,
                    nominal,
                    variant,
                    &field_ids,
                    &field_types,
                ) {
                    self.error(format!(
                        "{subject} ConstructVariantMove rejected: {message}"
                    ));
                }
            }
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind,
                fields,
            } => {
                let (Some(result_ty), Some(base_ty)) =
                    (value_type(function, result), value_type(function, base))
                else {
                    return;
                };
                let field_types = fields
                    .iter()
                    .filter_map(|value| value_type(function, value))
                    .collect::<Vec<_>>();
                if field_types.len() != fields.len() {
                    self.error(format!("{subject} record update field is absent"));
                } else if let Err(message) =
                    catalog.validate_record_update(&result_ty, &base_ty, kind, &field_types)
                {
                    self.error(format!("{subject} record update rejected: {message}"));
                }
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                self.validate_binary(function, result, *op, left, right, subject);
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                self.require_same_type_if_unary(function, result, operand, *op, subject);
            }
            MirInstructionKind::BuiltinCall { kind, .. } => {
                if !matches!(
                    kind,
                    crate::core::mir::types::MirBuiltinKind::Abs
                        | crate::core::mir::types::MirBuiltinKind::Min
                        | crate::core::mir::types::MirBuiltinKind::Max
                        | crate::core::mir::types::MirBuiltinKind::PrintlnBool
                        | crate::core::mir::types::MirBuiltinKind::PrintlnInt
                ) {
                    self.error(format!(
                        "{subject} builtin is outside the verifier capability"
                    ));
                }
            }
            MirInstructionKind::Convert { result, source } => {
                let (Some(source_ty), Some(result_ty)) =
                    (value_type(function, source), value_type(function, result))
                else {
                    return;
                };
                match catalog.validate_conversion(&source_ty, &result_ty) {
                    Ok(contract)
                        if matches!(
                            contract.kind,
                            crate::core::mir::types::MirConversionKind::ScalarIdentity
                                | crate::core::mir::types::MirConversionKind::SignedI32ToI64
                        ) => {}
                    Ok(_) => self.error(format!(
                        "{subject} conversion is outside the verifier capability"
                    )),
                    Err(message) => self.error(format!("{subject} conversion rejected: {message}")),
                }
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
            } => {
                self.validate_call(
                    function,
                    result.as_ref(),
                    callee,
                    type_arguments,
                    arguments,
                    subject,
                );
            }
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
            } => self.validate_flow_transition(function, result, transition, arguments, subject),
            MirInstructionKind::Nop => {}
        }
    }

    fn validate_flow_transition(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        transition: &crate::core::NodeId,
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let Some(contract) = self.program.transitions().get(transition) else {
            self.error(format!(
                "{subject} FlowTransition '{}' has no canonical contract",
                transition.0
            ));
            return;
        };
        if contract.effect != crate::core::mir::MirTransitionEffect::SilentLocal
            || contract.targets.len() != 1
            || contract.failure.is_some()
            || contract.is_fallback
            || contract.is_ffi_pinned
        {
            self.error(format!(
                "{subject} FlowTransition is outside the silent-local transition capability"
            ));
        }
        let Some(target) = self.program.functions().get(&contract.owner) else {
            self.error(format!(
                "{subject} FlowTransition target '{}' is absent",
                contract.owner.0
            ));
            return;
        };
        if arguments.len() != target.parameters.len() {
            self.error(format!(
                "{subject} FlowTransition argument arity disagrees with its canonical body"
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if value_type(function, argument) != value_type(target, parameter) {
                self.error(format!(
                    "{subject} FlowTransition argument TypeDesc disagrees with its canonical body"
                ));
            }
        }
        match value_type(function, result) {
            Some(actual) if actual == contract.result && actual == target.result => {}
            _ => self.error(format!(
                "{subject} FlowTransition result TypeDesc disagrees with its canonical contract"
            )),
        }
    }

    fn validate_call(
        &mut self,
        function: &MirFunction,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let ResolvedCallee::Function(owner) = callee else {
            self.error(format!(
                "{subject} callee is outside the canonical MIR verifier capability"
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
                return;
            }
            match instance.contract {
                MirGenericInstanceContract::ScalarIdentity
                | MirGenericInstanceContract::ScalarSetFacade { .. } => {}
            }
        } else if !type_arguments.is_empty() {
            self.error(format!(
                "{subject} generic arguments target a non-instance function"
            ));
            return;
        } else if function_has_ensures(function) {
            self.error(format!(
                "{subject} ordinary call in a contract-bearing function is outside the verifier capability"
            ));
            return;
        }
        if arguments.len() != target.parameters.len() {
            self.error(format!("{subject} call arity disagrees with callee"));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            if value_type(function, argument) != value_type(target, parameter) {
                self.error(format!(
                    "{subject} call argument TypeDesc disagrees with callee"
                ));
            }
        }
        match (result, self.program.type_catalog().get(&target.result)) {
            (Some(result), Some(_))
                if value_type(function, result).as_ref() != Some(&target.result) =>
            {
                self.error(format!(
                    "{subject} call result TypeDesc disagrees with callee"
                ));
            }
            (None, Some(descriptor)) if descriptor.abi != MirAbiClass::Unit => {
                self.error(format!("{subject} non-unit call has no result value"));
            }
            _ => {}
        }
    }

    fn validate_identity_instance(&mut self, function: &MirFunction, subject: &str) {
        let [parameter] = function.parameters.as_slice() else {
            self.error(format!(
                "instance '{}' identity body has an invalid signature",
                subject
            ));
            return;
        };
        if value_type(function, parameter).as_ref() != Some(&function.result) {
            self.error(format!(
                "instance '{}' identity body has an invalid signature",
                subject
            ));
        }
    }

    fn validate_binary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        op: ResolvedBinaryOp,
        left: &MirValueId,
        right: &MirValueId,
        subject: &str,
    ) {
        let (Some(result_ty), Some(left_ty), Some(right_ty)) = (
            value_type(function, result),
            value_type(function, left),
            value_type(function, right),
        ) else {
            return;
        };
        if left_ty != right_ty {
            self.error(format!(
                "{subject} binary operands have different TypeDesc identities"
            ));
            return;
        }
        let scalar = self.program.type_catalog().get(&left_ty);
        let Some(descriptor) = scalar else {
            return;
        };
        let valid = match descriptor.abi {
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } => matches!(
                op,
                ResolvedBinaryOp::Add
                    | ResolvedBinaryOp::Subtract
                    | ResolvedBinaryOp::Multiply
                    | ResolvedBinaryOp::Divide
                    | ResolvedBinaryOp::Remainder
                    | ResolvedBinaryOp::Equal
                    | ResolvedBinaryOp::NotEqual
                    | ResolvedBinaryOp::Less
                    | ResolvedBinaryOp::Greater
                    | ResolvedBinaryOp::LessEqual
                    | ResolvedBinaryOp::GreaterEqual
            ),
            MirAbiClass::Bool => matches!(
                op,
                ResolvedBinaryOp::Equal
                    | ResolvedBinaryOp::NotEqual
                    | ResolvedBinaryOp::LogicalAnd
                    | ResolvedBinaryOp::LogicalOr
            ),
            _ => false,
        };
        let result_is_bool = self
            .program
            .type_catalog()
            .get(&result_ty)
            .is_some_and(|descriptor| descriptor.abi == MirAbiClass::Bool);
        let comparison = matches!(
            op,
            ResolvedBinaryOp::Equal
                | ResolvedBinaryOp::NotEqual
                | ResolvedBinaryOp::Less
                | ResolvedBinaryOp::Greater
                | ResolvedBinaryOp::LessEqual
                | ResolvedBinaryOp::GreaterEqual
        );
        if !valid || (comparison && !result_is_bool) || (!comparison && result_ty != left_ty) {
            self.error(format!(
                "{subject} binary operator is outside the verifier capability"
            ));
        }
    }

    fn require_same_type_if_unary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        operand: &MirValueId,
        op: ResolvedUnaryOp,
        subject: &str,
    ) {
        match op {
            ResolvedUnaryOp::Negate | ResolvedUnaryOp::Not => {
                if op == ResolvedUnaryOp::Negate
                    && value_type(function, result) != value_type(function, operand)
                {
                    self.error(format!(
                        "{subject} negate result TypeDesc disagrees with operand"
                    ));
                }
                if op == ResolvedUnaryOp::Not
                    && !value_type(function, result)
                        .and_then(|ty| self.program.type_catalog().get(&ty))
                        .is_some_and(|descriptor| descriptor.abi == MirAbiClass::Bool)
                {
                    self.error(format!("{subject} Not result is not a canonical bool"));
                }
            }
            ResolvedUnaryOp::BorrowShared
            | ResolvedUnaryOp::BorrowMutable
            | ResolvedUnaryOp::Dereference => self.error(format!(
                "{subject} unary {op:?} is outside the explicit MIR Borrow/Project capability"
            )),
        }
    }

    fn require_same_type(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) {
        if value_type(function, result) != value_type(function, source) {
            self.error(format!(
                "{subject} result and source TypeDesc identities disagree"
            ));
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &str,
    ) {
        match terminator {
            MirTerminator::Goto { .. } | MirTerminator::Branch { .. } => {}
            MirTerminator::Switch { scrutinee, arms } => {
                let Some(ty) = value_type(function, scrutinee) else {
                    return;
                };
                if self.program.type_catalog().variant_layout(&ty).is_none()
                    || arms
                        .iter()
                        .any(|arm| matches!(arm.case, MirSwitchCase::Literal(_)))
                {
                    self.error(format!(
                        "{subject} Switch is outside the Copy variant verifier capability"
                    ));
                } else if let Err(message) = self.program.type_catalog().validate_switch(&ty, arms)
                {
                    self.error(format!("{subject} Switch rejected: {message}"));
                }
            }
            MirTerminator::SwitchMove { scrutinee, arms } => {
                let Some(scrutinee_ty) = value_type(function, scrutinee) else {
                    self.error(format!("{subject} SwitchMove scrutinee is absent"));
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_option_string_variant(&scrutinee_ty)
                {
                    self.error(format!("{subject} SwitchMove rejected: {message}"));
                    return;
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_switch_move(&scrutinee_ty, arms)
                {
                    self.error(format!("{subject} SwitchMove rejected: {message}"));
                    return;
                }
                let Some((_, variants)) = self.program.type_catalog().variant_layout(&scrutinee_ty)
                else {
                    self.error(format!(
                        "{subject} SwitchMove has no canonical variant layout"
                    ));
                    return;
                };
                let required = variants
                    .iter()
                    .map(|variant| variant.id.clone())
                    .collect::<BTreeSet<_>>();
                let mut seen = BTreeSet::new();
                if arms.len() != required.len() {
                    self.error(format!(
                        "{subject} SwitchMove requires exactly one explicit arm for each TypeDesc variant"
                    ));
                }
                for arm in arms {
                    let MirSwitchCase::Variant(variant_id) = &arm.case else {
                        self.error(format!(
                            "{subject} SwitchMove requires explicit variant arms; default/literal cases are not covered"
                        ));
                        continue;
                    };
                    let Some(variant) = self
                        .program
                        .type_catalog()
                        .variant(&scrutinee_ty, variant_id)
                    else {
                        self.error(format!(
                            "{subject} SwitchMove variant '{}' is absent from TypeDesc",
                            variant_id.0
                        ));
                        continue;
                    };
                    if !seen.insert(variant.id.clone()) {
                        self.error(format!(
                            "{subject} SwitchMove variant '{}' is repeated",
                            variant.name
                        ));
                    }
                    let Some(target) = function.blocks.get(&arm.target) else {
                        self.error(format!(
                            "{subject} SwitchMove edge target '{}' is absent",
                            arm.target
                        ));
                        continue;
                    };
                    for (index, argument) in arm.arguments.iter().enumerate() {
                        if !function.values.contains_key(argument) {
                            self.error(format!(
                                "{subject} SwitchMove edge argument '{}' is absent",
                                argument
                            ));
                            continue;
                        }
                        let Some(parameter) = target
                            .parameters
                            .get(index)
                            .and_then(|parameter| function.values.get(&parameter.value))
                        else {
                            continue;
                        };
                        if function
                            .values
                            .get(argument)
                            .is_some_and(|value| value.ty != parameter.ty)
                        {
                            self.error(format!(
                                "{subject} SwitchMove edge argument type disagrees with block parameter"
                            ));
                        }
                    }
                    if target.parameters.len() != arm.arguments.len() + arm.bindings.len() {
                        self.error(format!(
                            "{subject} SwitchMove edge arguments and payload bindings disagree with block parameter arity"
                        ));
                    }
                    let mut binding_fields = BTreeSet::new();
                    for (index, binding) in arm.bindings.iter().enumerate() {
                        if !binding_fields.insert(binding.projection.field.clone()) {
                            self.error(format!(
                                "{subject} SwitchMove payload field '{}' is bound more than once",
                                binding.projection.field.0
                            ));
                        }
                        let Some(target_parameter) =
                            target.parameters.get(arm.arguments.len() + index)
                        else {
                            continue;
                        };
                        let Some(parameter) = function.values.get(&target_parameter.value) else {
                            continue;
                        };
                        if binding.parameter != target_parameter.value {
                            self.error(format!(
                                "{subject} SwitchMove binding parameter disagrees with target block parameter"
                            ));
                        }
                        if let Err(message) = self
                            .program
                            .type_catalog()
                            .validate_variant_payload_projection_receipt(
                                &scrutinee_ty,
                                variant_id,
                                &parameter.ty,
                                &binding.projection,
                            )
                        {
                            self.error(format!("{subject} SwitchMove rejected: {message}"));
                        }
                    }
                }
            }
            MirTerminator::Return { .. } | MirTerminator::Trap { .. } => {}
            MirTerminator::Fault { .. } => {
                self.error(format!(
                    "{subject} Fault is outside the verifier capability"
                ));
            }
            MirTerminator::Unreachable => {
                self.error(format!(
                    "{subject} Unreachable is outside the default verifier island"
                ));
            }
        }
    }

    fn validate_acyclic_cfg(&mut self, function: &MirFunction) {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for block in function.blocks.keys() {
            if !visited.contains(block)
                && self.visit_cfg(function, block, &mut visiting, &mut visited)
            {
                self.error(format!(
                    "function '{}' contains a cyclic CFG outside the verifier capability",
                    function.owner.0
                ));
                return;
            }
        }
    }

    fn visit_cfg(
        &self,
        function: &MirFunction,
        block: &crate::core::mir::MirBlockId,
        visiting: &mut BTreeSet<crate::core::mir::MirBlockId>,
        visited: &mut BTreeSet<crate::core::mir::MirBlockId>,
    ) -> bool {
        if !visiting.insert(block.clone()) {
            return true;
        }
        let Some(block_data) = function.blocks.get(block) else {
            visiting.remove(block);
            return false;
        };
        let targets = match &block_data.terminator {
            MirTerminator::Goto { target, .. } => vec![target],
            MirTerminator::Branch {
                then_target,
                else_target,
                ..
            } => {
                vec![then_target, else_target]
            }
            MirTerminator::Switch { arms, .. } | MirTerminator::SwitchMove { arms, .. } => {
                arms.iter().map(|arm| &arm.target).collect()
            }
            MirTerminator::Return { .. }
            | MirTerminator::Trap { .. }
            | MirTerminator::Fault { .. }
            | MirTerminator::Unreachable => Vec::new(),
        };
        for target in targets {
            if !visited.contains(target) && self.visit_cfg(function, target, visiting, visited) {
                return true;
            }
            if visiting.contains(target) {
                return true;
            }
        }
        visiting.remove(block);
        visited.insert(block.clone());
        false
    }

    fn error(&mut self, message: String) {
        self.errors.insert(message);
    }
}

fn is_noop_glue(glue: crate::core::mir::types::MirGlueContract) -> bool {
    glue.move_out == MirGlueKind::Noop
        && glue.clone == MirGlueKind::Noop
        && glue.drop == MirGlueKind::Noop
}

fn value_type(function: &MirFunction, value: &MirValueId) -> Option<crate::core::ResolvedTypeId> {
    function.values.get(value).map(|value| value.ty.clone())
}

fn function_has_ensures(function: &MirFunction) -> bool {
    function
        .contracts
        .iter()
        .any(|contract| contract.kind == MirContractKind::Ensures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn canonical(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    #[test]
    fn accepts_copy_record_and_scalar_call_graph() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_record_copy.mimi"
        ));
        validate_mir_capabilities(&program).expect("copy record island capability");
    }

    #[test]
    fn accepts_non_copy_option_string_variant_contract() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_option_string.mimi"
        ));
        validate_mir_capabilities(&program).expect("Option<string> verifier capability");
    }

    #[test]
    fn accepts_recursive_owned_tuple_contract() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_recursive_tuple.mimi"
        ));
        validate_mir_capabilities(&program).expect("recursive tuple verifier capability");
        let tuple = program
            .type_catalog()
            .iter()
            .find_map(|(ty, descriptor)| {
                (matches!(descriptor.layout, MirLayout::Tuple(ref fields) if fields.len() == 2)
                    && descriptor.ownership == MirOwnership::Move)
                    .then(|| ty.clone())
            })
            .expect("recursive Move tuple TypeDesc");
        program
            .type_catalog()
            .validate_recursive_tuple_abi(&tuple)
            .expect("shared recursive tuple TypeDesc contract");
    }

    #[test]
    fn rejects_recursive_tuple_with_list_child_before_verifier_consumption() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_recursive_tuple_rejected.mimi"
        ));
        let errors = validate_mir_capabilities(&program)
            .expect_err("tuple with List child must remain outside verifier capability");
        assert!(errors
            .iter()
            .any(|error| { error.contains("scalar/String/tuple ABI") || error.contains("List") }));
    }

    #[test]
    fn rejects_non_copy_nested_variant_before_default_route() {
        let program = canonical(
            "func main() -> i32 { let value: Option<(string, i32)> = Some((\"owned\", 41)); drop(value); 42 }",
        );
        let errors =
            validate_mir_capabilities(&program).expect_err("nested Option payload must be gated");
        assert!(errors.iter().any(|error| {
            error.contains("non-Copy variant TypeDesc")
                || error.contains("Option<string> variant contract")
        }));
    }

    #[test]
    fn accepts_indexed_list_projection_with_materialized_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_list_index.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("List index receipt must satisfy verifier capability gate");
    }

    #[test]
    fn accepts_list_operation_with_materialized_receipt() {
        let program = canonical(include_str!(
            "../../tests/fixtures/mir_native_list_len.mimi"
        ));
        validate_mir_capabilities(&program)
            .expect("List operation receipt must satisfy verifier capability gate");
    }
}
