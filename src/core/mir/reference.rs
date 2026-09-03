//! Small, deterministic reference executor for canonical MIR.
//!
//! This is not the production VM. It is intentionally boring and independent
//! of LLVM/runtime code so that bytecode and native lowering can be compared
//! against a third semantic oracle. The supported operation set grows with
//! MIR lowering; unsupported operations fail explicitly.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::core::ir::{
    ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedType, ResolvedUnaryOp,
};
use crate::core::{NodeId, ResolvedPlace};

use super::types::{MirGlueOperation, MirLayout, MirTypeCatalog};
use super::{
    MirAggregateKind, MirFunction, MirGenericInstanceContract, MirInstance, MirInstanceId,
    MirInstruction, MirInstructionKind, MirProjection, MirSwitchArm, MirSwitchCase, MirTerminator,
    MirTransitionContract, MirTransitionEffect, MirValueId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirRuntimeValue {
    Int(i64),
    FloatBits(u64),
    Bool(bool),
    String(String),
    Tuple(Vec<MirRuntimeValue>),
    List(Vec<MirRuntimeValue>),
    Set(Vec<MirRuntimeValue>),
    Record {
        nominal: crate::core::ir::NominalTypeId,
        fields: Vec<MirRuntimeValue>,
    },
    Variant {
        nominal: crate::core::ir::NominalTypeId,
        variant: NodeId,
        payload: Vec<MirRuntimeValue>,
    },
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirExecutionError {
    pub function: NodeId,
    pub message: String,
}

impl std::fmt::Display for MirExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MIR execution in '{}': {}",
            self.function.0, self.message
        )
    }
}

impl std::error::Error for MirExecutionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirExecutionObservation {
    pub value: MirRuntimeValue,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProgramBuildError {
    Lowering(Vec<super::lower::MirLoweringError>),
    Types(Vec<String>),
    Validation(Vec<super::MirValidationError>),
}

impl std::fmt::Display for MirProgramBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lowering(errors) => {
                write!(formatter, "MIR lowering failed ({} errors)", errors.len())
            }
            Self::Types(errors) => write!(
                formatter,
                "MIR type catalog failed ({} errors)",
                errors.len()
            ),
            Self::Validation(errors) => {
                write!(formatter, "MIR validation failed ({} errors)", errors.len())
            }
        }
    }
}

impl std::error::Error for MirProgramBuildError {}

/// A validated collection of concrete MIR functions.
#[derive(Debug, Clone, Default)]
pub struct MirProgram {
    functions: BTreeMap<NodeId, MirFunction>,
    type_catalog: MirTypeCatalog,
    instances: BTreeMap<MirInstanceId, MirInstance>,
    transitions: BTreeMap<NodeId, MirTransitionContract>,
}

impl MirProgram {
    /// Canonical checked-program entry point. This is intentionally the only
    /// constructor that knows how to obtain MIR from frontend artifacts; all
    /// consumers after this point receive validated MIR plus its type catalog.
    pub fn from_checked_program(
        program: &crate::core::CheckedProgram,
    ) -> Result<Self, MirProgramBuildError> {
        let type_catalog =
            MirTypeCatalog::from_checked_program(program).map_err(MirProgramBuildError::Types)?;
        let mut functions = super::lower::lower_program_with_type_catalog(program, &type_catalog)
            .map_err(MirProgramBuildError::Lowering)?;
        let instances = super::lower::materialize_concrete_generic_instances(
            program,
            &type_catalog,
            &mut functions,
        )
        .map_err(MirProgramBuildError::Lowering)?;
        let transitions = materialize_transition_contracts(program, &type_catalog, None)
            .map_err(MirProgramBuildError::Validation)?;
        Self::with_type_catalog_and_instances_and_transitions(
            functions,
            type_catalog,
            instances,
            transitions,
        )
        .map_err(MirProgramBuildError::Validation)
    }

    /// Build canonical MIR while excluding checker callables whose origin is
    /// in a known compatibility source (for example the automatically merged
    /// prelude).  This keeps the migration boundary explicit: every selected
    /// user/imported callable still has to lower and validate, while an
    /// unsupported legacy-only compatibility item cannot poison an otherwise
    /// canonical program.  Calls into an excluded callable remain a hard
    /// error in downstream consumers because it is absent from the MIR graph.
    pub fn from_checked_program_excluding_sources(
        program: &crate::core::CheckedProgram,
        excluded_sources: &HashSet<crate::span::SourceId>,
    ) -> Result<Self, MirProgramBuildError> {
        let type_catalog =
            MirTypeCatalog::from_checked_program(program).map_err(MirProgramBuildError::Types)?;
        let mut functions = BTreeMap::new();
        let mut lowering_errors = Vec::new();
        for (owner, callable) in program.callables() {
            if excluded_sources.contains(&callable.body.root.origin.user_span().source_id) {
                continue;
            }
            // Generic declarations are templates, not executable MIR
            // functions. They are intentionally omitted until the MIR
            // instance table materializes a concrete body and ABI. A call
            // that still targets the template is rejected by the shared
            // call-graph validator below; it must never fall back to an AST
            // or legacy monomorphization path.
            if !callable.signature.generic_parameters.is_empty() {
                continue;
            }
            match super::lower::lower_callable_with_type_catalog(callable, &type_catalog) {
                Ok(function) => {
                    functions.insert(owner.clone(), function);
                }
                Err(mut errors) => lowering_errors.append(&mut errors),
            }
        }
        if !lowering_errors.is_empty() {
            return Err(MirProgramBuildError::Lowering(lowering_errors));
        }
        let instances = super::lower::materialize_concrete_generic_instances_excluding_sources(
            program,
            &type_catalog,
            &mut functions,
            excluded_sources,
        )
        .map_err(MirProgramBuildError::Lowering)?;
        let transitions =
            materialize_transition_contracts(program, &type_catalog, Some(excluded_sources))
                .map_err(MirProgramBuildError::Validation)?;
        Self::with_type_catalog_and_instances_and_transitions(
            functions,
            type_catalog,
            instances,
            transitions,
        )
        .map_err(MirProgramBuildError::Validation)
    }

    /// Internal constructor retained for structural/unit tests that build
    /// hand-written MIR. Production callers must use [`Self::with_type_catalog`]
    /// or [`Self::from_checked_program`].
    #[cfg(test)]
    pub(crate) fn new(
        functions: BTreeMap<NodeId, MirFunction>,
    ) -> Result<Self, Vec<super::MirValidationError>> {
        let mut errors = Vec::new();
        for function in functions.values() {
            if let Err(mut function_errors) = function.validate() {
                errors.append(&mut function_errors);
            }
        }
        if errors.is_empty() {
            Ok(Self {
                functions,
                type_catalog: MirTypeCatalog::default(),
                instances: BTreeMap::new(),
                transitions: BTreeMap::new(),
            })
        } else {
            Err(errors)
        }
    }

    pub fn with_type_catalog(
        functions: BTreeMap<NodeId, MirFunction>,
        type_catalog: MirTypeCatalog,
    ) -> Result<Self, Vec<super::MirValidationError>> {
        Self::with_type_catalog_and_instances(functions, type_catalog, BTreeMap::new())
    }

    pub fn with_type_catalog_and_instances(
        functions: BTreeMap<NodeId, MirFunction>,
        type_catalog: MirTypeCatalog,
        instances: BTreeMap<MirInstanceId, MirInstance>,
    ) -> Result<Self, Vec<super::MirValidationError>> {
        Self::with_type_catalog_and_instances_and_transitions(
            functions,
            type_catalog,
            instances,
            BTreeMap::new(),
        )
    }

    pub fn with_type_catalog_and_instances_and_transitions(
        functions: BTreeMap<NodeId, MirFunction>,
        type_catalog: MirTypeCatalog,
        instances: BTreeMap<MirInstanceId, MirInstance>,
        transitions: BTreeMap<NodeId, MirTransitionContract>,
    ) -> Result<Self, Vec<super::MirValidationError>> {
        let mut errors = Vec::new();
        errors.extend(validate_instance_table(
            &functions,
            &type_catalog,
            &instances,
        ));
        for function in functions.values() {
            if let Err(mut function_errors) = function.validate() {
                errors.append(&mut function_errors);
                continue;
            }
            for value in function.values.values() {
                if type_catalog.get(&value.ty).is_none() {
                    errors.push(super::MirValidationError {
                        subject: value.id.to_string(),
                        message: "value type is absent from MIR type catalog".into(),
                    });
                }
            }
            for value in function.values.values() {
                let Some(descriptor) = type_catalog.get(&value.ty) else {
                    continue;
                };
                if matches!(
                    descriptor.ownership,
                    super::types::MirOwnership::Copy | super::types::MirOwnership::SharedBorrow
                ) {
                    continue;
                }
                for operation in [
                    MirGlueOperation::MoveOut,
                    MirGlueOperation::Clone,
                    MirGlueOperation::Drop,
                ] {
                    if let Err(message) = type_catalog.validate_glue(&value.ty, operation) {
                        errors.push(super::MirValidationError {
                            subject: value.id.to_string(),
                            message,
                        });
                    }
                }
            }
            if type_catalog.get(&function.result).is_none() {
                errors.push(super::MirValidationError {
                    subject: function.owner.0.clone(),
                    message: "function result type is absent from MIR type catalog".into(),
                });
            }
            if super::is_owned_string_return_candidate(function, &type_catalog) {
                if let Err(message) =
                    super::validate_owned_string_return_shape(function, &type_catalog)
                {
                    errors.push(super::MirValidationError {
                        subject: function.owner.0.clone(),
                        message,
                    });
                }
            }
            errors.extend(validate_linear_consumption(function, &type_catalog));
            errors.extend(validate_borrow_usage(function));
            errors.extend(validate_builtin_calls(function, &type_catalog));
            errors.extend(validate_conversions(function, &type_catalog));
            errors.extend(super::contracts::validate_contracts(
                function,
                &type_catalog,
            ));
            for block in function.blocks.values() {
                for instruction in &block.instructions {
                    match &instruction.kind {
                        super::MirInstructionKind::Project {
                            result,
                            base,
                            projection,
                            list_index_contract,
                        } => {
                            let Some(base_value) = function.values.get(base) else {
                                continue;
                            };
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let validation = match projection {
                                super::MirProjection::Index(index) => {
                                    let Some(index_value) = function.values.get(index) else {
                                        errors.push(super::MirValidationError {
                                            subject: instruction.id.to_string(),
                                            message: "List index is absent from MIR value catalog"
                                                .into(),
                                        });
                                        continue;
                                    };
                                    let Some(receipt) = list_index_contract.as_ref() else {
                                        errors.push(super::MirValidationError {
                                            subject: instruction.id.to_string(),
                                            message:
                                                "List index projection has no canonical receipt"
                                                    .into(),
                                        });
                                        continue;
                                    };
                                    type_catalog.validate_list_index_projection_receipt(
                                        &base_value.ty,
                                        &index_value.ty,
                                        &result_value.ty,
                                        receipt,
                                    )
                                }
                                _ => {
                                    if list_index_contract.is_some() {
                                        Err("List index receipt is attached to a non-index projection".into())
                                    } else {
                                        type_catalog.validate_projection(
                                            &base_value.ty,
                                            &result_value.ty,
                                            projection,
                                        )
                                    }
                                }
                            };
                            if let Err(message) = validation {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::MoveProject {
                            result,
                            base,
                            projection,
                        } => {
                            let Some(base_value) = function.values.get(base) else {
                                continue;
                            };
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            if let Err(message) = type_catalog.validate_move_projection(
                                &base_value.ty,
                                &result_value.ty,
                                projection,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::MoveProjectDrop {
                            result,
                            base,
                            projection,
                            contract,
                        } => {
                            let Some(base_value) = function.values.get(base) else {
                                continue;
                            };
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let Some(receipt) = contract.as_ref() else {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message:
                                        "record move/drop projection has no canonical residual receipt"
                                            .into(),
                                });
                                continue;
                            };
                            if let Err(message) = type_catalog
                                .validate_record_move_projection_drop_receipt(
                                    &base_value.ty,
                                    &result_value.ty,
                                    receipt,
                                )
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                            if !matches!(projection, super::MirProjection::Field(_)) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message:
                                        "record move/drop projection requires a direct record field"
                                            .into(),
                                });
                            }
                        }
                        super::MirInstructionKind::VariantProject {
                            result,
                            base,
                            contract,
                        } => {
                            let Some(base_value) = function.values.get(base) else {
                                continue;
                            };
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let Some(receipt) = contract.as_ref() else {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message:
                                        "direct variant projection has no canonical trap receipt"
                                            .into(),
                                });
                                continue;
                            };
                            if let Err(message) = type_catalog
                                .validate_variant_projection_trap_receipt(
                                    &base_value.ty,
                                    &result_value.ty,
                                    receipt,
                                )
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::VariantProjectMove {
                            result,
                            base,
                            contract,
                        } => {
                            let Some(base_value) = function.values.get(base) else {
                                continue;
                            };
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let Some(receipt) = contract.as_ref() else {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message:
                                        "consuming direct variant projection has no canonical move receipt"
                                            .into(),
                                });
                                continue;
                            };
                            if let Err(message) = type_catalog
                                .validate_variant_move_projection_trap_receipt(
                                    &base_value.ty,
                                    &result_value.ty,
                                    receipt,
                                )
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::Load { result, place } => {
                            let local_id =
                                match MirValueId::new(format!("local:{}", place.base.0 .0)) {
                                    Ok(local_id) => local_id,
                                    Err(error) => {
                                        errors.push(super::MirValidationError {
                                            subject: instruction.id.to_string(),
                                            message: error.to_string(),
                                        });
                                        continue;
                                    }
                                };
                            let Some(base_value) = function.values.get(&local_id) else {
                                continue;
                            };
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            if let Err(message) = type_catalog.validate_place(
                                &base_value.ty,
                                &result_value.ty,
                                &place.projections,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::Construct {
                            result,
                            kind,
                            fields,
                        } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let field_types = fields
                                .iter()
                                .filter_map(|field| {
                                    function.values.get(field).map(|value| value.ty.clone())
                                })
                                .collect::<Vec<_>>();
                            if field_types.len() != fields.len() {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "aggregate field is absent from MIR value catalog"
                                        .into(),
                                });
                                continue;
                            }
                            if let Err(message) = type_catalog.validate_aggregate(
                                &result_value.ty,
                                kind,
                                &field_types,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                            if let Err(message) = type_catalog
                                .validate_glue(&result_value.ty, MirGlueOperation::MoveOut)
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::ConstructList {
                            result,
                            elements,
                            list_construct_contract,
                        } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let element_types = elements
                                .iter()
                                .filter_map(|element| {
                                    function.values.get(element).map(|value| value.ty.clone())
                                })
                                .collect::<Vec<_>>();
                            if element_types.len() != elements.len() {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "List element is absent from MIR value catalog".into(),
                                });
                                continue;
                            }
                            let validation = if let Some(receipt) = list_construct_contract.as_ref()
                            {
                                type_catalog.validate_list_construct_receipt(
                                    &result_value.ty,
                                    &element_types,
                                    receipt,
                                )
                            } else {
                                type_catalog
                                    .validate_list_construct(&result_value.ty, &element_types)
                                    .and(Err("List construction has no canonical receipt".into()))
                            };
                            if let Err(message) = validation {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::ListOp {
                            result,
                            operation,
                            list,
                            argument,
                            list_operation_contract,
                        } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let Some(list_value) = function.values.get(list) else {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message:
                                        "List operation receiver is absent from MIR value catalog"
                                            .into(),
                                });
                                continue;
                            };
                            let Some(receipt) = list_operation_contract.as_ref() else {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "List operation has no canonical receipt".into(),
                                });
                                continue;
                            };
                            let argument_ty = argument
                                .as_ref()
                                .and_then(|value| function.values.get(value))
                                .map(|value| value.ty.clone());
                            if let Err(message) = type_catalog
                                .validate_list_operation_receipt_with_argument(
                                    &result_value.ty,
                                    &list_value.ty,
                                    argument_ty.as_ref(),
                                    *operation,
                                    receipt,
                                )
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::VariantPredicate {
                            result,
                            predicate,
                            variant,
                            contract,
                        } => {
                            let (Some(result_value), Some(variant_value)) =
                                (function.values.get(result), function.values.get(variant))
                            else {
                                continue;
                            };
                            let Some(receipt) = contract.as_ref() else {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "Variant predicate has no canonical receipt".into(),
                                });
                                continue;
                            };
                            if let Err(message) = type_catalog.validate_variant_predicate_receipt(
                                &result_value.ty,
                                &variant_value.ty,
                                *predicate,
                                receipt,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::ConstructSet { result, elements } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let element_types = elements
                                .iter()
                                .filter_map(|element| {
                                    function.values.get(element).map(|value| value.ty.clone())
                                })
                                .collect::<Vec<_>>();
                            if element_types.len() != elements.len() {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "Set element is absent from MIR value catalog".into(),
                                });
                                continue;
                            }
                            if let Err(message) = type_catalog
                                .validate_set_construct(&result_value.ty, &element_types)
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::SetOp {
                            result,
                            operation,
                            set,
                            argument,
                        } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let Some(set_value) = function.values.get(set) else {
                                continue;
                            };
                            let argument_ty = argument
                                .as_ref()
                                .and_then(|value| function.values.get(value))
                                .map(|value| &value.ty);
                            if argument.is_some() && argument_ty.is_none() {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message:
                                        "Set operation argument is absent from MIR value catalog"
                                            .into(),
                                });
                                continue;
                            }
                            if let Err(message) = type_catalog.validate_set_operation(
                                &result_value.ty,
                                &set_value.ty,
                                argument_ty,
                                *operation,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::ConstructVariant {
                            result,
                            nominal,
                            variant,
                            fields,
                        } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let field_types = fields
                                .iter()
                                .filter_map(|(_, field)| {
                                    function.values.get(field).map(|value| value.ty.clone())
                                })
                                .collect::<Vec<_>>();
                            let field_ids = fields
                                .iter()
                                .map(|(field, _)| field.clone())
                                .collect::<Vec<_>>();
                            if field_types.len() != fields.len() {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "variant payload is absent from MIR value catalog"
                                        .into(),
                                });
                                continue;
                            }
                            if let Err(message) = type_catalog.validate_variant_construct(
                                &result_value.ty,
                                nominal,
                                variant,
                                &field_ids,
                                &field_types,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                            if let Err(message) = type_catalog
                                .validate_glue(&result_value.ty, MirGlueOperation::MoveOut)
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                            if type_catalog
                                .get(&result_value.ty)
                                .is_some_and(|descriptor| {
                                    descriptor.ownership != super::types::MirOwnership::Copy
                                })
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "copy variant construction cannot produce a non-Copy value; use ConstructVariantMove".into(),
                                });
                            }
                        }
                        super::MirInstructionKind::ConstructVariantMove {
                            result,
                            nominal,
                            variant,
                            fields,
                        } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let field_types = fields
                                .iter()
                                .filter_map(|(_, field)| {
                                    function.values.get(field).map(|value| value.ty.clone())
                                })
                                .collect::<Vec<_>>();
                            let field_ids = fields
                                .iter()
                                .map(|(field, _)| field.clone())
                                .collect::<Vec<_>>();
                            if field_types.len() != fields.len() {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "variant payload is absent from MIR value catalog"
                                        .into(),
                                });
                                continue;
                            }
                            if let Err(message) = type_catalog.validate_variant_construct(
                                &result_value.ty,
                                nominal,
                                variant,
                                &field_ids,
                                &field_types,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                            if type_catalog
                                .get(&result_value.ty)
                                .is_some_and(|descriptor| {
                                    descriptor.ownership == super::types::MirOwnership::Copy
                                })
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message:
                                        "ConstructVariantMove requires a non-Copy variant value"
                                            .into(),
                                });
                            }
                            if let Err(message) = type_catalog
                                .validate_glue(&result_value.ty, MirGlueOperation::MoveOut)
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::UpdateRecord {
                            result,
                            base,
                            kind,
                            fields,
                        } => {
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            let Some(base_value) = function.values.get(base) else {
                                continue;
                            };
                            let field_types = fields
                                .iter()
                                .filter_map(|field| {
                                    function.values.get(field).map(|value| value.ty.clone())
                                })
                                .collect::<Vec<_>>();
                            if field_types.len() != fields.len() {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "record update field is absent from MIR value catalog"
                                        .into(),
                                });
                                continue;
                            }
                            if let Err(message) = type_catalog.validate_record_update(
                                &result_value.ty,
                                &base_value.ty,
                                kind,
                                &field_types,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::Copy { result, source } => {
                            let (Some(result_value), Some(source_value)) =
                                (function.values.get(result), function.values.get(source))
                            else {
                                continue;
                            };
                            if let Err(message) = type_catalog.validate_copy(&source_value.ty) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                            if result_value.ty != source_value.ty {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message: "copy result type disagrees with source type".into(),
                                });
                            }
                        }
                        super::MirInstructionKind::Move { result, source }
                        | super::MirInstructionKind::Clone { result, source } => {
                            let (Some(result_value), Some(source_value)) =
                                (function.values.get(result), function.values.get(source))
                            else {
                                continue;
                            };
                            let operation = if matches!(
                                &instruction.kind,
                                super::MirInstructionKind::Move { .. }
                            ) {
                                MirGlueOperation::MoveOut
                            } else {
                                MirGlueOperation::Clone
                            };
                            if let Err(message) = type_catalog.validate_value_operation(
                                &result_value.ty,
                                &source_value.ty,
                                operation,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::Drop { value } => {
                            let Some(value) = function.values.get(value) else {
                                continue;
                            };
                            if let Err(message) =
                                type_catalog.validate_glue(&value.ty, MirGlueOperation::Drop)
                            {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::Borrow {
                            result,
                            source,
                            mutable,
                        } => {
                            let (Some(result_value), Some(source_value)) =
                                (function.values.get(result), function.values.get(source))
                            else {
                                continue;
                            };
                            if let Err(message) = type_catalog.validate_borrow(
                                &source_value.ty,
                                &result_value.ty,
                                *mutable,
                            ) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                        super::MirInstructionKind::Const { .. }
                        | super::MirInstructionKind::Call { .. }
                        | super::MirInstructionKind::FlowTransition { .. }
                        | super::MirInstructionKind::BuiltinCall { .. }
                        | super::MirInstructionKind::Binary { .. }
                        | super::MirInstructionKind::Unary { .. }
                        | super::MirInstructionKind::Convert { .. }
                        | super::MirInstructionKind::Nop => {}
                        super::MirInstructionKind::EndBorrow { borrow } => {
                            let Some(value) = function.values.get(borrow) else {
                                continue;
                            };
                            if let Err(message) = type_catalog.validate_reference_type(&value.ty) {
                                errors.push(super::MirValidationError {
                                    subject: instruction.id.to_string(),
                                    message,
                                });
                            }
                        }
                    }
                }
                if let super::MirTerminator::Switch { scrutinee, arms }
                | super::MirTerminator::SwitchMove { scrutinee, arms } = &block.terminator
                {
                    let Some(scrutinee_value) = function.values.get(scrutinee) else {
                        continue;
                    };
                    let move_scrutinee =
                        matches!(&block.terminator, super::MirTerminator::SwitchMove { .. });
                    let validation = if move_scrutinee {
                        type_catalog
                            .validate_variant_switch_move_contract(&scrutinee_value.ty, arms)
                    } else {
                        type_catalog.validate_switch(&scrutinee_value.ty, arms)
                    };
                    if let Err(message) = validation {
                        errors.push(super::MirValidationError {
                            subject: block.id.to_string(),
                            message,
                        });
                    }
                    for arm in arms {
                        let Some(target) = function.blocks.get(&arm.target) else {
                            continue;
                        };
                        if target.parameters.len() != arm.arguments.len() + arm.bindings.len() {
                            errors.push(super::MirValidationError {
                                subject: arm.edge.to_string(),
                                message: "switch edge arguments and payload bindings disagree with block parameter arity".into(),
                            });
                        }
                        let variant_id = match &arm.case {
                            super::MirSwitchCase::Variant(variant) => Some(variant),
                            _ => None,
                        };
                        if variant_id.is_none() && !arm.bindings.is_empty() {
                            errors.push(super::MirValidationError {
                                subject: arm.edge.to_string(),
                                message: "switch payload bindings require a canonical variant case"
                                    .into(),
                            });
                            continue;
                        }
                        if let Some(variant_id) = variant_id {
                            for (index, binding) in arm.bindings.iter().enumerate() {
                                let Some(target_parameter) =
                                    target.parameters.get(arm.arguments.len() + index)
                                else {
                                    continue;
                                };
                                let Some(parameter) = function.values.get(&target_parameter.value)
                                else {
                                    continue;
                                };
                                if binding.parameter != target_parameter.value {
                                    errors.push(super::MirValidationError {
                                        subject: arm.edge.to_string(),
                                        message: "switch binding parameter disagrees with target block parameter".into(),
                                    });
                                }
                                if let Err(message) = type_catalog
                                    .validate_variant_payload_projection_receipt(
                                        &scrutinee_value.ty,
                                        variant_id,
                                        &parameter.ty,
                                        &binding.projection,
                                    )
                                {
                                    errors.push(super::MirValidationError {
                                        subject: arm.edge.to_string(),
                                        message,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        if errors.is_empty() {
            errors.extend(validate_call_graph(
                &functions,
                &instances,
                &type_catalog,
                &transitions,
            ));
        }
        if errors.is_empty() {
            Ok(Self {
                functions,
                type_catalog,
                instances,
                transitions,
            })
        } else {
            Err(errors)
        }
    }

    #[cfg(test)]
    pub(crate) fn single(function: MirFunction) -> Result<Self, Vec<super::MirValidationError>> {
        let owner = function.owner.clone();
        Self::new(BTreeMap::from([(owner, function)]))
    }

    pub fn functions(&self) -> &BTreeMap<NodeId, MirFunction> {
        &self.functions
    }

    pub fn type_catalog(&self) -> &MirTypeCatalog {
        &self.type_catalog
    }

    pub fn instances(&self) -> &BTreeMap<MirInstanceId, MirInstance> {
        &self.instances
    }

    pub fn transitions(&self) -> &BTreeMap<NodeId, MirTransitionContract> {
        &self.transitions
    }
}

/// Validate every `Convert` against the closed TypeDesc conversion contract
/// before either the reference executor or a production backend sees it.
/// Existing surface casts such as integer-to-float remain deliberately
/// rejected until their deterministic numeric and trap semantics are
/// materialized here.
fn validate_conversions(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    for block in function.blocks.values() {
        for instruction in &block.instructions {
            let super::MirInstructionKind::Convert { result, source } = &instruction.kind else {
                continue;
            };
            let (Some(source_value), Some(result_value)) =
                (function.values.get(source), function.values.get(result))
            else {
                continue;
            };
            if let Err(message) =
                type_catalog.validate_conversion(&source_value.ty, &result_value.ty)
            {
                errors.push(super::MirValidationError {
                    subject: instruction.id.to_string(),
                    message,
                });
            }
        }
    }
    errors
}

/// Validate the complete semantic contract for every first-class builtin
/// instruction. The validator owns the boundary between checker-resolved
/// types and backend dispatch: arity, exact result identity, TypeDesc ABI,
/// and Copy ownership are checked before execution.
fn validate_builtin_calls(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    for block in function.blocks.values() {
        for instruction in &block.instructions {
            let super::MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
            } = &instruction.kind
            else {
                continue;
            };
            let contract = super::types::MirBuiltinContract::for_kind(*kind);
            if arguments.len() != contract.arity {
                errors.push(super::MirValidationError {
                    subject: instruction.id.to_string(),
                    message: format!(
                        "builtin '{}' supplies {} arguments but its MIR contract requires {}",
                        contract.name,
                        arguments.len(),
                        contract.arity
                    ),
                });
            }
            let result_value = function.values.get(result);
            if result_value.is_none() {
                errors.push(super::MirValidationError {
                    subject: instruction.id.to_string(),
                    message: format!("builtin result '{}' is absent from MIR values", result),
                });
            };
            let mut first_type = None;
            for (index, argument) in arguments.iter().enumerate() {
                let Some(argument_value) = function.values.get(argument) else {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "builtin argument {index} value '{}' is absent from MIR values",
                            argument
                        ),
                    });
                    continue;
                };
                if contract.requires_same_input_type {
                    if let Some(first_type) = &first_type {
                        if first_type != &argument_value.ty {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: format!(
                                    "builtin '{}' arguments must have the same ResolvedTypeId (argument {index} differs)",
                                    contract.name
                                ),
                            });
                        }
                    } else {
                        first_type = Some(argument_value.ty.clone());
                    }
                } else if first_type.is_none() {
                    first_type = Some(argument_value.ty.clone());
                }
                let Some(descriptor) = type_catalog.get(&argument_value.ty) else {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "builtin argument {index} type '{}' is absent from MIR TypeDesc",
                            argument_value.ty.as_str()
                        ),
                    });
                    continue;
                };
                if !contract.accepts_abi(descriptor.abi) {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "builtin '{}' does not support argument {index} ABI {:?}; canonical contract accepts {}",
                            contract.name,
                            descriptor.abi,
                            contract.accepted_abi_description()
                        ),
                    });
                }
                if !contract.accepts_layout(&descriptor.layout) {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "builtin '{}' requires scalar TypeDesc layout for argument {index}, got {:?}",
                            contract.name, descriptor.layout
                        ),
                    });
                }
                if contract.requires_copy
                    && descriptor.ownership != super::types::MirOwnership::Copy
                {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "builtin '{}' requires Copy arguments but argument {index} TypeDesc says {:?}",
                            contract.name, descriptor.ownership
                        ),
                    });
                }
            }
            if let (Some(result_value), Some(first_type)) = (result_value, first_type) {
                if contract.preserves_type && result_value.ty != first_type {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: "builtin result type disagrees with its argument type".into(),
                    });
                }
            }
            if contract.result_must_be_unit {
                let valid_unit = result_value.is_some_and(|value| {
                    type_catalog.get(&value.ty).is_some_and(|descriptor| {
                        descriptor.layout == super::types::MirLayout::Unit
                            && descriptor.abi == super::types::MirAbiClass::Unit
                            && descriptor.ownership == super::types::MirOwnership::Copy
                            && descriptor.glue
                                == (super::types::MirGlueContract {
                                    move_out: super::types::MirGlueKind::Noop,
                                    clone: super::types::MirGlueKind::Noop,
                                    drop: super::types::MirGlueKind::Noop,
                                })
                    })
                });
                if !valid_unit {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "builtin '{}' result must be the canonical Copy unit TypeDesc",
                            contract.name
                        ),
                    });
                }
            }
        }
    }
    errors
}

fn validate_instance_table(
    functions: &BTreeMap<NodeId, MirFunction>,
    type_catalog: &MirTypeCatalog,
    instances: &BTreeMap<MirInstanceId, MirInstance>,
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    let mut executable_functions = BTreeSet::new();
    for (id, instance) in instances {
        let expected_id = match MirInstanceId::for_template(&instance.template, &instance.arguments)
        {
            Ok(expected) => expected,
            Err(error) => {
                errors.push(super::MirValidationError {
                    subject: id.to_string(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        if &expected_id != id || instance.id != *id {
            errors.push(super::MirValidationError {
                subject: id.to_string(),
                message: "generic MIR instance identity disagrees with its template/arguments"
                    .into(),
            });
        }
        let argument_error = match instance.contract {
            MirGenericInstanceContract::ScalarIdentity
            | MirGenericInstanceContract::OwnedStringIdentity => {
                type_catalog.validate_generic_identity_arguments(&instance.arguments)
            }
            MirGenericInstanceContract::ScalarSetFacade { .. } => {
                type_catalog.validate_scalar_generic_arguments(&instance.arguments)
            }
            MirGenericInstanceContract::ScalarListFacade { .. } => {
                type_catalog.validate_scalar_generic_arguments(&instance.arguments)
            }
            MirGenericInstanceContract::ScalarListConstruct { .. } => {
                type_catalog.validate_scalar_generic_arguments(&instance.arguments)
            }
            MirGenericInstanceContract::ScalarListProjection { .. } => {
                type_catalog.validate_scalar_generic_arguments(&instance.arguments)
            }
            MirGenericInstanceContract::ScalarRecordProjection { .. } => {
                type_catalog.validate_scalar_generic_arguments(&instance.arguments)
            }
            MirGenericInstanceContract::OwnedRecordProjection { .. } => {
                if instance.arguments.len() != 1 {
                    Err(format!(
                        "owned generic record projection contract requires one type argument, got {}",
                        instance.arguments.len()
                    ))
                } else {
                    type_catalog.validate_owned_string(&instance.arguments[0])
                }
            }
            MirGenericInstanceContract::ScalarVariantPredicate { .. } => {
                type_catalog.validate_scalar_generic_arguments(&instance.arguments)
            }
        };
        if let Err(message) = argument_error {
            errors.push(super::MirValidationError {
                subject: id.to_string(),
                message: format!("generic MIR instance TypeDesc contract is invalid: {message}"),
            });
        }
        let Some(function) = functions.get(&instance.function) else {
            errors.push(super::MirValidationError {
                subject: id.to_string(),
                message: format!(
                    "generic MIR instance executable function '{}' is absent",
                    instance.function.0
                ),
            });
            continue;
        };
        if !executable_functions.insert(instance.function.clone()) {
            errors.push(super::MirValidationError {
                subject: id.to_string(),
                message: format!(
                    "generic MIR instance executable function '{}' is bound by multiple instance entries",
                    instance.function.0
                ),
            });
        }
        if function.owner != instance.function {
            errors.push(super::MirValidationError {
                subject: id.to_string(),
                message: "generic MIR instance function owner disagrees with its table entry"
                    .into(),
            });
        }
        match instance.contract {
            MirGenericInstanceContract::ScalarIdentity => {
                errors.extend(validate_generic_identity_instance_function(
                    id,
                    function,
                    &instance.arguments,
                ));
            }
            MirGenericInstanceContract::OwnedStringIdentity => {
                if let Some(concrete) = instance.arguments.first() {
                    if let Err(message) = type_catalog.validate_owned_string(concrete) {
                        errors.push(super::MirValidationError {
                            subject: id.to_string(),
                            message: format!(
                                "owned String generic identity TypeDesc contract is invalid: {message}"
                            ),
                        });
                    }
                }
                errors.extend(validate_owned_string_identity_instance_function(
                    id,
                    function,
                    &instance.arguments,
                ));
            }
            MirGenericInstanceContract::ScalarSetFacade { operation } => {
                if let Err(message) =
                    super::lower::validate_scalar_set_facade_mir(function, type_catalog, operation)
                {
                    errors.push(super::MirValidationError {
                        subject: id.to_string(),
                        message: format!("generic MIR Set facade contract is invalid: {message}"),
                    });
                }
            }
            MirGenericInstanceContract::ScalarListFacade { operation } => {
                if let Err(message) =
                    super::lower::validate_scalar_list_facade_mir(function, type_catalog, operation)
                {
                    errors.push(super::MirValidationError {
                        subject: id.to_string(),
                        message: format!("generic MIR List facade contract is invalid: {message}"),
                    });
                }
            }
            MirGenericInstanceContract::ScalarListConstruct { ref contract } => {
                if let Err(message) = super::lower::validate_scalar_list_construct_mir(
                    function,
                    type_catalog,
                    contract,
                ) {
                    errors.push(super::MirValidationError {
                        subject: id.to_string(),
                        message: format!(
                            "generic MIR List construction contract is invalid: {message}"
                        ),
                    });
                }
            }
            MirGenericInstanceContract::ScalarListProjection {
                ref contract,
                index_value,
            } => {
                if let Err(message) = super::lower::validate_scalar_list_projection_mir(
                    function,
                    type_catalog,
                    contract,
                    index_value,
                ) {
                    errors.push(super::MirValidationError {
                        subject: id.to_string(),
                        message: format!(
                            "generic MIR List projection contract is invalid: {message}"
                        ),
                    });
                }
            }
            MirGenericInstanceContract::ScalarRecordProjection { ref contract } => {
                if let Err(message) = super::lower::validate_scalar_record_projection_mir(
                    function,
                    type_catalog,
                    contract,
                ) {
                    errors.push(super::MirValidationError {
                        subject: id.to_string(),
                        message: format!(
                            "generic MIR record projection contract is invalid: {message}"
                        ),
                    });
                }
            }
            MirGenericInstanceContract::OwnedRecordProjection { ref contract } => {
                if let Err(message) = super::lower::validate_owned_record_projection_mir(
                    function,
                    type_catalog,
                    contract,
                ) {
                    errors.push(super::MirValidationError {
                        subject: id.to_string(),
                        message: format!(
                            "generic MIR owned record projection contract is invalid: {message}"
                        ),
                    });
                }
            }
            MirGenericInstanceContract::ScalarVariantPredicate { ref contract } => {
                if let Err(message) = super::lower::validate_scalar_variant_predicate_mir(
                    function,
                    type_catalog,
                    contract,
                ) {
                    errors.push(super::MirValidationError {
                        subject: id.to_string(),
                        message: format!(
                            "generic MIR Option predicate contract is invalid: {message}"
                        ),
                    });
                }
            }
        }
    }
    errors
}

fn validate_generic_identity_instance_function(
    instance_id: &MirInstanceId,
    function: &MirFunction,
    arguments: &[crate::core::ResolvedTypeId],
) -> Vec<super::MirValidationError> {
    let subject = instance_id.to_string();
    let Some(concrete) = arguments.first() else {
        return vec![super::MirValidationError {
            subject,
            message: "generic MIR identity instance has no concrete argument".into(),
        }];
    };
    super::validate_generic_identity_shape(function, concrete)
        .err()
        .map(|message| vec![super::MirValidationError { subject, message }])
        .unwrap_or_default()
}

fn validate_owned_string_identity_instance_function(
    instance_id: &MirInstanceId,
    function: &MirFunction,
    arguments: &[crate::core::ResolvedTypeId],
) -> Vec<super::MirValidationError> {
    let subject = instance_id.to_string();
    let Some(concrete) = arguments.first() else {
        return vec![super::MirValidationError {
            subject,
            message: "owned String generic identity instance has no concrete argument".into(),
        }];
    };
    super::validate_owned_string_identity_shape(function, concrete)
        .err()
        .map(|message| vec![super::MirValidationError { subject, message }])
        .unwrap_or_default()
}

/// Validate the canonical intra-program call ABI before a backend sees MIR.
///
/// A MIR `Call` is deliberately narrower than the surface callable universe
/// in this migration slice: it must target another materialized MIR function,
/// and its argument/result value types must match that function's canonical
/// signature exactly.  This keeps missing targets, builtin/actor/effect calls,
/// and ABI drift from being rediscovered independently by reference, bytecode,
/// or native consumers.  Such shapes remain fail-closed until their own MIR
/// effect and ABI contracts are materialized.
fn validate_call_graph(
    functions: &BTreeMap<NodeId, MirFunction>,
    instances: &BTreeMap<MirInstanceId, MirInstance>,
    type_catalog: &MirTypeCatalog,
    transitions: &BTreeMap<NodeId, MirTransitionContract>,
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    errors.extend(validate_transition_contracts(
        functions,
        type_catalog,
        transitions,
    ));
    for function in functions.values() {
        for (_block_id, block) in &function.blocks {
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                if let super::MirInstructionKind::FlowTransition {
                    result,
                    transition,
                    arguments,
                } = &instruction.kind
                {
                    validate_flow_transition_instruction(
                        function,
                        type_catalog,
                        functions,
                        transitions,
                        result,
                        transition,
                        arguments,
                        &instruction.id.to_string(),
                        &mut errors,
                    );
                    continue;
                }
                let super::MirInstructionKind::Call {
                    result,
                    callee,
                    type_arguments,
                    arguments,
                    variant_call_contract,
                } = &instruction.kind
                else {
                    continue;
                };
                let ResolvedCallee::Function(target_owner) = callee else {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!("callee '{callee:?}' is not a materialized MIR function"),
                    });
                    continue;
                };
                let Some(target) = functions.get(target_owner) else {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "callee '{}' is absent from the canonical MIR program",
                            target_owner.0
                        ),
                    });
                    continue;
                };

                let target_parameter_types = target
                    .parameters
                    .iter()
                    .filter_map(|parameter| target.values.get(parameter))
                    .map(|value| value.ty.clone())
                    .collect::<Vec<_>>();
                let flat_variant_result = type_catalog
                    .validate_flat_copy_variant(&target.result)
                    .is_ok();
                let move_owned_result = type_catalog
                    .validate_result_string_i32_variant(&target.result)
                    .is_ok();
                if flat_variant_result || move_owned_result {
                    let Some(receipt) = variant_call_contract.as_ref() else {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message:
                                if flat_variant_result {
                                    "call returning flat Copy Option/Result has no canonical ABI receipt".into()
                                } else {
                                    "call returning move-owned Result<string, i32> has no canonical ABI receipt".into()
                                },
                        });
                        continue;
                    };
                    if let Err(message) = type_catalog.validate_variant_call_abi_receipt(
                        target_owner,
                        type_arguments,
                        &target_parameter_types,
                        &target.result,
                        receipt,
                    ) {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message,
                        });
                    }
                    if move_owned_result {
                        if let Err(message) =
                            super::validate_move_owned_result_return_merge(target, type_catalog)
                        {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message,
                            });
                        }
                    }
                } else if variant_call_contract.is_some() {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message:
                            "variant call ABI receipt is attached to an unsupported variant result"
                                .into(),
                    });
                } else if matches!(
                    type_catalog.get(&target.result),
                    Some(descriptor)
                        if descriptor.kind == super::types::MirTypeKind::Result
                            && descriptor.ownership != super::types::MirOwnership::Copy
                ) {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message:
                            "non-Copy Result call result is outside the canonical call ABI contract"
                                .into(),
                    });
                }

                let target_instance = instances
                    .values()
                    .find(|instance| instance.function == *target_owner);
                if type_arguments.is_empty() {
                    if target_instance.is_some() {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "call to generic MIR instance '{}' omits its canonical type arguments",
                                target_owner.0
                            ),
                        });
                    }
                } else {
                    let Some(instance) = target_instance else {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "call to non-instance MIR function '{}' carries generic type arguments",
                                target_owner.0
                            ),
                        });
                        continue;
                    };
                    if instance.arguments != *type_arguments {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "call generic arguments disagree with MIR instance '{}', expected [{}]",
                                target_owner.0,
                                instance
                                    .arguments
                                    .iter()
                                    .map(|argument| argument.as_str())
                                    .collect::<Vec<_>>()
                                    .join(",")
                            ),
                        });
                    }
                    if let MirGenericInstanceContract::OwnedRecordProjection { .. } =
                        &instance.contract
                    {
                        let Some(target_parameter) = target.parameters.first() else {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: "owned generic record projection target has no parameter"
                                    .into(),
                            });
                            continue;
                        };
                        let Some(target_parameter_ty) = target
                            .values
                            .get(target_parameter)
                            .map(|value| value.ty.clone())
                        else {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: "owned generic record projection target parameter TypeDesc is absent".into(),
                            });
                            continue;
                        };
                        if let Err(message) = super::lower::validate_owned_record_call_argument(
                            function,
                            block,
                            instruction_index,
                            &target_parameter_ty,
                            type_catalog,
                        ) {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: format!(
                                    "owned generic record projection call transfer is invalid: {message}"
                                ),
                            });
                        }
                    } else if let MirGenericInstanceContract::ScalarRecordProjection { .. } =
                        &instance.contract
                    {
                        let Some(target_parameter) = target.parameters.first() else {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: "generic scalar record projection target has no parameter"
                                    .into(),
                            });
                            continue;
                        };
                        let Some(target_parameter_ty) = target
                            .values
                            .get(target_parameter)
                            .map(|value| value.ty.clone())
                        else {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: "generic scalar record projection target parameter TypeDesc is absent".into(),
                            });
                            continue;
                        };
                        if let Err(message) = super::lower::validate_scalar_record_call_argument(
                            function,
                            block,
                            instruction_index,
                            &target_parameter_ty,
                            type_catalog,
                        ) {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: format!(
                                    "generic scalar record projection call transfer is invalid: {message}"
                                ),
                            });
                        }
                    }
                }

                if arguments.len() != target.parameters.len() {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "call to '{}' supplies {} arguments but its MIR signature requires {}",
                            target_owner.0,
                            arguments.len(),
                            target.parameters.len()
                        ),
                    });
                }
                for (index, (argument, parameter)) in
                    arguments.iter().zip(target.parameters.iter()).enumerate()
                {
                    let Some(argument_value) = function.values.get(argument) else {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "call argument {index} value '{}' is absent from the caller",
                                argument
                            ),
                        });
                        continue;
                    };
                    let Some(parameter_value) = target.values.get(parameter) else {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "callee '{}' parameter {index} value '{}' is absent from its MIR value catalog",
                                target_owner.0, parameter
                            ),
                        });
                        continue;
                    };
                    if argument_value.ty != parameter_value.ty {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "call argument {index} type '{}' disagrees with callee '{}' parameter type '{}'",
                                argument_value.ty.as_str(),
                                target_owner.0,
                                parameter_value.ty.as_str()
                            ),
                        });
                    }
                }

                if let Some(result) = result {
                    let Some(result_value) = function.values.get(result) else {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "call result value '{}' is absent from the caller",
                                result
                            ),
                        });
                        continue;
                    };
                    if result_value.ty != target.result {
                        errors.push(super::MirValidationError {
                            subject: instruction.id.to_string(),
                            message: format!(
                                "call result type '{}' disagrees with callee '{}' result type '{}'",
                                result_value.ty.as_str(),
                                target_owner.0,
                                target.result.as_str()
                            ),
                        });
                    }
                }
            }
        }
    }
    errors
}

fn validate_transition_contracts(
    functions: &BTreeMap<NodeId, MirFunction>,
    type_catalog: &MirTypeCatalog,
    transitions: &BTreeMap<NodeId, MirTransitionContract>,
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    for (owner, contract) in transitions {
        let subject = owner.0.clone();
        if owner != &contract.owner {
            errors.push(super::MirValidationError {
                subject: subject.clone(),
                message: "transition contract owner key disagrees with its owner identity".into(),
            });
        }
        let Some(target) = functions.get(&contract.owner) else {
            errors.push(super::MirValidationError {
                subject: subject.clone(),
                message: "transition executable body is absent from the canonical MIR program"
                    .into(),
            });
            continue;
        };
        for ty in std::iter::once(&contract.source)
            .chain(contract.parameters.iter())
            .chain(std::iter::once(&contract.result))
            .chain(contract.targets.iter())
            .chain(contract.failure.iter())
        {
            if type_catalog.get(ty).is_none() {
                errors.push(super::MirValidationError {
                    subject: subject.clone(),
                    message: format!(
                        "transition contract refers to TypeDesc '{}' absent from the catalog",
                        ty.as_str()
                    ),
                });
            }
        }
        if contract.parameters.first() != Some(&contract.source)
            || contract.parameters.len() != target.parameters.len()
        {
            errors.push(super::MirValidationError {
                subject: subject.clone(),
                message:
                    "transition source/parameter contract disagrees with executable MIR signature"
                        .into(),
            });
        }
        for (actual, expected) in contract.parameters.iter().zip(&target.parameters) {
            if target
                .values
                .get(expected)
                .is_none_or(|value| value.ty != *actual)
            {
                errors.push(super::MirValidationError {
                    subject: subject.clone(),
                    message:
                        "transition parameter TypeDesc disagrees with executable MIR signature"
                            .into(),
                });
                break;
            }
        }
        if contract.result != target.result {
            errors.push(super::MirValidationError {
                subject: subject.clone(),
                message: "transition result TypeDesc disagrees with executable MIR signature"
                    .into(),
            });
        }
        match contract.effect {
            MirTransitionEffect::SilentLocal => {
                if contract.targets.len() != 1
                    || contract.failure.is_some()
                    || contract.is_fallback
                    || contract.is_ffi_pinned
                    || contract.targets.first() != Some(&contract.result)
                {
                    errors.push(super::MirValidationError {
                        subject,
                        message: "silent-local transition must be one target, non-failing, non-fallback, non-pinned, and return that target state".into(),
                    });
                }
            }
            MirTransitionEffect::Boundary => {
                errors.push(super::MirValidationError {
                    subject,
                    message:
                        "transition boundary effect is outside the implemented canonical MIR island"
                            .into(),
                });
            }
        }
    }
    errors
}

fn validate_flow_transition_instruction(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    functions: &BTreeMap<NodeId, MirFunction>,
    transitions: &BTreeMap<NodeId, MirTransitionContract>,
    result: &MirValueId,
    transition: &NodeId,
    arguments: &[MirValueId],
    subject: &str,
    errors: &mut Vec<super::MirValidationError>,
) {
    let Some(contract) = transitions.get(transition) else {
        errors.push(super::MirValidationError {
            subject: subject.into(),
            message: format!(
                "transition '{}' has no canonical MIR contract",
                transition.0
            ),
        });
        return;
    };
    if contract.effect != MirTransitionEffect::SilentLocal
        || contract.targets.len() != 1
        || contract.failure.is_some()
        || contract.is_fallback
        || contract.is_ffi_pinned
    {
        errors.push(super::MirValidationError {
            subject: subject.into(),
            message: "FlowTransition instruction is outside the silent-local transition island"
                .into(),
        });
    }
    let Some(target) = functions.get(&contract.owner) else {
        errors.push(super::MirValidationError {
            subject: subject.into(),
            message: "FlowTransition target body is absent from the canonical MIR program".into(),
        });
        return;
    };
    if arguments.len() != target.parameters.len() {
        errors.push(super::MirValidationError {
            subject: subject.into(),
            message: "FlowTransition argument arity disagrees with its contract".into(),
        });
    }
    for (argument, parameter) in arguments.iter().zip(&target.parameters) {
        let Some(actual) = function.values.get(argument) else {
            errors.push(super::MirValidationError {
                subject: subject.into(),
                message: format!(
                    "FlowTransition argument '{}' is absent from caller values",
                    argument
                ),
            });
            continue;
        };
        let Some(expected) = target.values.get(parameter) else {
            continue;
        };
        if actual.ty != expected.ty {
            errors.push(super::MirValidationError {
                subject: subject.into(),
                message: "FlowTransition argument TypeDesc disagrees with transition parameter"
                    .into(),
            });
        }
    }
    let Some(result_value) = function.values.get(result) else {
        errors.push(super::MirValidationError {
            subject: subject.into(),
            message: "FlowTransition result is absent from caller values".into(),
        });
        return;
    };
    if result_value.ty != contract.result || result_value.ty != target.result {
        errors.push(super::MirValidationError {
            subject: subject.into(),
            message: "FlowTransition result TypeDesc disagrees with transition result".into(),
        });
    }
    if type_catalog.get(&result_value.ty).is_none() {
        errors.push(super::MirValidationError {
            subject: subject.into(),
            message: "FlowTransition result TypeDesc is absent from the catalog".into(),
        });
    }
}

fn materialize_transition_contracts(
    program: &crate::core::CheckedProgram,
    type_catalog: &MirTypeCatalog,
    excluded_sources: Option<&HashSet<crate::span::SourceId>>,
) -> Result<BTreeMap<NodeId, MirTransitionContract>, Vec<super::MirValidationError>> {
    let mut transitions = BTreeMap::new();
    let mut errors = Vec::new();
    let mut resolved = program.transitions().values().collect::<Vec<_>>();
    resolved.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    for transition in resolved {
        if excluded_sources
            .is_some_and(|excluded| excluded.contains(&transition.origin.user_span().source_id))
        {
            continue;
        }
        // The checker catalog also contains generated matrix transitions that
        // are declaration-only.  They are not executable MIR and must not be
        // advertised as contracts that every consumer has to resolve.  Only a
        // checker-owned ResolvedBody can enter the canonical transition island.
        if program.resolved_body(&transition.node_id).is_none() {
            continue;
        }
        let Some(signature) = program.resolved_signature(&transition.node_id) else {
            errors.push(super::MirValidationError {
                subject: transition.node_id.0.clone(),
                message: "resolved transition signature is absent".into(),
            });
            continue;
        };
        let state_type = |state: &crate::core::StateId| {
            let nominal =
                crate::core::NominalTypeId::new(format!("state:{}::{}", state.flow.0, state.name))
                    .ok()?;
            program
                .resolved_types()
                .iter()
                .find_map(|(id, ty)| match ty {
                    ResolvedType::Nominal { item, .. } if *item == nominal => Some(id.clone()),
                    _ => None,
                })
        };
        let Some(source) = state_type(&transition.id.source) else {
            errors.push(super::MirValidationError {
                subject: transition.node_id.0.clone(),
                message: "transition source state has no canonical TypeDesc identity".into(),
            });
            continue;
        };
        let targets = transition
            .targets
            .iter()
            .filter_map(|state| state_type(state))
            .collect::<Vec<_>>();
        if targets.len() != transition.targets.len() {
            errors.push(super::MirValidationError {
                subject: transition.node_id.0.clone(),
                message: "transition target state has no canonical TypeDesc identity".into(),
            });
            continue;
        }
        let parameters = signature
            .parameters
            .iter()
            .map(|parameter| parameter.ty.clone())
            .collect::<Vec<_>>();
        let failure = if transition.fails.is_some() {
            match program.resolved_types().get(&signature.result) {
                Some(ResolvedType::Result { error, .. }) => Some(error.clone()),
                _ => {
                    errors.push(super::MirValidationError {
                        subject: transition.node_id.0.clone(),
                        message: "failing transition signature is not a canonical Result".into(),
                    });
                    None
                }
            }
        } else {
            None
        };
        for ty in std::iter::once(&source)
            .chain(parameters.iter())
            .chain(std::iter::once(&signature.result))
            .chain(targets.iter())
            .chain(failure.iter())
        {
            if type_catalog.get(ty).is_none() {
                errors.push(super::MirValidationError {
                    subject: transition.node_id.0.clone(),
                    message: format!("transition contract TypeDesc '{}' is absent", ty.as_str()),
                });
            }
        }
        transitions.insert(
            transition.node_id.clone(),
            MirTransitionContract {
                owner: transition.node_id.clone(),
                source,
                parameters,
                result: signature.result.clone(),
                targets,
                failure,
                effect: if transition.silent_transition {
                    MirTransitionEffect::SilentLocal
                } else {
                    MirTransitionEffect::Boundary
                },
                is_fallback: transition.is_fallback,
                is_ffi_pinned: transition.is_ffi_pinned,
            },
        );
    }
    if errors.is_empty() {
        Ok(transitions)
    } else {
        Err(errors)
    }
}

/// Validate explicit ownership boundaries before any execution backend sees
/// MIR. The state is a set of consumed non-Copy value identities and is
/// propagated through every CFG edge to a fixed point. Joining states uses
/// union: if any incoming path has consumed a value, a later use is rejected
/// unless a block parameter explicitly rebinds it. This makes branch joins and
/// loop back-edges fail closed instead of relying on block-local bookkeeping.
/// Aggregate destructuring and partial moves remain fail-closed until their
/// own field-level contract is materialized.
fn validate_linear_consumption(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    let mut seen_errors = BTreeSet::new();
    let mut incoming = function
        .blocks
        .keys()
        .cloned()
        .map(|block| (block, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut worklist = function.blocks.keys().cloned().collect::<VecDeque<_>>();
    let mut queued = function.blocks.keys().cloned().collect::<BTreeSet<_>>();

    while let Some(block_id) = worklist.pop_front() {
        queued.remove(&block_id);
        let Some(block) = function.blocks.get(&block_id) else {
            continue;
        };
        let mut consumed = incoming.get(&block_id).cloned().unwrap_or_default();
        // A block parameter is a fresh value on each incoming edge. In
        // particular, this resets a parameter on a loop back-edge rather than
        // mistaking the previous iteration's value for the next one.
        for parameter in &block.parameters {
            consumed.remove(&parameter.value);
        }

        for instruction in &block.instructions {
            let sources = consumed_sources(&instruction.kind);
            consume_values(
                function,
                type_catalog,
                &mut consumed,
                &sources,
                instruction.id.to_string(),
                &mut errors,
                &mut seen_errors,
            );
            if let Some(result) = produced_value(&instruction.kind) {
                // MIR structural validation guarantees a single definition;
                // a produced value is live until a later consuming operation.
                consumed.remove(result);
            }
        }

        match &block.terminator {
            super::MirTerminator::Goto {
                target,
                arguments,
                edge,
            } => propagate_edge(
                function,
                type_catalog,
                &consumed,
                target,
                arguments,
                edge.to_string(),
                &mut incoming,
                &mut worklist,
                &mut queued,
                &mut errors,
                &mut seen_errors,
            ),
            super::MirTerminator::Branch {
                then_target,
                then_arguments,
                then_edge,
                else_target,
                else_arguments,
                else_edge,
                ..
            } => {
                propagate_edge(
                    function,
                    type_catalog,
                    &consumed,
                    then_target,
                    then_arguments,
                    then_edge.to_string(),
                    &mut incoming,
                    &mut worklist,
                    &mut queued,
                    &mut errors,
                    &mut seen_errors,
                );
                propagate_edge(
                    function,
                    type_catalog,
                    &consumed,
                    else_target,
                    else_arguments,
                    else_edge.to_string(),
                    &mut incoming,
                    &mut worklist,
                    &mut queued,
                    &mut errors,
                    &mut seen_errors,
                );
            }
            super::MirTerminator::Switch {
                scrutinee, arms, ..
            } => {
                if is_non_copy(function, type_catalog, scrutinee) {
                    push_ownership_error(
                        &mut errors,
                        &mut seen_errors,
                        block.id.to_string(),
                        format!(
                            "switch scrutinee '{}' is non-Copy but aggregate match glue is not materialized",
                            scrutinee
                        ),
                    );
                }
                for arm in arms {
                    propagate_edge(
                        function,
                        type_catalog,
                        &consumed,
                        &arm.target,
                        &arm.arguments,
                        arm.edge.to_string(),
                        &mut incoming,
                        &mut worklist,
                        &mut queued,
                        &mut errors,
                        &mut seen_errors,
                    );
                }
            }
            super::MirTerminator::SwitchMove {
                scrutinee, arms, ..
            } => {
                let mut after_switch = consumed;
                consume_values(
                    function,
                    type_catalog,
                    &mut after_switch,
                    std::slice::from_ref(scrutinee),
                    block.id.to_string(),
                    &mut errors,
                    &mut seen_errors,
                );
                for arm in arms {
                    propagate_edge(
                        function,
                        type_catalog,
                        &after_switch,
                        &arm.target,
                        &arm.arguments,
                        arm.edge.to_string(),
                        &mut incoming,
                        &mut worklist,
                        &mut queued,
                        &mut errors,
                        &mut seen_errors,
                    );
                }
            }
            super::MirTerminator::Return { value: Some(value) } => consume_values(
                function,
                type_catalog,
                &mut consumed,
                std::slice::from_ref(value),
                block.id.to_string(),
                &mut errors,
                &mut seen_errors,
            ),
            super::MirTerminator::Return { value: None }
            | super::MirTerminator::Trap { .. }
            | super::MirTerminator::Fault { .. }
            | super::MirTerminator::Unreachable => {}
        }
    }
    errors
}

fn consumed_sources(kind: &super::MirInstructionKind) -> Vec<MirValueId> {
    match kind {
        super::MirInstructionKind::Move { source, .. }
        | super::MirInstructionKind::Drop { value: source }
        | super::MirInstructionKind::MoveProject { base: source, .. }
        | super::MirInstructionKind::MoveProjectDrop { base: source, .. }
        | super::MirInstructionKind::VariantProjectMove { base: source, .. } => {
            vec![source.clone()]
        }
        super::MirInstructionKind::Call { arguments, .. }
        | super::MirInstructionKind::FlowTransition { arguments, .. }
        | super::MirInstructionKind::BuiltinCall { arguments, .. }
        | super::MirInstructionKind::Construct {
            fields: arguments, ..
        } => arguments.clone(),
        super::MirInstructionKind::ConstructList { elements, .. } => elements.clone(),
        super::MirInstructionKind::ListOp {
            operation: super::MirListOperation::Concat,
            list,
            argument: Some(argument),
            ..
        } => vec![list.clone(), argument.clone()],
        super::MirInstructionKind::ListOp { .. } => Vec::new(),
        super::MirInstructionKind::ConstructSet { elements, .. } => elements.clone(),
        super::MirInstructionKind::SetOp {
            operation,
            set,
            argument,
            ..
        } if matches!(
            operation,
            super::MirSetOperation::Insert | super::MirSetOperation::Remove
        ) =>
        {
            let mut sources = vec![set.clone()];
            if let Some(argument) = argument {
                sources.push(argument.clone());
            }
            sources
        }
        super::MirInstructionKind::ConstructVariant { fields, .. }
        | super::MirInstructionKind::ConstructVariantMove { fields, .. } => {
            fields.iter().map(|(_, value)| value.clone()).collect()
        }
        super::MirInstructionKind::UpdateRecord {
            base,
            fields: arguments,
            ..
        } => {
            let mut sources = Vec::with_capacity(arguments.len() + 1);
            sources.push(base.clone());
            sources.extend(arguments.iter().cloned());
            sources
        }
        _ => Vec::new(),
    }
}

fn validate_borrow_usage(function: &MirFunction) -> Vec<super::MirValidationError> {
    let borrow_values = function
        .blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .map(move |instruction| (block, instruction))
        })
        .filter_map(|(block, instruction)| match &instruction.kind {
            super::MirInstructionKind::Borrow { result, .. } => {
                Some((result.clone(), instruction.id.to_string(), block.id.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut errors = Vec::new();
    for (borrow, definition, definition_block) in borrow_values {
        for block in function.blocks.values() {
            let mut ended = false;
            for instruction in &block.instructions {
                if produced_value(&instruction.kind) == Some(&borrow) {
                    continue;
                }
                if !instruction_uses_value(&instruction.kind, &borrow) {
                    continue;
                }
                if block.id != definition_block {
                    errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "borrow value '{}' from '{}' escapes through a basic block; only same-block Dereference or EndBorrow may use it",
                            borrow, definition
                        ),
                    });
                    continue;
                }
                match &instruction.kind {
                    super::MirInstructionKind::Project {
                        base,
                        projection: super::MirProjection::Dereference,
                        ..
                    } if base == &borrow => {
                        if ended {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: format!(
                                    "borrow value '{}' from '{}' is used after EndBorrow",
                                    borrow, definition
                                ),
                            });
                        }
                    }
                    super::MirInstructionKind::EndBorrow { borrow: value }
                        if value == &borrow =>
                    {
                        if ended {
                            errors.push(super::MirValidationError {
                                subject: instruction.id.to_string(),
                                message: format!(
                                    "borrow value '{}' from '{}' has more than one EndBorrow",
                                    borrow, definition
                                ),
                            });
                        }
                        ended = true;
                    }
                    _ => errors.push(super::MirValidationError {
                        subject: instruction.id.to_string(),
                        message: format!(
                            "borrow value '{}' from '{}' escapes; only Dereference or EndBorrow may use it",
                            borrow, definition
                        ),
                    }),
                }
            }
            if terminator_uses_value(&block.terminator, &borrow) {
                errors.push(super::MirValidationError {
                    subject: block.id.to_string(),
                    message: format!(
                        "borrow value '{}' from '{}' escapes through a control-flow edge or terminator",
                        borrow, definition
                    ),
                });
            }
        }
    }
    errors
}

fn instruction_uses_value(kind: &super::MirInstructionKind, needle: &MirValueId) -> bool {
    match kind {
        super::MirInstructionKind::Const { .. }
        | super::MirInstructionKind::Load { .. }
        | super::MirInstructionKind::Nop => false,
        super::MirInstructionKind::Copy { source, .. }
        | super::MirInstructionKind::Move { source, .. }
        | super::MirInstructionKind::Clone { source, .. }
        | super::MirInstructionKind::Convert { source, .. } => source == needle,
        super::MirInstructionKind::Drop { value }
        | super::MirInstructionKind::EndBorrow { borrow: value } => value == needle,
        super::MirInstructionKind::Borrow { source, .. } => source == needle,
        super::MirInstructionKind::Project {
            base, projection, ..
        }
        | super::MirInstructionKind::MoveProject {
            base, projection, ..
        } => {
            base == needle
                || matches!(projection, super::MirProjection::Index(index) if index == needle)
        }
        super::MirInstructionKind::MoveProjectDrop {
            base, projection, ..
        } => {
            base == needle
                || matches!(projection, super::MirProjection::Index(index) if index == needle)
        }
        super::MirInstructionKind::VariantProject { base, .. }
        | super::MirInstructionKind::VariantProjectMove { base, .. } => base == needle,
        super::MirInstructionKind::Construct { fields, .. } => fields.iter().any(|v| v == needle),
        super::MirInstructionKind::ConstructList { elements, .. } => {
            elements.iter().any(|v| v == needle)
        }
        super::MirInstructionKind::ListOp { list, argument, .. } => {
            list == needle || argument.as_ref() == Some(needle)
        }
        super::MirInstructionKind::VariantPredicate { variant, .. } => variant == needle,
        super::MirInstructionKind::ConstructSet { elements, .. } => {
            elements.iter().any(|v| v == needle)
        }
        super::MirInstructionKind::SetOp { set, argument, .. } => {
            set == needle || argument.as_ref() == Some(needle)
        }
        super::MirInstructionKind::ConstructVariant { fields, .. }
        | super::MirInstructionKind::ConstructVariantMove { fields, .. } => {
            fields.iter().any(|(_, v)| v == needle)
        }
        super::MirInstructionKind::UpdateRecord { base, fields, .. } => {
            base == needle || fields.iter().any(|v| v == needle)
        }
        super::MirInstructionKind::Binary { left, right, .. } => left == needle || right == needle,
        super::MirInstructionKind::Unary { operand, .. } => operand == needle,
        super::MirInstructionKind::Call { arguments, .. }
        | super::MirInstructionKind::FlowTransition { arguments, .. }
        | super::MirInstructionKind::BuiltinCall { arguments, .. } => {
            arguments.iter().any(|v| v == needle)
        }
    }
}

fn terminator_uses_value(terminator: &super::MirTerminator, needle: &MirValueId) -> bool {
    match terminator {
        super::MirTerminator::Goto { arguments, .. } => arguments.iter().any(|v| v == needle),
        super::MirTerminator::Branch {
            condition,
            then_arguments,
            else_arguments,
            ..
        } => {
            condition == needle
                || then_arguments.iter().any(|v| v == needle)
                || else_arguments.iter().any(|v| v == needle)
        }
        super::MirTerminator::Switch { scrutinee, arms }
        | super::MirTerminator::SwitchMove { scrutinee, arms } => {
            scrutinee == needle
                || arms
                    .iter()
                    .any(|arm| arm.arguments.iter().any(|v| v == needle))
        }
        super::MirTerminator::Return { value } | super::MirTerminator::Fault { value } => {
            value.as_ref() == Some(needle)
        }
        super::MirTerminator::Trap { .. } | super::MirTerminator::Unreachable => false,
    }
}

fn produced_value(kind: &super::MirInstructionKind) -> Option<&MirValueId> {
    match kind {
        super::MirInstructionKind::Const { result, .. }
        | super::MirInstructionKind::Load { result, .. }
        | super::MirInstructionKind::Copy { result, .. }
        | super::MirInstructionKind::Move { result, .. }
        | super::MirInstructionKind::Clone { result, .. }
        | super::MirInstructionKind::Borrow { result, .. }
        | super::MirInstructionKind::Project { result, .. }
        | super::MirInstructionKind::MoveProject { result, .. }
        | super::MirInstructionKind::MoveProjectDrop { result, .. }
        | super::MirInstructionKind::VariantProject { result, .. }
        | super::MirInstructionKind::VariantProjectMove { result, .. }
        | super::MirInstructionKind::Construct { result, .. }
        | super::MirInstructionKind::ConstructList { result, .. }
        | super::MirInstructionKind::ListOp { result, .. }
        | super::MirInstructionKind::VariantPredicate { result, .. }
        | super::MirInstructionKind::ConstructSet { result, .. }
        | super::MirInstructionKind::SetOp { result, .. }
        | super::MirInstructionKind::ConstructVariant { result, .. }
        | super::MirInstructionKind::ConstructVariantMove { result, .. }
        | super::MirInstructionKind::UpdateRecord { result, .. }
        | super::MirInstructionKind::Binary { result, .. }
        | super::MirInstructionKind::Unary { result, .. }
        | super::MirInstructionKind::BuiltinCall { result, .. }
        | super::MirInstructionKind::Convert { result, .. } => Some(result),
        super::MirInstructionKind::Call { result, .. } => result.as_ref(),
        super::MirInstructionKind::FlowTransition { result, .. } => Some(result),
        super::MirInstructionKind::EndBorrow { .. }
        | super::MirInstructionKind::Drop { .. }
        | super::MirInstructionKind::Nop => None,
    }
}

fn propagate_edge(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    before_edge: &BTreeSet<MirValueId>,
    target: &super::MirBlockId,
    values: &[MirValueId],
    subject: String,
    incoming: &mut BTreeMap<super::MirBlockId, BTreeSet<MirValueId>>,
    worklist: &mut VecDeque<super::MirBlockId>,
    queued: &mut BTreeSet<super::MirBlockId>,
    errors: &mut Vec<super::MirValidationError>,
    seen_errors: &mut BTreeSet<(String, String)>,
) {
    let mut consumed = before_edge.clone();
    consume_values(
        function,
        type_catalog,
        &mut consumed,
        values,
        subject,
        errors,
        seen_errors,
    );
    if let Some(block) = function.blocks.get(target) {
        for parameter in &block.parameters {
            consumed.remove(&parameter.value);
        }
    }
    let Some(state) = incoming.get_mut(target) else {
        return;
    };
    let changed = consumed.iter().any(|value| state.insert(value.clone()));
    if changed && queued.insert(target.clone()) {
        worklist.push_back(target.clone());
    }
}

fn consume_values(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    consumed: &mut BTreeSet<MirValueId>,
    values: &[MirValueId],
    subject: String,
    errors: &mut Vec<super::MirValidationError>,
    seen_errors: &mut BTreeSet<(String, String)>,
) {
    for value in values {
        if !is_non_copy(function, type_catalog, value) {
            continue;
        }
        if !consumed.insert((*value).clone()) {
            push_ownership_error(
                errors,
                seen_errors,
                subject.clone(),
                format!("use after consuming non-Copy value '{}'", value),
            );
        }
    }
}

fn push_ownership_error(
    errors: &mut Vec<super::MirValidationError>,
    seen_errors: &mut BTreeSet<(String, String)>,
    subject: String,
    message: String,
) {
    if seen_errors.insert((subject.clone(), message.clone())) {
        errors.push(super::MirValidationError { subject, message });
    }
}

fn is_non_copy(function: &MirFunction, type_catalog: &MirTypeCatalog, value: &MirValueId) -> bool {
    function
        .values
        .get(value)
        .and_then(|value| type_catalog.get(&value.ty))
        .is_some_and(|descriptor| descriptor.ownership != super::types::MirOwnership::Copy)
}

pub struct MirReferenceInterpreter<'a> {
    program: &'a MirProgram,
    max_steps: usize,
    output: RefCell<String>,
}

impl<'a> MirReferenceInterpreter<'a> {
    pub fn new(program: &'a MirProgram) -> Self {
        Self {
            program,
            max_steps: 1_000_000,
            output: RefCell::new(String::new()),
        }
    }

    pub fn with_step_limit(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn execute(
        &self,
        owner: &NodeId,
        arguments: &[MirRuntimeValue],
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        self.execute_with_output(owner, arguments)
            .map(|observation| observation.value)
    }

    pub fn execute_with_output(
        &self,
        owner: &NodeId,
        arguments: &[MirRuntimeValue],
    ) -> Result<MirExecutionObservation, MirExecutionError> {
        self.output.borrow_mut().clear();
        let function = self
            .program
            .functions
            .get(owner)
            .ok_or_else(|| self.error(owner, "function is absent from MIR program"))?;
        let mut steps = 0;
        let value = self.execute_function(function, arguments, &mut steps)?;
        Ok(MirExecutionObservation {
            value,
            output: self.output.borrow().clone(),
        })
    }

    fn execute_function(
        &self,
        function: &MirFunction,
        arguments: &[MirRuntimeValue],
        steps: &mut usize,
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        if function.parameters.len() != arguments.len() {
            return Err(self.error(
                &function.owner,
                format!(
                    "expected {} arguments, received {}",
                    function.parameters.len(),
                    arguments.len()
                ),
            ));
        }
        let mut values = HashMap::new();
        for (parameter, argument) in function.parameters.iter().zip(arguments.iter()) {
            values.insert(parameter.clone(), argument.clone());
        }
        let mut current = function.entry.clone();
        let mut incoming = Vec::new();
        loop {
            *steps = steps.saturating_add(1);
            if *steps > self.max_steps {
                return Err(self.error(&function.owner, "reference execution step limit exceeded"));
            }
            let block = function.blocks.get(&current).ok_or_else(|| {
                self.error(&function.owner, format!("block '{}' is absent", current))
            })?;
            if block.parameters.len() != incoming.len() {
                return Err(self.error(
                    &function.owner,
                    format!(
                        "block '{}' expected {} incoming values, received {}",
                        current,
                        block.parameters.len(),
                        incoming.len()
                    ),
                ));
            }
            for (parameter, value) in block.parameters.iter().zip(incoming.drain(..)) {
                values.insert(parameter.value.clone(), value);
            }
            for instruction in &block.instructions {
                *steps = steps.saturating_add(1);
                if *steps > self.max_steps {
                    return Err(
                        self.error(&function.owner, "reference execution step limit exceeded")
                    );
                }
                self.execute_instruction(function, instruction, &mut values, steps)?;
            }
            match &block.terminator {
                MirTerminator::Goto {
                    target, arguments, ..
                } => {
                    incoming = self.take_transfer_values(function, &mut values, arguments)?;
                    current = target.clone();
                }
                MirTerminator::Branch {
                    condition,
                    then_target,
                    then_arguments,
                    else_target,
                    else_arguments,
                    ..
                } => {
                    let condition = self.read_value(function, &values, condition)?;
                    let (target, arguments) = match condition {
                        MirRuntimeValue::Bool(true) => (then_target, then_arguments),
                        MirRuntimeValue::Bool(false) => (else_target, else_arguments),
                        _ => {
                            return Err(self.error(&function.owner, "branch condition is not bool"))
                        }
                    };
                    incoming = self.take_transfer_values(function, &mut values, arguments)?;
                    current = target.clone();
                }
                MirTerminator::Switch { scrutinee, arms } => {
                    let scrutinee_id = scrutinee.clone();
                    let scrutinee = self.read_value(function, &values, scrutinee)?;
                    let arm = self.select_switch_arm(function, &scrutinee, arms)?;
                    incoming = self.switch_arguments(
                        function,
                        &mut values,
                        &scrutinee_id,
                        &scrutinee,
                        arm,
                    )?;
                    current = arm.target.clone();
                }
                MirTerminator::SwitchMove { scrutinee, arms } => {
                    let scrutinee_id = scrutinee.clone();
                    let scrutinee = self.read_value(function, &values, scrutinee)?;
                    let arm = self.select_switch_arm(function, &scrutinee, arms)?;
                    incoming =
                        self.switch_move_arguments(function, &mut values, &scrutinee_id, arm)?;
                    current = arm.target.clone();
                }
                MirTerminator::Return { value } => {
                    return value
                        .as_ref()
                        .map(|value| self.take_transfer_value(function, &mut values, value))
                        .unwrap_or(Ok(MirRuntimeValue::Unit));
                }
                MirTerminator::Trap { code } => {
                    return Err(self.error(&function.owner, format!("trap {code}")));
                }
                MirTerminator::Fault { .. } => {
                    return Err(self.error(&function.owner, "fault terminator reached"));
                }
                MirTerminator::Unreachable => {
                    return Err(self.error(&function.owner, "unreachable terminator reached"));
                }
            }
        }
    }

    fn execute_instruction(
        &self,
        function: &MirFunction,
        instruction: &MirInstruction,
        values: &mut HashMap<MirValueId, MirRuntimeValue>,
        steps: &mut usize,
    ) -> Result<(), MirExecutionError> {
        match &instruction.kind {
            MirInstructionKind::Const { result, literal } => {
                values.insert(result.clone(), runtime_literal(literal));
            }
            MirInstructionKind::Load { result, place } => {
                let source = self.load_place(function, values, place)?;
                values.insert(result.clone(), source);
            }
            MirInstructionKind::Copy { result, source }
            | MirInstructionKind::Clone { result, source } => {
                let value = self.read_value(function, values, source)?;
                values.insert(result.clone(), value);
            }
            MirInstructionKind::Convert { result, source } => {
                let source_ty = function
                    .values
                    .get(source)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(
                            &function.owner,
                            format!("conversion source '{}' is absent from MIR values", source),
                        )
                    })?;
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(
                            &function.owner,
                            format!("conversion result '{}' is absent from MIR values", result),
                        )
                    })?;
                let contract = self
                    .program
                    .type_catalog()
                    .validate_conversion(&source_ty, &result_ty)
                    .map_err(|message| self.error(&function.owner, message))?;
                let value = self.read_value(function, values, source)?;
                let value = match contract.kind {
                    super::types::MirConversionKind::ScalarIdentity => value,
                    super::types::MirConversionKind::SignedI32ToI64 => match value {
                        MirRuntimeValue::Int(value) => MirRuntimeValue::Int(value),
                        _ => {
                            return Err(self.error(
                                &function.owner,
                                format!(
                                    "conversion '{}' received an incompatible runtime value",
                                    contract.name
                                ),
                            ))
                        }
                    },
                };
                values.insert(result.clone(), value);
            }
            MirInstructionKind::Move { result, source } => {
                let is_copy = function
                    .values
                    .get(source)
                    .and_then(|value| self.program.type_catalog().get(&value.ty))
                    .is_some_and(|descriptor| {
                        descriptor.ownership == super::types::MirOwnership::Copy
                    });
                let value = if is_copy {
                    self.read_value(function, values, source)?
                } else {
                    values.remove(source).ok_or_else(|| {
                        self.error(
                            &function.owner,
                            format!("move source '{}' is unavailable", source),
                        )
                    })?
                };
                values.insert(result.clone(), value);
            }
            MirInstructionKind::Drop { value } => {
                let ty = function
                    .values
                    .get(value)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, format!("drop value '{}' is absent", value))
                    })?;
                let is_copy = self
                    .program
                    .type_catalog()
                    .get(&ty)
                    .is_some_and(|descriptor| {
                        descriptor.ownership == super::types::MirOwnership::Copy
                    });
                if !is_copy {
                    let runtime_value = values.remove(value).ok_or_else(|| {
                        self.error(
                            &function.owner,
                            format!("drop value '{}' is unavailable", value),
                        )
                    })?;
                    self.drop_runtime_value(function, &ty, runtime_value)?;
                }
            }
            MirInstructionKind::Borrow { result, source, .. } => {
                let value = self.read_value(function, values, source)?;
                values.insert(result.clone(), value);
            }
            MirInstructionKind::EndBorrow { borrow } => {
                values.remove(borrow);
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
                list_index_contract,
            } => {
                let value = self.read_value(function, values, base)?;
                let base_ty = function.values.get(base).map(|value| &value.ty);
                let result_ty = function.values.get(result).map(|value| &value.ty);
                let index_value = match projection {
                    MirProjection::Index(index) => Some(self.read_value(function, values, index)?),
                    _ => None,
                };
                let index_ty = match projection {
                    MirProjection::Index(index) => {
                        function.values.get(index).map(|value| &value.ty)
                    }
                    _ => None,
                };
                let projected = project_value(
                    &function.owner,
                    value,
                    base_ty,
                    result_ty,
                    projection,
                    index_value.as_ref(),
                    index_ty,
                    list_index_contract.as_ref(),
                    self.program.type_catalog(),
                )?;
                values.insert(result.clone(), projected);
            }
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => {
                let base_ty = function
                    .values
                    .get(base)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "move projection base has no type")
                    })?;
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "move projection result has no type")
                    })?;
                let base_value = self.take_transfer_value(function, values, base)?;
                let projected = move_project_value(
                    &function.owner,
                    base_value,
                    &base_ty,
                    &result_ty,
                    projection,
                    self.program.type_catalog(),
                )?;
                values.insert(result.clone(), projected);
            }
            MirInstructionKind::MoveProjectDrop {
                result,
                base,
                contract,
                ..
            } => {
                let base_ty = function
                    .values
                    .get(base)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(
                            &function.owner,
                            "record move/drop projection base has no type",
                        )
                    })?;
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(
                            &function.owner,
                            "record move/drop projection result has no type",
                        )
                    })?;
                let receipt = contract.as_ref().ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "record move/drop projection has no canonical residual receipt",
                    )
                })?;
                self.program
                    .type_catalog()
                    .validate_record_move_projection_drop_receipt(&base_ty, &result_ty, receipt)
                    .map_err(|message| self.error(&function.owner, message))?;
                let value = self.take_transfer_value(function, values, base)?;
                let (projected, residual) = move_project_record_drop_value(
                    &function.owner,
                    value,
                    &base_ty,
                    &result_ty,
                    receipt,
                    self.program.type_catalog(),
                )?;
                for (residual_ty, residual_value) in residual {
                    self.drop_runtime_value(function, &residual_ty, residual_value)?;
                }
                values.insert(result.clone(), projected);
            }
            MirInstructionKind::VariantProject {
                result,
                base,
                contract,
            } => {
                let base_ty = function
                    .values
                    .get(base)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "variant projection base has no type")
                    })?;
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "variant projection result has no type")
                    })?;
                let receipt = contract.as_ref().ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "direct variant projection has no canonical trap receipt",
                    )
                })?;
                self.program
                    .type_catalog()
                    .validate_variant_projection_trap_receipt(&base_ty, &result_ty, receipt)
                    .map_err(|message| self.error(&function.owner, message))?;
                let value = self.read_value(function, values, base)?;
                let projected = project_variant_value(
                    &function.owner,
                    value,
                    &base_ty,
                    &result_ty,
                    receipt,
                    self.program.type_catalog(),
                )?;
                values.insert(result.clone(), projected);
            }
            MirInstructionKind::VariantProjectMove {
                result,
                base,
                contract,
            } => {
                let base_ty = function
                    .values
                    .get(base)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "variant move projection base has no type")
                    })?;
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(
                            &function.owner,
                            "variant move projection result has no type",
                        )
                    })?;
                let receipt = contract.as_ref().ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "consuming direct variant projection has no canonical move receipt",
                    )
                })?;
                self.program
                    .type_catalog()
                    .validate_variant_move_projection_trap_receipt(&base_ty, &result_ty, receipt)
                    .map_err(|message| self.error(&function.owner, message))?;
                let value = self.take_transfer_value(function, values, base)?;
                let projected = move_project_variant_value(
                    &function.owner,
                    value,
                    &base_ty,
                    &result_ty,
                    receipt,
                    self.program.type_catalog(),
                )?;
                values.insert(result.clone(), projected);
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let fields = self.take_transfer_values(function, values, fields)?;
                let value = match kind {
                    MirAggregateKind::Tuple => MirRuntimeValue::Tuple(fields),
                    MirAggregateKind::Record {
                        nominal,
                        fields: field_ids,
                    } => self.construct_record(function, result, nominal, field_ids, fields)?,
                };
                values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructList {
                result,
                elements,
                list_construct_contract,
            } => {
                let element_types = elements
                    .iter()
                    .map(|element| {
                        function
                            .values
                            .get(element)
                            .map(|value| value.ty.clone())
                            .ok_or_else(|| {
                                self.error(&function.owner, "List element has no MIR type")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| self.error(&function.owner, "List result has no MIR type"))?;
                let receipt = list_construct_contract.as_ref().ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "List construction has no canonical receipt",
                    )
                })?;
                self.program
                    .type_catalog()
                    .validate_list_construct_receipt(&result_ty, &element_types, receipt)
                    .map_err(|message| self.error(&function.owner, message))?;
                let elements = self.take_transfer_values(function, values, elements)?;
                values.insert(result.clone(), MirRuntimeValue::List(elements));
            }
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "List operation result has no MIR type")
                    })?;
                let list_ty = function
                    .values
                    .get(list)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "List operation receiver has no MIR type")
                    })?;
                let receipt = list_operation_contract.as_ref().ok_or_else(|| {
                    self.error(&function.owner, "List operation has no canonical receipt")
                })?;
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                self.program
                    .type_catalog()
                    .validate_list_operation_receipt_with_argument(
                        &result_ty,
                        &list_ty,
                        argument_ty.as_ref(),
                        *operation,
                        receipt,
                    )
                    .map_err(|message| self.error(&function.owner, message))?;
                let output = match operation {
                    super::MirListOperation::Len => {
                        let MirRuntimeValue::List(elements) =
                            self.read_value(function, values, list)?
                        else {
                            return Err(self.error(
                                &function.owner,
                                "List operation receiver is not a canonical List",
                            ));
                        };
                        if elements.len() > i32::MAX as usize {
                            return Err(self.error(
                                &function.owner,
                                "E0802: canonical List.len result overflows i32",
                            ));
                        }
                        MirRuntimeValue::Int(elements.len() as i64)
                    }
                    super::MirListOperation::Reverse => {
                        let MirRuntimeValue::List(elements) =
                            self.read_value(function, values, list)?
                        else {
                            return Err(self.error(
                                &function.owner,
                                "List operation receiver is not a canonical List",
                            ));
                        };
                        // Reverse is a cloning transform: the source List
                        // remains available for its own Drop and the result
                        // owns an independent element vector.
                        MirRuntimeValue::List(elements.into_iter().rev().collect())
                    }
                    super::MirListOperation::Concat => {
                        let Some(argument) = argument else {
                            return Err(self.error(
                                &function.owner,
                                "List.concat operation has no second input",
                            ));
                        };
                        let left = self.take_transfer_value(function, values, list)?;
                        let right = self.take_transfer_value(function, values, argument)?;
                        let MirRuntimeValue::List(left) = left else {
                            return Err(self.error(
                                &function.owner,
                                "List.concat receiver is not a canonical List",
                            ));
                        };
                        let MirRuntimeValue::List(right) = right else {
                            return Err(self.error(
                                &function.owner,
                                "List.concat argument is not a canonical List",
                            ));
                        };
                        MirRuntimeValue::List(left.into_iter().chain(right).collect())
                    }
                };
                values.insert(result.clone(), output);
            }
            MirInstructionKind::VariantPredicate {
                result,
                predicate,
                variant,
                contract,
            } => {
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "variant predicate result has no MIR type")
                    })?;
                let variant_ty = function
                    .values
                    .get(variant)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "variant predicate source has no MIR type")
                    })?;
                let receipt = contract.as_ref().ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "variant predicate has no canonical receipt",
                    )
                })?;
                self.program
                    .type_catalog()
                    .validate_variant_predicate_receipt(
                        &result_ty,
                        &variant_ty,
                        *predicate,
                        receipt,
                    )
                    .map_err(|message| self.error(&function.owner, message))?;
                let MirRuntimeValue::Variant {
                    nominal,
                    variant: actual_variant,
                    ..
                } = self.read_value(function, values, variant)?
                else {
                    return Err(self.error(
                        &function.owner,
                        "variant predicate source is not a canonical Variant",
                    ));
                };
                if nominal != receipt.nominal {
                    return Err(self.error(
                        &function.owner,
                        "variant predicate runtime nominal disagrees with TypeDesc",
                    ));
                }
                let is_target = if actual_variant == receipt.variant {
                    true
                } else if actual_variant == receipt.alternate_variant {
                    false
                } else {
                    return Err(self.error(
                        &function.owner,
                        "variant predicate runtime variant disagrees with TypeDesc",
                    ));
                };
                values.insert(result.clone(), MirRuntimeValue::Bool(is_target));
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                let element_types = elements
                    .iter()
                    .map(|element| {
                        function
                            .values
                            .get(element)
                            .map(|value| value.ty.clone())
                            .ok_or_else(|| {
                                self.error(&function.owner, "Set element has no MIR type")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| self.error(&function.owner, "Set result has no MIR type"))?;
                self.program
                    .type_catalog()
                    .validate_set_construct(&result_ty, &element_types)
                    .map_err(|message| self.error(&function.owner, message))?;
                let elements = self.take_transfer_values(function, values, elements)?;
                // A Set literal is semantically the same insertion sequence
                // used by the production backends: duplicate members do not
                // survive construction.  Keeping the oracle as a raw Vec
                // must not leak that representation detail into L1 results.
                let mut unique = Vec::with_capacity(elements.len());
                for element in elements {
                    if !unique.iter().any(|existing| existing == &element) {
                        unique.push(element);
                    }
                }
                values.insert(result.clone(), MirRuntimeValue::Set(unique));
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                let result_ty = function
                    .values
                    .get(result)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "Set operation result has no MIR type")
                    })?;
                let set_ty = function
                    .values
                    .get(set)
                    .map(|value| value.ty.clone())
                    .ok_or_else(|| {
                        self.error(&function.owner, "Set operation receiver has no MIR type")
                    })?;
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| &value.ty);
                self.program
                    .type_catalog()
                    .validate_set_operation(&result_ty, &set_ty, argument_ty, *operation)
                    .map_err(|message| self.error(&function.owner, message))?;
                let output = match operation {
                    super::MirSetOperation::Size => {
                        let MirRuntimeValue::Set(set) = self.read_value(function, values, set)?
                        else {
                            return Err(
                                self.error(&function.owner, "Set.size receiver is not a Set")
                            );
                        };
                        MirRuntimeValue::Int(set.len() as i64)
                    }
                    super::MirSetOperation::IsEmpty => {
                        let MirRuntimeValue::Set(set) = self.read_value(function, values, set)?
                        else {
                            return Err(
                                self.error(&function.owner, "Set.is_empty receiver is not a Set")
                            );
                        };
                        MirRuntimeValue::Bool(set.is_empty())
                    }
                    super::MirSetOperation::Contains => {
                        let MirRuntimeValue::Set(set) = self.read_value(function, values, set)?
                        else {
                            return Err(
                                self.error(&function.owner, "Set.contains receiver is not a Set")
                            );
                        };
                        let argument = argument.as_ref().ok_or_else(|| {
                            self.error(&function.owner, "Set.contains argument is absent")
                        })?;
                        let argument = self.read_value(function, values, argument)?;
                        MirRuntimeValue::Bool(set.iter().any(|value| value == &argument))
                    }
                    super::MirSetOperation::Insert | super::MirSetOperation::Remove => {
                        let mut set = match self.take_transfer_value(function, values, set)? {
                            MirRuntimeValue::Set(set) => set,
                            _ => {
                                return Err(self.error(
                                    &function.owner,
                                    "Set transformation receiver is not a Set",
                                ))
                            }
                        };
                        let argument = argument.as_ref().ok_or_else(|| {
                            self.error(&function.owner, "Set transformation argument is absent")
                        })?;
                        let argument = self.read_value(function, values, argument)?;
                        if *operation == super::MirSetOperation::Insert {
                            if !set.iter().any(|value| value == &argument) {
                                set.push(argument);
                            }
                        } else {
                            set.retain(|value| value != &argument);
                        }
                        MirRuntimeValue::Set(set)
                    }
                    super::MirSetOperation::ToList => {
                        let mut values = match self.read_value(function, values, set)? {
                            MirRuntimeValue::Set(values) => values,
                            _ => {
                                return Err(self
                                    .error(&function.owner, "Set.to_list receiver is not a Set"))
                            }
                        };
                        // Set storage is semantically unordered. The
                        // canonical scalar Set contract exposes a sorted
                        // List view so the reference, VM, and native
                        // HashSet-backed implementation have one observable
                        // result independent of insertion/iteration order.
                        values.sort_by(|left, right| match (left, right) {
                            (MirRuntimeValue::Int(left), MirRuntimeValue::Int(right)) => {
                                left.cmp(right)
                            }
                            (MirRuntimeValue::Bool(left), MirRuntimeValue::Bool(right)) => {
                                left.cmp(right)
                            }
                            _ => std::cmp::Ordering::Equal,
                        });
                        MirRuntimeValue::List(values)
                    }
                };
                values.insert(result.clone(), output);
            }
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_values = fields
                    .iter()
                    .map(|(_, value)| self.take_transfer_value(function, values, value))
                    .collect::<Result<Vec<_>, _>>()?;
                let value = self.construct_variant(
                    function,
                    result,
                    nominal,
                    variant,
                    &field_ids,
                    field_values,
                )?;
                values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => {
                let field_ids = fields
                    .iter()
                    .map(|(field, _)| field.clone())
                    .collect::<Vec<_>>();
                let field_values = fields
                    .iter()
                    .map(|(_, value)| self.take_transfer_value(function, values, value))
                    .collect::<Result<Vec<_>, _>>()?;
                let value = self.construct_variant(
                    function,
                    result,
                    nominal,
                    variant,
                    &field_ids,
                    field_values,
                )?;
                values.insert(result.clone(), value);
            }
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind: MirAggregateKind::Record { nominal, fields },
                fields: update_values,
            } => {
                let base_value = self.take_transfer_value(function, values, base)?;
                let update_values = self.take_transfer_values(function, values, update_values)?;
                let value = self.update_record(
                    function,
                    result,
                    base_value,
                    nominal,
                    fields,
                    update_values,
                )?;
                values.insert(result.clone(), value);
            }
            MirInstructionKind::UpdateRecord { .. } => {
                return Err(self.error(
                    &function.owner,
                    "record update instruction has a non-record aggregate kind",
                ));
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                let integer_width = self.integer_width(function, left);
                let left = self.read_value(function, values, left)?;
                let right = self.read_value(function, values, right)?;
                let output = evaluate_binary(&function.owner, *op, left, right, integer_width)?;
                values.insert(result.clone(), output);
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                let integer_width = self.integer_width(function, operand);
                let operand = self.read_value(function, values, operand)?;
                let output = evaluate_unary(&function.owner, *op, operand, integer_width)?;
                values.insert(result.clone(), output);
            }
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
            } => {
                let contract = super::types::MirBuiltinContract::for_kind(*kind);
                if arguments.len() != contract.arity {
                    return Err(self.error(
                        &function.owner,
                        format!(
                            "builtin '{}' received {} arguments; contract requires {}",
                            contract.name,
                            arguments.len(),
                            contract.arity
                        ),
                    ));
                }
                let output = match *kind {
                    super::types::MirBuiltinKind::Abs => {
                        let argument = arguments.first().ok_or_else(|| {
                            self.error(&function.owner, "builtin argument is absent")
                        })?;
                        let argument = self.read_value(function, values, argument)?;
                        match argument {
                            MirRuntimeValue::Int(value) => value
                                .checked_abs()
                                .map(MirRuntimeValue::Int)
                                .ok_or_else(|| {
                                    self.error(
                                        &function.owner,
                                        "E0802: integer absolute value overflow",
                                    )
                                })?,
                            MirRuntimeValue::FloatBits(bits) => {
                                MirRuntimeValue::FloatBits(f64::from_bits(bits).abs().to_bits())
                            }
                            _ => {
                                return Err(self.error(
                                    &function.owner,
                                    format!(
                                        "builtin '{}' received an incompatible runtime value",
                                        contract.name
                                    ),
                                ))
                            }
                        }
                    }
                    super::types::MirBuiltinKind::Min | super::types::MirBuiltinKind::Max => {
                        let left = arguments.first().ok_or_else(|| {
                            self.error(&function.owner, "builtin left argument is absent")
                        })?;
                        let right = arguments.get(1).ok_or_else(|| {
                            self.error(&function.owner, "builtin right argument is absent")
                        })?;
                        let left = self.read_value(function, values, left)?;
                        let right = self.read_value(function, values, right)?;
                        match (left, right) {
                            (MirRuntimeValue::Int(left), MirRuntimeValue::Int(right)) => {
                                let value = if *kind == super::types::MirBuiltinKind::Min {
                                    left.min(right)
                                } else {
                                    left.max(right)
                                };
                                MirRuntimeValue::Int(value)
                            }
                            _ => {
                                return Err(self.error(
                                    &function.owner,
                                    format!(
                                        "builtin '{}' received an incompatible runtime value",
                                        contract.name
                                    ),
                                ))
                            }
                        }
                    }
                    super::types::MirBuiltinKind::PrintlnBool => {
                        let argument = arguments.first().ok_or_else(|| {
                            self.error(&function.owner, "println argument is absent")
                        })?;
                        let argument = self.read_value(function, values, argument)?;
                        let MirRuntimeValue::Bool(value) = argument else {
                            return Err(self.error(
                                &function.owner,
                                "builtin 'println' received a non-bool value",
                            ));
                        };
                        let mut output = self.output.borrow_mut();
                        output.push_str(if value { "true\n" } else { "false\n" });
                        MirRuntimeValue::Unit
                    }
                    super::types::MirBuiltinKind::PrintlnInt => {
                        let argument = arguments.first().ok_or_else(|| {
                            self.error(&function.owner, "println argument is absent")
                        })?;
                        let argument = self.read_value(function, values, argument)?;
                        let MirRuntimeValue::Int(value) = argument else {
                            return Err(self.error(
                                &function.owner,
                                "builtin 'println' received a non-integer value",
                            ));
                        };
                        self.output.borrow_mut().push_str(&format!("{value}\n"));
                        MirRuntimeValue::Unit
                    }
                };
                values.insert(result.clone(), output);
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
                variant_call_contract,
            } => {
                let ResolvedCallee::Function(owner) = callee else {
                    return Err(self.error(
                        &function.owner,
                        format!("callee '{callee:?}' is not a MIR function"),
                    ));
                };
                let callee = self.program.functions.get(owner).ok_or_else(|| {
                    self.error(&function.owner, format!("callee '{}' is absent", owner.0))
                })?;
                let parameter_types = callee
                    .parameters
                    .iter()
                    .map(|parameter| callee.values.get(parameter).map(|value| value.ty.clone()))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        self.error(
                            &function.owner,
                            format!("callee '{}' has an incomplete MIR signature", owner.0),
                        )
                    })?;
                let flat_variant_result = self
                    .program
                    .type_catalog()
                    .validate_flat_copy_variant(&callee.result)
                    .is_ok();
                let move_owned_result = self
                    .program
                    .type_catalog()
                    .validate_result_string_i32_variant(&callee.result)
                    .is_ok();
                if flat_variant_result || move_owned_result {
                    let receipt = variant_call_contract.as_ref().ok_or_else(|| {
                        self.error(
                            &function.owner,
                            if flat_variant_result {
                                "call returning flat Copy Option/Result has no canonical ABI receipt"
                            } else {
                                "call returning move-owned Result<string, i32> has no canonical ABI receipt"
                            },
                        )
                    })?;
                    self.program
                        .type_catalog()
                        .validate_variant_call_abi_receipt(
                            owner,
                            type_arguments,
                            &parameter_types,
                            &callee.result,
                            receipt,
                        )
                        .map_err(|message| self.error(&function.owner, message))?;
                    if move_owned_result {
                        super::validate_move_owned_result_return_merge(
                            callee,
                            self.program.type_catalog(),
                        )
                        .map_err(|message| self.error(&function.owner, message))?;
                    }
                } else if variant_call_contract.is_some() {
                    return Err(self.error(
                        &function.owner,
                        "variant call ABI receipt is attached to an unsupported variant result",
                    ));
                } else if self.program.type_catalog().get(&callee.result).is_some_and(
                    |descriptor| {
                        descriptor.kind == super::types::MirTypeKind::Result
                            && descriptor.ownership != super::types::MirOwnership::Copy
                    },
                ) {
                    return Err(self.error(
                        &function.owner,
                        "non-Copy Result call result is outside the canonical call ABI contract",
                    ));
                }
                let arguments = self.take_transfer_values(function, values, arguments)?;
                let output = self.execute_function(callee, &arguments, steps)?;
                if let Some(result) = result {
                    values.insert(result.clone(), output);
                }
            }
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
            } => {
                let arguments = self.take_transfer_values(function, values, arguments)?;
                let contract = self.program.transitions.get(transition).ok_or_else(|| {
                    self.error(
                        &function.owner,
                        format!("transition '{}' has no MIR contract", transition.0),
                    )
                })?;
                if contract.effect != MirTransitionEffect::SilentLocal
                    || contract.targets.len() != 1
                    || contract.failure.is_some()
                    || contract.is_fallback
                    || contract.is_ffi_pinned
                {
                    return Err(self.error(
                        &function.owner,
                        "FlowTransition is outside the silent-local transition island",
                    ));
                }
                let callee = self.program.functions.get(&contract.owner).ok_or_else(|| {
                    self.error(
                        &function.owner,
                        format!("transition '{}' executable body is absent", transition.0),
                    )
                })?;
                let output = self.execute_function(callee, &arguments, steps)?;
                values.insert(result.clone(), output);
            }
            MirInstructionKind::Nop => {}
        }
        Ok(())
    }

    fn load_place(
        &self,
        function: &MirFunction,
        values: &HashMap<MirValueId, MirRuntimeValue>,
        place: &ResolvedPlace,
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        let local = MirValueId::new(format!("local:{}", place.base.0 .0))
            .map_err(|error| self.error(&function.owner, error.to_string()))?;
        let mut value = self.read_value(function, values, &local)?;
        let mut current_ty = function
            .values
            .get(&local)
            .map(|value| value.ty.clone())
            .ok_or_else(|| self.error(&function.owner, "place base has no MIR type"))?;
        for projection in &place.projections {
            let mir_projection = match projection {
                crate::core::ir::ResolvedProjection::Tuple { index, .. } => {
                    MirProjection::Tuple(*index)
                }
                crate::core::ir::ResolvedProjection::Field { field, .. } => {
                    MirProjection::Field(field.clone())
                }
                crate::core::ir::ResolvedProjection::Index { .. } => {
                    return Err(self.error(
                        &function.owner,
                        "indexed place projection has no canonical MIR layout contract",
                    ));
                }
                crate::core::ir::ResolvedProjection::Deref { .. } => {
                    return Err(self.error(
                        &function.owner,
                        "dereference place projection has no canonical MIR layout contract",
                    ));
                }
            };
            value = project_value(
                &function.owner,
                value,
                Some(&current_ty),
                Some(projection.ty()),
                &mir_projection,
                None,
                None,
                None,
                self.program.type_catalog(),
            )?;
            current_ty = projection.ty().clone();
        }
        Ok(value)
    }

    fn construct_record(
        &self,
        function: &MirFunction,
        result: &MirValueId,
        nominal: &crate::core::ir::NominalTypeId,
        field_ids: &[NodeId],
        values: Vec<MirRuntimeValue>,
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        let result_ty = function
            .values
            .get(result)
            .map(|value| &value.ty)
            .ok_or_else(|| self.error(&function.owner, "record result has no MIR type"))?;
        let descriptor = self
            .program
            .type_catalog()
            .get(result_ty)
            .ok_or_else(|| self.error(&function.owner, "record result has no TypeDesc"))?;
        let MirLayout::Record {
            nominal: expected_nominal,
            fields: layout_fields,
        } = &descriptor.layout
        else {
            return Err(self.error(&function.owner, "record result has no record layout"));
        };
        if nominal != expected_nominal {
            return Err(self.error(
                &function.owner,
                "record construction nominal disagrees with TypeDesc",
            ));
        }
        if field_ids.len() != values.len() {
            return Err(self.error(
                &function.owner,
                "record construction field/value arity disagrees",
            ));
        }
        let mut supplied = HashMap::new();
        for (field, value) in field_ids.iter().zip(values) {
            if supplied.insert(field.clone(), value).is_some() {
                return Err(self.error(&function.owner, "record construction repeats a field"));
            }
        }
        layout_fields
            .iter()
            .map(|field| {
                supplied.remove(&field.id).ok_or_else(|| {
                    self.error(
                        &function.owner,
                        format!("record construction omits field '{}'", field.id.0),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .and_then(|fields| {
                if supplied.is_empty() {
                    Ok(MirRuntimeValue::Record {
                        nominal: expected_nominal.clone(),
                        fields,
                    })
                } else {
                    Err(self.error(
                        &function.owner,
                        "record construction contains an unknown field",
                    ))
                }
            })
    }

    fn update_record(
        &self,
        function: &MirFunction,
        result: &MirValueId,
        base: MirRuntimeValue,
        nominal: &crate::core::ir::NominalTypeId,
        field_ids: &[NodeId],
        update_values: Vec<MirRuntimeValue>,
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        let MirRuntimeValue::Record {
            nominal: base_nominal,
            mut fields,
        } = base
        else {
            return Err(self.error(&function.owner, "record update base is not a record"));
        };
        let result_ty = function
            .values
            .get(result)
            .map(|value| &value.ty)
            .ok_or_else(|| self.error(&function.owner, "record update result has no MIR type"))?;
        let descriptor = self
            .program
            .type_catalog()
            .get(result_ty)
            .ok_or_else(|| self.error(&function.owner, "record update has no TypeDesc"))?;
        let MirLayout::Record {
            nominal: expected_nominal,
            fields: layout_fields,
        } = &descriptor.layout
        else {
            return Err(self.error(&function.owner, "record update result has no record layout"));
        };
        if nominal != expected_nominal || &base_nominal != expected_nominal {
            return Err(self.error(
                &function.owner,
                "record update nominal disagrees with TypeDesc",
            ));
        }
        if fields.len() != layout_fields.len() {
            return Err(self.error(&function.owner, "record base is shorter than TypeDesc"));
        }
        if field_ids.len() != update_values.len() || field_ids.len() > u16::MAX as usize {
            return Err(self.error(&function.owner, "record update field/value arity disagrees"));
        }
        for (field, value) in field_ids.iter().zip(update_values) {
            let Some(index) = layout_fields
                .iter()
                .position(|candidate| candidate.id == *field)
            else {
                return Err(self.error(
                    &function.owner,
                    format!("record update field '{}' is absent", field.0),
                ));
            };
            if index >= fields.len() {
                return Err(self.error(&function.owner, "record base is shorter than TypeDesc"));
            }
            fields[index] = value;
        }
        Ok(MirRuntimeValue::Record {
            nominal: expected_nominal.clone(),
            fields,
        })
    }

    fn construct_variant(
        &self,
        function: &MirFunction,
        result: &MirValueId,
        nominal: &crate::core::NominalTypeId,
        variant: &NodeId,
        field_ids: &[NodeId],
        values: Vec<MirRuntimeValue>,
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        let result_ty = function
            .values
            .get(result)
            .map(|value| &value.ty)
            .ok_or_else(|| self.error(&function.owner, "variant result has no MIR type"))?;
        let Some((expected_nominal, variants)) =
            self.program.type_catalog().variant_layout(result_ty)
        else {
            return Err(self.error(&function.owner, "variant result has no TypeDesc layout"));
        };
        if nominal.as_str() != expected_nominal {
            return Err(self.error(
                &function.owner,
                "variant construction nominal disagrees with TypeDesc",
            ));
        }
        let Some(expected_variant) = variants.iter().find(|candidate| candidate.id == *variant)
        else {
            return Err(self.error(&function.owner, "variant is absent from TypeDesc"));
        };
        let mut supplied = HashMap::new();
        for (field, value) in field_ids.iter().zip(values) {
            if supplied.insert(field.clone(), value).is_some() {
                return Err(self.error(&function.owner, "variant payload field is repeated"));
            }
        }
        let payload = expected_variant
            .fields
            .iter()
            .map(|field| {
                supplied.remove(&field.id).ok_or_else(|| {
                    self.error(
                        &function.owner,
                        format!("variant payload field '{}' is missing", field.id.0),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !supplied.is_empty() {
            return Err(self.error(
                &function.owner,
                "variant construction contains an unknown payload field",
            ));
        }
        Ok(MirRuntimeValue::Variant {
            nominal: crate::core::NominalTypeId::new(expected_nominal)
                .expect("TypeDesc variant nominal is non-empty"),
            variant: expected_variant.id.clone(),
            payload,
        })
    }

    fn read_value(
        &self,
        function: &MirFunction,
        values: &HashMap<MirValueId, MirRuntimeValue>,
        value: &MirValueId,
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        values
            .get(value)
            .cloned()
            .ok_or_else(|| self.error(&function.owner, format!("value '{}' is unavailable", value)))
    }

    fn take_transfer_value(
        &self,
        function: &MirFunction,
        values: &mut HashMap<MirValueId, MirRuntimeValue>,
        id: &MirValueId,
    ) -> Result<MirRuntimeValue, MirExecutionError> {
        let ty = function
            .values
            .get(id)
            .map(|value| value.ty.clone())
            .ok_or_else(|| self.error(&function.owner, format!("value '{}' is absent", id)))?;
        let Some(descriptor) = self.program.type_catalog().get(&ty) else {
            // Hand-written structural MIR tests use `MirProgram::new`, which
            // intentionally has no catalog. Production MIR always reaches
            // this path through `with_type_catalog`, where the absence is a
            // validation error before execution.
            return self.read_value(function, values, id);
        };
        if descriptor.ownership == super::types::MirOwnership::Copy {
            self.read_value(function, values, id)
        } else {
            values.remove(id).ok_or_else(|| {
                self.error(
                    &function.owner,
                    format!("transfer source '{}' is unavailable", id),
                )
            })
        }
    }

    fn take_transfer_values(
        &self,
        function: &MirFunction,
        values: &mut HashMap<MirValueId, MirRuntimeValue>,
        ids: &[MirValueId],
    ) -> Result<Vec<MirRuntimeValue>, MirExecutionError> {
        ids.iter()
            .map(|id| self.take_transfer_value(function, values, id))
            .collect()
    }

    fn drop_runtime_value(
        &self,
        function: &MirFunction,
        ty: &crate::core::ResolvedTypeId,
        value: MirRuntimeValue,
    ) -> Result<(), MirExecutionError> {
        self.program
            .type_catalog()
            .validate_glue(ty, MirGlueOperation::Drop)
            .map_err(|message| self.error(&function.owner, message))?;
        let descriptor = self
            .program
            .type_catalog()
            .get(ty)
            .ok_or_else(|| self.error(&function.owner, "drop value has no TypeDesc"))?;
        if descriptor.ownership == super::types::MirOwnership::Copy {
            return Ok(());
        }
        match &descriptor.layout {
            MirLayout::Tuple(elements) => {
                let MirRuntimeValue::Tuple(mut fields) = value else {
                    return Err(self.error(&function.owner, "aggregate drop value is not a tuple"));
                };
                let Some(plan) = &descriptor.drop_plan else {
                    return Err(
                        self.error(&function.owner, "aggregate drop value has no drop plan")
                    );
                };
                if fields.len() != elements.len() || plan.fields.len() != elements.len() {
                    return Err(self.error(
                        &function.owner,
                        "aggregate drop value disagrees with TypeDesc arity",
                    ));
                }
                for field in &plan.fields {
                    let child = std::mem::replace(
                        fields.get_mut(field.index).ok_or_else(|| {
                            self.error(&function.owner, "aggregate drop field is out of bounds")
                        })?,
                        MirRuntimeValue::Unit,
                    );
                    self.drop_runtime_value(function, &field.ty, child)?;
                }
                Ok(())
            }
            MirLayout::Record {
                nominal: expected_nominal,
                fields: layout_fields,
            } => {
                let MirRuntimeValue::Record {
                    nominal,
                    mut fields,
                } = value
                else {
                    return Err(self.error(&function.owner, "aggregate drop value is not a record"));
                };
                if nominal != *expected_nominal {
                    return Err(self.error(
                        &function.owner,
                        "aggregate drop record nominal disagrees with TypeDesc",
                    ));
                }
                let Some(plan) = &descriptor.drop_plan else {
                    return Err(
                        self.error(&function.owner, "aggregate drop value has no drop plan")
                    );
                };
                if fields.len() != layout_fields.len() || plan.fields.len() != layout_fields.len() {
                    return Err(self.error(
                        &function.owner,
                        "aggregate drop record disagrees with TypeDesc arity",
                    ));
                }
                for field in &plan.fields {
                    let child = std::mem::replace(
                        fields.get_mut(field.index).ok_or_else(|| {
                            self.error(&function.owner, "aggregate drop field is out of bounds")
                        })?,
                        MirRuntimeValue::Unit,
                    );
                    self.drop_runtime_value(function, &field.ty, child)?;
                }
                Ok(())
            }
            MirLayout::List { element } => {
                let MirRuntimeValue::List(elements) = value else {
                    return Err(
                        self.error(&function.owner, "List drop value is not a canonical List")
                    );
                };
                for element_value in elements {
                    self.drop_runtime_value(function, element, element_value)?;
                }
                Ok(())
            }
            MirLayout::Set { element } => {
                let MirRuntimeValue::Set(elements) = value else {
                    return Err(
                        self.error(&function.owner, "Set drop value is not a canonical Set")
                    );
                };
                for element_value in elements {
                    self.drop_runtime_value(function, element, element_value)?;
                }
                Ok(())
            }
            MirLayout::Handle if matches!(&value, MirRuntimeValue::String(_)) => Ok(()),
            MirLayout::Opaque => Err(self.error(
                &function.owner,
                "non-Copy opaque value has no canonical drop implementation",
            )),
            MirLayout::Option { .. } | MirLayout::Result { .. } | MirLayout::Enum { .. } => {
                let MirRuntimeValue::Variant {
                    nominal,
                    variant,
                    mut payload,
                } = value
                else {
                    return Err(self.error(
                        &function.owner,
                        "variant drop value is not a canonical Variant",
                    ));
                };
                let (expected_nominal, plan) = self
                    .program
                    .type_catalog()
                    .validated_variant_drop_contract(ty, &variant)
                    .map_err(|message| self.error(&function.owner, message))?;
                if nominal.as_str() != expected_nominal {
                    return Err(self.error(
                        &function.owner,
                        "variant drop nominal disagrees with TypeDesc",
                    ));
                }
                if payload.len() != plan.fields.len() {
                    return Err(self.error(
                        &function.owner,
                        "variant drop value disagrees with TypeDesc arity",
                    ));
                }
                for field in &plan.fields {
                    let child = std::mem::replace(
                        payload.get_mut(field.index).ok_or_else(|| {
                            self.error(&function.owner, "variant drop field is out of bounds")
                        })?,
                        MirRuntimeValue::Unit,
                    );
                    self.drop_runtime_value(function, &field.ty, child)?;
                }
                Ok(())
            }
            _ => Err(self.error(
                &function.owner,
                "non-Copy value has no canonical reference drop implementation",
            )),
        }
    }

    fn select_switch_arm<'b>(
        &self,
        function: &MirFunction,
        value: &MirRuntimeValue,
        arms: &'b [MirSwitchArm],
    ) -> Result<&'b MirSwitchArm, MirExecutionError> {
        let mut default = None;
        for arm in arms {
            match &arm.case {
                MirSwitchCase::Literal(literal) if &runtime_literal(literal) == value => {
                    return Ok(arm);
                }
                MirSwitchCase::Variant(variant) if matches!(value, MirRuntimeValue::Variant { variant: actual, .. } if actual == variant) =>
                {
                    return Ok(arm);
                }
                MirSwitchCase::Default => default = Some(arm),
                MirSwitchCase::Variant(_) => {}
                MirSwitchCase::Literal(_) => {}
            }
        }
        default.ok_or_else(|| self.error(&function.owner, "switch has no matching arm"))
    }

    fn switch_arguments(
        &self,
        function: &MirFunction,
        values: &mut HashMap<MirValueId, MirRuntimeValue>,
        scrutinee_id: &MirValueId,
        value: &MirRuntimeValue,
        arm: &MirSwitchArm,
    ) -> Result<Vec<MirRuntimeValue>, MirExecutionError> {
        let mut incoming = self.take_transfer_values(function, values, &arm.arguments)?;
        let scrutinee_ty = function
            .values
            .get(scrutinee_id)
            .map(|value| &value.ty)
            .ok_or_else(|| self.error(&function.owner, "switch scrutinee has no MIR type"))?;
        let Some((expected_nominal, _)) = self.program.type_catalog().variant_layout(scrutinee_ty)
        else {
            return Ok(incoming);
        };
        let MirRuntimeValue::Variant {
            nominal: actual_nominal,
            variant: actual_variant,
            payload,
        } = value
        else {
            return Err(self.error(
                &function.owner,
                "variant switch payload binding received a non-variant value",
            ));
        };
        if actual_nominal.as_str() != expected_nominal {
            return Err(self.error(
                &function.owner,
                "runtime variant nominal disagrees with scrutinee TypeDesc",
            ));
        }
        if !matches!(&arm.case, MirSwitchCase::Variant(case) if case == actual_variant) {
            return Err(self.error(
                &function.owner,
                "switch payload binding case disagrees with runtime variant",
            ));
        }
        if arm.bindings.is_empty() {
            return Ok(incoming);
        }
        for (index, binding) in arm.bindings.iter().enumerate() {
            let target_parameter = function
                .blocks
                .get(&arm.target)
                .and_then(|block| block.parameters.get(arm.arguments.len() + index))
                .ok_or_else(|| self.error(&function.owner, "switch binding target is absent"))?;
            let parameter = function
                .values
                .get(&target_parameter.value)
                .ok_or_else(|| {
                    self.error(&function.owner, "switch binding target type is absent")
                })?;
            if binding.parameter != target_parameter.value {
                return Err(self.error(
                    &function.owner,
                    "switch binding parameter disagrees with target block parameter",
                ));
            }
            if binding.projection.variant != *actual_variant
                || binding.projection.nominal.as_str() != actual_nominal.as_str()
                || binding.projection.arity != payload.len()
                || binding.projection.field_ty != parameter.ty
            {
                return Err(self.error(
                    &function.owner,
                    "variant payload projection receipt disagrees with runtime value",
                ));
            }
            let field = payload
                .get(binding.projection.field_index)
                .cloned()
                .ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "runtime variant payload is shorter than TypeDesc",
                    )
                })?;
            incoming.push(field);
        }
        Ok(incoming)
    }

    fn switch_move_arguments(
        &self,
        function: &MirFunction,
        values: &mut HashMap<MirValueId, MirRuntimeValue>,
        scrutinee_id: &MirValueId,
        arm: &MirSwitchArm,
    ) -> Result<Vec<MirRuntimeValue>, MirExecutionError> {
        let mut incoming = self.take_transfer_values(function, values, &arm.arguments)?;
        let scrutinee_ty = function
            .values
            .get(scrutinee_id)
            .map(|value| value.ty.clone())
            .ok_or_else(|| self.error(&function.owner, "switch-move scrutinee has no MIR type"))?;
        let scrutinee = values.remove(scrutinee_id).ok_or_else(|| {
            self.error(
                &function.owner,
                format!("switch-move source '{}' is unavailable", scrutinee_id),
            )
        })?;
        let MirRuntimeValue::Variant {
            nominal: actual_nominal,
            variant: actual_variant,
            mut payload,
        } = scrutinee
        else {
            return Err(self.error(
                &function.owner,
                "switch-move scrutinee is not a canonical Variant",
            ));
        };
        let (expected_nominal, plan) = self
            .program
            .type_catalog()
            .validated_variant_drop_contract(&scrutinee_ty, &actual_variant)
            .map_err(|message| self.error(&function.owner, message))?;
        if actual_nominal.as_str() != expected_nominal {
            return Err(self.error(
                &function.owner,
                "switch-move variant nominal disagrees with TypeDesc",
            ));
        }
        if payload.len() != plan.fields.len() {
            return Err(self.error(
                &function.owner,
                "switch-move payload arity disagrees with TypeDesc",
            ));
        }
        if !matches!(&arm.case, MirSwitchCase::Variant(case) if case == &actual_variant)
            && !matches!(&arm.case, MirSwitchCase::Default)
        {
            return Err(self.error(
                &function.owner,
                "switch-move arm disagrees with runtime variant",
            ));
        }
        if matches!(&arm.case, MirSwitchCase::Default) || arm.bindings.is_empty() {
            let value = MirRuntimeValue::Variant {
                nominal: actual_nominal,
                variant: actual_variant,
                payload,
            };
            self.drop_runtime_value(function, &scrutinee_ty, value)?;
            return Ok(incoming);
        }
        let plan = plan.clone();
        let mut bound_indices = BTreeMap::new();
        for (binding_index, binding) in arm.bindings.iter().enumerate() {
            let target_parameter = function
                .blocks
                .get(&arm.target)
                .and_then(|block| block.parameters.get(arm.arguments.len() + binding_index))
                .ok_or_else(|| {
                    self.error(&function.owner, "switch-move binding target is absent")
                })?;
            let parameter = function
                .values
                .get(&target_parameter.value)
                .ok_or_else(|| {
                    self.error(&function.owner, "switch-move binding target type is absent")
                })?;
            if binding.parameter != target_parameter.value {
                return Err(self.error(
                    &function.owner,
                    "switch-move binding parameter disagrees with target block parameter",
                ));
            }
            if binding.projection.variant != actual_variant
                || binding.projection.nominal.as_str() != actual_nominal.as_str()
                || binding.projection.arity != payload.len()
                || binding.projection.field_ty != parameter.ty
            {
                return Err(self.error(
                    &function.owner,
                    "switch-move payload projection receipt disagrees with runtime value",
                ));
            }
            let index = binding.projection.field_index;
            if bound_indices
                .insert(binding.projection.field.clone(), index)
                .is_some()
            {
                return Err(self.error(&function.owner, "switch-move binding field is repeated"));
            }
        }
        let mut bound_values = BTreeMap::new();
        for field in plan.fields {
            let child = std::mem::replace(
                payload.get_mut(field.index).ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "switch-move payload field is out of bounds",
                    )
                })?,
                MirRuntimeValue::Unit,
            );
            if bound_indices.values().any(|index| *index == field.index) {
                bound_values.insert(field.index, child);
            } else {
                self.drop_runtime_value(function, &field.ty, child)?;
            }
        }
        for binding in &arm.bindings {
            let index = *bound_indices
                .get(&binding.projection.field)
                .ok_or_else(|| {
                    self.error(&function.owner, "switch-move binding index is absent")
                })?;
            let value = bound_values.remove(&index).ok_or_else(|| {
                self.error(&function.owner, "switch-move binding was consumed twice")
            })?;
            incoming.push(value);
        }
        Ok(incoming)
    }

    fn error(&self, function: &NodeId, message: impl Into<String>) -> MirExecutionError {
        MirExecutionError {
            function: function.clone(),
            message: message.into(),
        }
    }

    fn integer_width(&self, function: &MirFunction, value: &MirValueId) -> Option<u16> {
        let ty = function.values.get(value)?.ty.clone();
        let descriptor = self.program.type_catalog().get(&ty)?;
        match descriptor.abi {
            super::types::MirAbiClass::Integer { bits, signed: true } => Some(bits),
            _ => None,
        }
    }
}

fn runtime_literal(literal: &ResolvedLiteral) -> MirRuntimeValue {
    match literal {
        ResolvedLiteral::Int(value) => MirRuntimeValue::Int(*value),
        ResolvedLiteral::FloatBits(value) => MirRuntimeValue::FloatBits(*value),
        ResolvedLiteral::Bool(value) => MirRuntimeValue::Bool(*value),
        ResolvedLiteral::String(value) => MirRuntimeValue::String(value.clone()),
        ResolvedLiteral::Unit => MirRuntimeValue::Unit,
    }
}

fn project_variant_value(
    function: &NodeId,
    value: MirRuntimeValue,
    base_ty: &crate::core::ResolvedTypeId,
    result_ty: &crate::core::ResolvedTypeId,
    receipt: &super::types::MirVariantProjectionTrapContract,
    type_catalog: &MirTypeCatalog,
) -> Result<MirRuntimeValue, MirExecutionError> {
    type_catalog
        .validate_variant_projection_trap_receipt(base_ty, result_ty, receipt)
        .map_err(|message| execution_error(function, message))?;
    let MirRuntimeValue::Variant {
        nominal,
        variant,
        payload,
    } = value
    else {
        return Err(execution_error(
            function,
            "direct variant projection base is not a canonical Variant",
        ));
    };
    if nominal != receipt.projection.nominal {
        return Err(execution_error(
            function,
            "direct variant projection runtime nominal disagrees with TypeDesc",
        ));
    }
    if variant != receipt.projection.variant {
        return Err(execution_error(
            function,
            format!(
                "{}: canonical direct variant projection expected active variant '{}'",
                receipt.trap_code, receipt.variant_name
            ),
        ));
    }
    if payload.len() != receipt.projection.arity {
        return Err(execution_error(
            function,
            "direct variant projection runtime arity disagrees with TypeDesc",
        ));
    }
    payload
        .get(receipt.projection.field_index)
        .cloned()
        .ok_or_else(|| execution_error(function, "direct variant projection field is absent"))
}

fn move_project_variant_value(
    function: &NodeId,
    value: MirRuntimeValue,
    base_ty: &crate::core::ResolvedTypeId,
    result_ty: &crate::core::ResolvedTypeId,
    receipt: &super::types::MirVariantProjectionTrapContract,
    type_catalog: &MirTypeCatalog,
) -> Result<MirRuntimeValue, MirExecutionError> {
    type_catalog
        .validate_variant_move_projection_trap_receipt(base_ty, result_ty, receipt)
        .map_err(|message| execution_error(function, message))?;
    let MirRuntimeValue::Variant {
        nominal,
        variant,
        mut payload,
    } = value
    else {
        return Err(execution_error(
            function,
            "consuming direct variant projection base is not a canonical Variant",
        ));
    };
    if nominal != receipt.projection.nominal {
        return Err(execution_error(
            function,
            "consuming direct variant projection runtime nominal disagrees with TypeDesc",
        ));
    }
    if variant != receipt.projection.variant {
        return Err(execution_error(
            function,
            format!(
                "{}: canonical MIR consuming direct variant projection expected active variant '{}'",
                receipt.trap_code, receipt.variant_name
            ),
        ));
    }
    if payload.len() != receipt.projection.arity {
        return Err(execution_error(
            function,
            "consuming direct variant projection runtime arity disagrees with TypeDesc",
        ));
    }
    payload
        .get_mut(receipt.projection.field_index)
        .map(|value| std::mem::replace(value, MirRuntimeValue::Unit))
        .ok_or_else(|| {
            execution_error(
                function,
                "consuming direct variant projection field is absent",
            )
        })
}

fn project_value(
    function: &NodeId,
    value: MirRuntimeValue,
    base_ty: Option<&crate::core::ResolvedTypeId>,
    result_ty: Option<&crate::core::ResolvedTypeId>,
    projection: &MirProjection,
    index_value: Option<&MirRuntimeValue>,
    index_ty: Option<&crate::core::ResolvedTypeId>,
    list_index_contract: Option<&super::types::MirListIndexProjectionContract>,
    type_catalog: &MirTypeCatalog,
) -> Result<MirRuntimeValue, MirExecutionError> {
    match (value, projection) {
        (MirRuntimeValue::Tuple(values), MirProjection::Tuple(index)) => {
            let Some(base_ty) = base_ty else {
                return Err(execution_error(
                    function,
                    "tuple projection has no base type",
                ));
            };
            let Some(result_ty) = result_ty else {
                return Err(execution_error(
                    function,
                    "tuple projection has no result type",
                ));
            };
            let contract = type_catalog
                .validated_tuple_field_projection_contract(base_ty, *index, result_ty)
                .map_err(|message| execution_error(function, message))?;
            if values.len() != contract.arity {
                return Err(execution_error(
                    function,
                    "tuple runtime arity disagrees with projection contract",
                ));
            }
            values
                .get(contract.field_index)
                .cloned()
                .ok_or_else(|| execution_error(function, "tuple field is out of bounds"))
        }
        (MirRuntimeValue::Record { nominal, fields }, MirProjection::Field(field)) => {
            let Some(base_ty) = base_ty else {
                return Err(execution_error(
                    function,
                    "record projection has no base type",
                ));
            };
            let Some(result_ty) = result_ty else {
                return Err(execution_error(
                    function,
                    "record projection has no result type",
                ));
            };
            let contract = type_catalog
                .validated_record_field_projection_contract(base_ty, field, result_ty)
                .map_err(|message| execution_error(function, message))?;
            if nominal != contract.nominal {
                return Err(execution_error(
                    function,
                    "record runtime nominal disagrees with projection contract",
                ));
            }
            if fields.len() != contract.arity {
                return Err(execution_error(
                    function,
                    "record runtime field arity disagrees with projection contract",
                ));
            }
            if contract.field != *field {
                return Err(execution_error(
                    function,
                    "record projection field identity disagrees with contract",
                ));
            }
            fields
                .get(contract.field_index)
                .cloned()
                .ok_or_else(|| execution_error(function, "record field vector is too short"))
        }
        (value, MirProjection::Dereference) => Ok(value),
        (MirRuntimeValue::List(values), MirProjection::Index(_)) => {
            let Some(base_ty) = base_ty else {
                return Err(execution_error(
                    function,
                    "List index projection has no base type",
                ));
            };
            let Some(result_ty) = result_ty else {
                return Err(execution_error(
                    function,
                    "List index projection has no result type",
                ));
            };
            let Some(receipt) = list_index_contract else {
                return Err(execution_error(
                    function,
                    "List index projection has no canonical receipt",
                ));
            };
            let Some(index_ty) = index_ty else {
                return Err(execution_error(
                    function,
                    "List index projection has no index type",
                ));
            };
            if receipt.list_ty != *base_ty
                || receipt.element_ty != *result_ty
                || receipt.result_ty != *result_ty
                || receipt.index_ty != *index_ty
            {
                return Err(execution_error(
                    function,
                    "List index projection receipt disagrees with MIR value types",
                ));
            }
            let raw = match (projection, index_value) {
                (MirProjection::Index(_), Some(MirRuntimeValue::Int(index))) => *index,
                _ => {
                    return Err(execution_error(
                        function,
                        "List index runtime value is not a signed integer",
                    ))
                }
            };
            let index = canonical_list_index(function, raw, values.len())?;
            values.get(index).cloned().ok_or_else(|| {
                execution_error(
                    function,
                    "List index E0803 bounds check lost selected element",
                )
            })
        }
        (_, MirProjection::Index(_)) => Err(execution_error(
            function,
            "indexed projection requires a canonical List value",
        )),
        _ => Err(execution_error(
            function,
            "projection does not match aggregate value",
        )),
    }
}

fn canonical_list_index(
    function: &NodeId,
    raw: i64,
    length: usize,
) -> Result<usize, MirExecutionError> {
    let index = if raw < 0 {
        let distance = raw.unsigned_abs();
        if distance > length as u64 {
            return Err(execution_error(
                function,
                format!("index E0803 out of bounds (index {raw}, len {length})"),
            ));
        }
        length - distance as usize
    } else {
        let index = raw as u64;
        if index >= length as u64 {
            return Err(execution_error(
                function,
                format!("index E0803 out of bounds (index {raw}, len {length})"),
            ));
        }
        index as usize
    };
    Ok(index)
}

fn move_project_value(
    function: &NodeId,
    value: MirRuntimeValue,
    base_ty: &crate::core::ResolvedTypeId,
    result_ty: &crate::core::ResolvedTypeId,
    projection: &MirProjection,
    type_catalog: &MirTypeCatalog,
) -> Result<MirRuntimeValue, MirExecutionError> {
    let MirRuntimeValue::Record {
        nominal,
        mut fields,
    } = value
    else {
        return Err(execution_error(
            function,
            "move projection base is not a record",
        ));
    };
    let MirProjection::Field(field) = projection else {
        return Err(execution_error(
            function,
            "move projection requires a direct record field",
        ));
    };
    let contract = type_catalog
        .validated_record_field_projection_contract(base_ty, field, result_ty)
        .map_err(|message| execution_error(function, message))?;
    if nominal != contract.nominal || fields.len() != contract.arity {
        return Err(execution_error(
            function,
            "move projection record disagrees with TypeDesc",
        ));
    }
    Ok(std::mem::replace(
        fields
            .get_mut(contract.field_index)
            .ok_or_else(|| execution_error(function, "move projection field is out of bounds"))?,
        MirRuntimeValue::Unit,
    ))
}

fn move_project_record_drop_value(
    function: &NodeId,
    value: MirRuntimeValue,
    base_ty: &crate::core::ResolvedTypeId,
    result_ty: &crate::core::ResolvedTypeId,
    receipt: &super::types::MirRecordMoveProjectionDropContract,
    type_catalog: &MirTypeCatalog,
) -> Result<
    (
        MirRuntimeValue,
        Vec<(crate::core::ResolvedTypeId, MirRuntimeValue)>,
    ),
    MirExecutionError,
> {
    type_catalog
        .validate_record_move_projection_drop_receipt(base_ty, result_ty, receipt)
        .map_err(|message| execution_error(function, message))?;
    let MirRuntimeValue::Record {
        nominal,
        mut fields,
    } = value
    else {
        return Err(execution_error(
            function,
            "record move/drop projection base is not a record",
        ));
    };
    if nominal != receipt.projection.nominal {
        return Err(execution_error(
            function,
            "record move/drop projection nominal disagrees with TypeDesc",
        ));
    }
    if fields.len() != receipt.projection.arity {
        return Err(execution_error(
            function,
            "record move/drop projection runtime arity disagrees with TypeDesc",
        ));
    }
    if receipt.projection.field_index >= fields.len() {
        return Err(execution_error(
            function,
            "record move/drop projection field is out of bounds",
        ));
    }
    let projected = std::mem::replace(
        &mut fields[receipt.projection.field_index],
        MirRuntimeValue::Unit,
    );
    let mut residual = Vec::with_capacity(receipt.residual.len());
    for field in &receipt.residual {
        if field.index >= fields.len() {
            return Err(execution_error(
                function,
                format!(
                    "record move/drop projection residual field '{}' is out of bounds",
                    field.name
                ),
            ));
        }
        let value = std::mem::replace(&mut fields[field.index], MirRuntimeValue::Unit);
        residual.push((field.ty.clone(), value));
    }
    if fields
        .iter()
        .any(|field| !matches!(field, MirRuntimeValue::Unit))
    {
        return Err(execution_error(
            function,
            "record move/drop projection has runtime fields outside its TypeDesc receipt",
        ));
    }
    Ok((projected, residual))
}

fn evaluate_unary(
    function: &NodeId,
    op: ResolvedUnaryOp,
    operand: MirRuntimeValue,
    integer_width: Option<u16>,
) -> Result<MirRuntimeValue, MirExecutionError> {
    match (op, operand) {
        (ResolvedUnaryOp::Negate, MirRuntimeValue::Int(value)) if integer_width == Some(32) => {
            i32::try_from(value)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?
                .checked_neg()
                .map(|value| MirRuntimeValue::Int(value as i64))
                .ok_or_else(|| execution_error(function, "integer negation overflow"))
        }
        (ResolvedUnaryOp::Negate, MirRuntimeValue::Int(value)) => value
            .checked_neg()
            .map(MirRuntimeValue::Int)
            .ok_or_else(|| execution_error(function, "integer negation overflow")),
        (ResolvedUnaryOp::Negate, MirRuntimeValue::FloatBits(value)) => Ok(
            MirRuntimeValue::FloatBits((-f64::from_bits(value)).to_bits()),
        ),
        (ResolvedUnaryOp::Not, MirRuntimeValue::Bool(value)) => Ok(MirRuntimeValue::Bool(!value)),
        (ResolvedUnaryOp::Not, MirRuntimeValue::Int(value)) => Ok(MirRuntimeValue::Int(!value)),
        (ResolvedUnaryOp::BorrowShared | ResolvedUnaryOp::BorrowMutable, value)
        | (ResolvedUnaryOp::Dereference, value) => Ok(value),
        _ => Err(execution_error(function, "unsupported unary operand type")),
    }
}

fn evaluate_binary(
    function: &NodeId,
    op: ResolvedBinaryOp,
    left: MirRuntimeValue,
    right: MirRuntimeValue,
    integer_width: Option<u16>,
) -> Result<MirRuntimeValue, MirExecutionError> {
    use MirRuntimeValue::{Bool, Int};
    match (op, left, right) {
        (ResolvedBinaryOp::Add, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            let right = i32::try_from(right)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            left.checked_add(right)
                .map(|value| Int(value as i64))
                .ok_or_else(|| execution_error(function, "integer addition overflow"))
        }
        (ResolvedBinaryOp::Subtract, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            let right = i32::try_from(right)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            left.checked_sub(right)
                .map(|value| Int(value as i64))
                .ok_or_else(|| execution_error(function, "integer subtraction overflow"))
        }
        (ResolvedBinaryOp::Multiply, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            let right = i32::try_from(right)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            left.checked_mul(right)
                .map(|value| Int(value as i64))
                .ok_or_else(|| execution_error(function, "integer multiplication overflow"))
        }
        (ResolvedBinaryOp::Divide, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            let right = i32::try_from(right)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            if right == 0 {
                Err(execution_error(function, "integer division by zero"))
            } else {
                left.checked_div(right)
                    .map(|value| Int(value as i64))
                    .ok_or_else(|| execution_error(function, "integer division overflow"))
            }
        }
        (ResolvedBinaryOp::Remainder, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            let right = i32::try_from(right)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            if right == 0 {
                Err(execution_error(function, "integer remainder by zero"))
            } else {
                left.checked_rem(right)
                    .map(|value| Int(value as i64))
                    .ok_or_else(|| execution_error(function, "integer remainder overflow"))
            }
        }
        (ResolvedBinaryOp::Add, Int(left), Int(right)) => left
            .checked_add(right)
            .map(Int)
            .ok_or_else(|| execution_error(function, "integer addition overflow")),
        (ResolvedBinaryOp::Subtract, Int(left), Int(right)) => left
            .checked_sub(right)
            .map(Int)
            .ok_or_else(|| execution_error(function, "integer subtraction overflow")),
        (ResolvedBinaryOp::Multiply, Int(left), Int(right)) => left
            .checked_mul(right)
            .map(Int)
            .ok_or_else(|| execution_error(function, "integer multiplication overflow")),
        (ResolvedBinaryOp::Divide, Int(left), Int(right)) => {
            if right == 0 {
                Err(execution_error(function, "integer division by zero"))
            } else {
                left.checked_div(right)
                    .map(Int)
                    .ok_or_else(|| execution_error(function, "integer division overflow"))
            }
        }
        (ResolvedBinaryOp::Remainder, Int(left), Int(right)) => {
            if right == 0 {
                Err(execution_error(function, "integer remainder by zero"))
            } else {
                left.checked_rem(right)
                    .map(Int)
                    .ok_or_else(|| execution_error(function, "integer remainder overflow"))
            }
        }
        (ResolvedBinaryOp::Equal, left, right) => Ok(Bool(left == right)),
        (ResolvedBinaryOp::NotEqual, left, right) => Ok(Bool(left != right)),
        (ResolvedBinaryOp::Less, Int(left), Int(right)) => Ok(Bool(left < right)),
        (ResolvedBinaryOp::Greater, Int(left), Int(right)) => Ok(Bool(left > right)),
        (ResolvedBinaryOp::LessEqual, Int(left), Int(right)) => Ok(Bool(left <= right)),
        (ResolvedBinaryOp::GreaterEqual, Int(left), Int(right)) => Ok(Bool(left >= right)),
        (ResolvedBinaryOp::LogicalAnd, Bool(left), Bool(right)) => Ok(Bool(left && right)),
        (ResolvedBinaryOp::LogicalOr, Bool(left), Bool(right)) => Ok(Bool(left || right)),
        (ResolvedBinaryOp::BitAnd, Int(left), Int(right)) => Ok(Int(left & right)),
        (ResolvedBinaryOp::BitOr, Int(left), Int(right)) => Ok(Int(left | right)),
        (ResolvedBinaryOp::BitXor, Int(left), Int(right)) => Ok(Int(left ^ right)),
        (ResolvedBinaryOp::ShiftLeft, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            if right < 0 {
                return Err(execution_error(function, "negative shift amount"));
            }
            Ok(Int(left.wrapping_shl((right as u32) & 31) as i64))
        }
        (ResolvedBinaryOp::ShiftRight, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            if right < 0 {
                return Err(execution_error(function, "negative shift amount"));
            }
            Ok(Int(left.wrapping_shr((right as u32) & 31) as i64))
        }
        (ResolvedBinaryOp::Power, Int(left), Int(right)) if integer_width == Some(32) => {
            let left = i32::try_from(left)
                .map_err(|_| execution_error(function, "i32 operand out of range"))?;
            if right < 0 {
                return Err(execution_error(function, "negative integer power"));
            }
            let mut result = 1_i32;
            for _ in 0..right {
                result = result.wrapping_mul(left);
            }
            Ok(Int(result as i64))
        }
        (ResolvedBinaryOp::ShiftLeft, Int(left), Int(right)) if (0..64).contains(&right) => {
            Ok(Int(left.wrapping_shl(right as u32)))
        }
        (ResolvedBinaryOp::ShiftRight, Int(left), Int(right)) if (0..64).contains(&right) => {
            Ok(Int(left.wrapping_shr(right as u32)))
        }
        (ResolvedBinaryOp::Power, Int(left), Int(right)) if right >= 0 => {
            let mut result = 1_i64;
            for _ in 0..right {
                result = result
                    .checked_mul(left)
                    .ok_or_else(|| execution_error(function, "integer power overflow"))?;
            }
            Ok(Int(result))
        }
        _ => Err(execution_error(
            function,
            "unsupported binary operand type or operation",
        )),
    }
}

fn execution_error(function: &NodeId, message: impl Into<String>) -> MirExecutionError {
    MirExecutionError {
        function: function.clone(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{MirProgram, MirProgramBuildError, MirReferenceInterpreter, MirRuntimeValue};
    use crate::core::mir::lower::{lower_body, lower_program};
    use crate::core::mir::{
        MirAggregateKind, MirGenericInstanceContract, MirInstruction, MirInstructionKind,
    };
    use crate::core::{NodeId, ResolvedCallee};
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn lower_main(source: &str) -> (crate::core::NodeId, MirProgram) {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let callable = checked
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .expect("main callable");
        let function = lower_body(&callable.body).expect("MIR lowering");
        let owner = function.owner.clone();
        let program = MirProgram::single(function).expect("MIR validation");
        (owner, program)
    }

    fn lower_program_with_main(source: &str) -> (crate::core::NodeId, MirProgram) {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let owner = checked
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .map(|callable| callable.owner.clone())
            .expect("main callable");
        let program = MirProgram::new(lower_program(&checked).expect("MIR lowering"))
            .expect("MIR validation");
        (owner, program)
    }

    fn canonical_program_with_main(source: &str) -> (crate::core::NodeId, MirProgram) {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let owner = checked
            .callables()
            .values()
            .find(|callable| callable.owner.0.ends_with("main"))
            .map(|callable| callable.owner.clone())
            .expect("main callable");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        (owner, program)
    }

    #[test]
    fn executes_scalar_arithmetic_without_backend() {
        let (owner, program) = lower_main("func main() -> i32 { 40 + 2 }");
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn executes_parameter_load_and_arithmetic() {
        let (owner, program) = lower_main("func main(x: i32) -> i32 { x + 1 }");
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[MirRuntimeValue::Int(41)])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn executes_both_if_control_flow_paths() {
        for (source, expected) in [
            (
                "func main(flag: bool) -> i32 { if flag { 1 } else { 2 } }",
                1,
            ),
            (
                "func main(flag: bool) -> i32 { if flag { 1 } else { 2 } }",
                2,
            ),
        ] {
            let (owner, program) = lower_main(source);
            let flag = expected == 1;
            let value = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[MirRuntimeValue::Bool(flag)])
                .expect("reference execution");
            assert_eq!(value, MirRuntimeValue::Int(expected));
        }
    }

    #[test]
    fn executes_literal_match_and_default_arm() {
        let (owner, program) =
            lower_main("func main(x: i32) -> i32 { match x { 0 => 1, _ => 2 } }");
        let interpreter = MirReferenceInterpreter::new(&program);
        assert_eq!(
            interpreter
                .execute(&owner, &[MirRuntimeValue::Int(0)])
                .expect("match literal"),
            MirRuntimeValue::Int(1)
        );
        assert_eq!(
            interpreter
                .execute(&owner, &[MirRuntimeValue::Int(9)])
                .expect("match default"),
            MirRuntimeValue::Int(2)
        );
    }

    #[test]
    fn executes_while_false_without_entering_the_body() {
        let (owner, program) = lower_main("func main() -> i32 { while false { 1 } 42 }");
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn executes_break_as_an_explicit_loop_exit_edge() {
        let (owner, program) = lower_main("func main() -> i32 { while true { break } 42 }");
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn executes_tuple_construction_as_a_first_class_mir_value() {
        let (owner, program) = lower_main("func main() -> (i32, i32) { (1, 2) }");
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(
            value,
            MirRuntimeValue::Tuple(vec![MirRuntimeValue::Int(1), MirRuntimeValue::Int(2)])
        );
    }

    #[test]
    fn executes_tuple_projection_through_the_type_desc_receipt() {
        let (owner, program) =
            canonical_program_with_main("func main() -> i32 { let pair = (40, 2); pair.0 }");
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference tuple projection");
        assert_eq!(value, MirRuntimeValue::Int(40));
    }

    #[test]
    fn executes_a_call_against_the_same_canonical_program() {
        let source = "func add_one(x: i32) -> i32 { x + 1 }\nfunc main() -> i32 { add_one(41) }";
        let (owner, program) = lower_program_with_main(source);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_abs_has_a_first_class_node_and_reference_oracle() {
        let (owner, program) = canonical_program_with_main(
            "func abs_i64(value: i64) -> i64 { abs(value) }\nfunc main() -> i32 { if abs_i64(-4294967297) == 4294967297 { 42 } else { 0 } }",
        );
        assert!(program.functions().values().any(|function| {
            function
                .blocks
                .values()
                .flat_map(|block| block.instructions.iter())
                .any(|instruction| {
                    matches!(
                        &instruction.kind,
                        crate::core::mir::MirInstructionKind::BuiltinCall {
                            kind: crate::core::mir::types::MirBuiltinKind::Abs,
                            ..
                        }
                    )
                })
        }));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference abs");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_abs_supports_f64_through_the_same_contract() {
        let (owner, program) =
            canonical_program_with_main("func main() -> f64 { let value: f64 = -2.5; abs(value) }");
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference f64 abs");
        assert_eq!(value, MirRuntimeValue::FloatBits(2.5_f64.to_bits()));
    }

    #[test]
    fn canonical_abs_rejects_i32_before_any_backend() {
        let source = "func main() -> i32 { abs(5) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("i32 abs is outside the canonical builtin contract");
        match error {
            MirProgramBuildError::Validation(errors) => assert!(errors.iter().any(|error| {
                error.message.contains("builtin 'abs'")
                    && error
                        .message
                        .contains("canonical contract accepts signed i64 or f64")
            })),
            other => panic!("unsupported builtin ABI escaped the validator: {other:?}"),
        }
    }

    #[test]
    fn canonical_min_max_have_first_class_nodes_and_reference_oracle() {
        let (owner, program) = canonical_program_with_main(
            "func min_i64(left: i64, right: i64) -> i64 { min(left, right) }\nfunc max_i64(left: i64, right: i64) -> i64 { max(left, right) }\nfunc main() -> i32 { if min_i64(9223372036854775806, 9223372036854775807) == 9223372036854775806 { if max_i64(-9223372036854775807, 9223372036854775806) == 9223372036854775806 { 42 } else { 0 } } else { 0 } }",
        );
        let kinds: Vec<_> = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::BuiltinCall { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert!(kinds.contains(&crate::core::mir::types::MirBuiltinKind::Min));
        assert!(kinds.contains(&crate::core::mir::types::MirBuiltinKind::Max));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference min/max");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_min_rejects_f64_before_any_backend() {
        let source = "func main() -> f64 { min(1.0, 2.0) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("f64 min is outside the first finite scalar contract");
        match error {
            MirProgramBuildError::Validation(errors) => assert!(errors.iter().any(|error| {
                error.message.contains("builtin 'min'")
                    && error
                        .message
                        .contains("canonical contract accepts signed i64")
            })),
            other => panic!("unsupported min ABI escaped the validator: {other:?}"),
        }
    }

    #[test]
    fn canonical_i32_to_i64_conversion_feeds_min_max_reference_oracle() {
        let (owner, program) = canonical_program_with_main(
            "func min_i64(left: i32, right: i32) -> i64 { min(left as i64, right as i64) }\nfunc main() -> i32 { if min_i64(1, 2) == 1 { 42 } else { 0 } }",
        );
        let instructions = program
            .functions()
            .values()
            .flat_map(|function| function.blocks.values())
            .flat_map(|block| block.instructions.iter())
            .collect::<Vec<_>>();
        assert!(instructions.iter().any(|instruction| matches!(
            &instruction.kind,
            crate::core::mir::MirInstructionKind::Convert { .. }
        )));
        assert!(instructions.iter().any(|instruction| matches!(
            &instruction.kind,
            crate::core::mir::MirInstructionKind::BuiltinCall {
                kind: crate::core::mir::types::MirBuiltinKind::Min,
                ..
            }
        )));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference i32 to i64 conversion");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_native_scalar_slice_has_a_reference_oracle() {
        let (owner, program) = canonical_program_with_main(
            "func choose(flag: bool, when_true: i32, when_false: i32) -> i32 { if flag { when_true } else { when_false } }\nfunc main() -> i32 { let selected = choose(true, 40, 0); let magnitude = abs(7 as i64); if (selected + 2) == 42 { if magnitude == (7 as i64) { 42 } else { 0 } } else { 0 } }",
        );
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference native scalar slice");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_native_flat_record_slice_has_a_reference_oracle() {
        let (owner, program) = canonical_program_with_main(
            "type Point { x: i32, enabled: bool }\nfunc make_point(x: i32, enabled: bool) -> Point { Point { enabled: enabled, x: x } }\nfunc main() -> i32 { let point = make_point(40, true); if point.enabled { point.x + 2 } else { 0 } }",
        );
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_i32_to_f64_conversion_rejects_before_any_backend() {
        let source = "func main() -> f64 { let value: i32 = 7; value as f64 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("i32 to f64 remains outside the canonical conversion contract");
        match error {
            MirProgramBuildError::Validation(errors) => assert!(errors.iter().any(|error| {
                error.message.contains("conversion")
                    && error.message.contains("accepted: same Copy scalar type")
            })),
            other => panic!("unsupported conversion escaped the validator: {other:?}"),
        }
    }

    #[test]
    fn checked_program_constructor_closes_type_catalog_before_execution() {
        let source = "func main() -> i32 { 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert!(!program.type_catalog().is_empty());
    }

    #[test]
    fn generic_templates_are_not_executable_mir_functions() {
        let source = "func identity<T>(value: T) -> T { value }\nfunc main() -> i32 { 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("an unused generic template must not poison executable MIR");
        assert!(program
            .functions()
            .keys()
            .all(|owner| !owner.0.ends_with("identity")));
        assert!(program
            .functions()
            .contains_key(&NodeId("function:main".into())));
    }

    #[test]
    fn concrete_scalar_generic_identity_is_an_executable_mir_instance() {
        let source =
            "func identity<T>(value: T) -> T { value }\nfunc main() -> i32 { identity(41) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("a supported concrete generic call must materialize in MIR");
        assert_eq!(program.instances().len(), 1);
        let instance = program
            .instances()
            .values()
            .next()
            .expect("identity instance");
        assert_eq!(instance.template, NodeId("function:identity".into()));
        assert!(program.functions().contains_key(&instance.function));
        let main = program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("main MIR function");
        let (callee, type_arguments) = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call {
                    callee,
                    type_arguments,
                    ..
                } => Some((callee, type_arguments)),
                _ => None,
            })
            .expect("identity call");
        assert_eq!(
            callee,
            &crate::core::ir::ResolvedCallee::Function(instance.function.clone())
        );
        assert_eq!(type_arguments, &instance.arguments);

        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(41));
    }

    #[test]
    fn concrete_scalar_generic_record_projection_is_canonical_and_borrowed() {
        let source =
            include_str!("../../../tests/fixtures/mir_native_generic_record_projection.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("generic record projection must materialize in canonical MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("generic record projection instance");
        let MirGenericInstanceContract::ScalarRecordProjection { contract } = &instance.contract
        else {
            panic!("generic record projection must carry a record receipt");
        };
        assert_eq!(contract.arity, 1);
        assert_eq!(contract.name, "value");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("materialized generic record projection target");
        assert_eq!(contract.field_ty, target.result);
        let parameter_ty = target
            .parameters
            .first()
            .and_then(|parameter| target.values.get(parameter))
            .map(|value| value.ty.clone())
            .expect("record projection parameter TypeDesc");
        let parameter_desc = program
            .type_catalog()
            .get(&parameter_ty)
            .expect("record projection concrete TypeDesc");
        assert_eq!(
            parameter_desc.ownership,
            crate::core::mir::types::MirOwnership::Copy
        );
        assert_eq!(
            parameter_desc.abi,
            crate::core::mir::types::MirAbiClass::Aggregate
        );
        assert!(matches!(
            parameter_desc.layout,
            crate::core::mir::types::MirLayout::Record { ref fields, .. }
                if fields.len() == 1 && fields[0].ty == contract.field_ty
        ));
        assert!(target.canonical_text().contains("project"));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference generic record projection execution");
        assert_eq!(value, MirRuntimeValue::Int(41));
    }

    #[test]
    fn concrete_scalar_generic_record_projection_covers_i64_and_bool_abi_family() {
        for (source, expected) in [
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_record_projection_i64.mimi"
                ),
                MirRuntimeValue::Int(41),
            ),
            (
                include_str!(
                    "../../../tests/fixtures/mir_native_generic_record_projection_bool.mimi"
                ),
                MirRuntimeValue::Bool(true),
            ),
        ] {
            let tokens = Lexer::new(source).tokenize().expect("lex");
            let file = Parser::new(tokens).parse_file().expect("parse");
            let checked = crate::core::check_program(&file).expect("check");
            let program = MirProgram::from_checked_program(&checked)
                .expect("scalar generic record projection must materialize");
            let instance = program
                .instances()
                .values()
                .next()
                .expect("generic record projection instance");
            let MirGenericInstanceContract::ScalarRecordProjection { contract } =
                &instance.contract
            else {
                panic!("scalar generic record projection must carry a record receipt");
            };
            assert_eq!(contract.arity, 1);
            assert_eq!(contract.name, "value");
            let target = program
                .functions()
                .get(&instance.function)
                .expect("materialized generic record projection target");
            assert_eq!(contract.field_ty, target.result);
            let result_desc = program
                .type_catalog()
                .get(&target.result)
                .expect("concrete scalar result TypeDesc");
            assert_eq!(
                result_desc.ownership,
                crate::core::mir::types::MirOwnership::Copy
            );
            assert_eq!(
                result_desc.layout,
                crate::core::mir::types::MirLayout::Scalar
            );
            assert!(matches!(
                result_desc.abi,
                crate::core::mir::types::MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                } | crate::core::mir::types::MirAbiClass::Bool
            ));
            let value = MirReferenceInterpreter::new(&program)
                .execute(&NodeId("function:main".into()), &[])
                .expect("reference scalar generic record projection execution");
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn concrete_scalar_generic_record_projection_rvalue_call_keeps_construct_proof() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_rvalue.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("generic record Copy rvalue call must materialize");
        let main = program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("generic record Copy rvalue caller");
        let (call_index, call_argument) = main
            .blocks
            .values()
            .find_map(|block| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(index, instruction)| match &instruction.kind {
                        MirInstructionKind::Call { arguments, .. } => {
                            arguments.first().cloned().map(|argument| (index, argument))
                        }
                        _ => None,
                    })
            })
            .expect("generic record Copy rvalue call");
        assert!(matches!(
            main.blocks
                .values()
                .find_map(|block| block.instructions.get(call_index.saturating_sub(1))),
            Some(MirInstruction {
                kind: MirInstructionKind::Construct {
                    result,
                    kind: MirAggregateKind::Record { .. },
                    ..
                },
                ..
            }) if result == &call_argument
        ));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference generic record Copy rvalue execution");
        assert_eq!(value, MirRuntimeValue::Int(41));
    }

    #[test]
    fn scalar_generic_record_projection_indirect_argument_fails_closed() {
        let source = "type Box<T> { value: T }\nfunc get<T>(boxed: Box<T>) -> T { boxed.value }\nfunc main() -> i32 { let picked = get(if true { Box { value: 41 } } else { Box { value: 42 } }); picked }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("generic record Copy indirect call must fail closed");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(errors.iter().any(|error| {
                error.message.contains(
                    "generic scalar record projection call requires a direct local Clone or fresh Record Construct producer",
                )
            }), "{errors:?}"),
            other => panic!("generic record Copy indirect call crossed MIR gate: {other:?}"),
        }
    }

    #[test]
    fn scalar_generic_record_projection_call_producer_cannot_cross_mir_gate() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_record_projection_rvalue.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("generic record Copy rvalue call must materialize");
        let owner = NodeId("function:main".into());
        let mut main = program
            .functions()
            .get(&owner)
            .cloned()
            .expect("generic record Copy rvalue caller");
        let block =
            main.blocks
                .values_mut()
                .find(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(instruction.kind, MirInstructionKind::Call { .. })
                    })
                })
                .expect("generic record Copy rvalue call block");
        let call_index = block
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
            .expect("generic record Copy rvalue call");
        let replacement = block
            .instructions
            .iter()
            .find_map(|instruction| match &instruction.kind {
                MirInstructionKind::Const { result, .. } => Some(result.clone()),
                _ => None,
            })
            .expect("generic record Copy rvalue scalar producer");
        let MirInstructionKind::Call { arguments, .. } = &mut block.instructions[call_index].kind
        else {
            unreachable!("call index points to a non-call instruction")
        };
        arguments[0] = replacement;
        let mut functions = program.functions().clone();
        functions.insert(owner, main);
        let errors = MirProgram::with_type_catalog_and_instances(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
        )
        .expect_err("generic record Copy call without a producer must fail closed");
        assert!(
            errors.iter().any(|error| {
                error
                    .message
                    .contains("generic scalar record projection call transfer is invalid")
                    && error.message.contains("direct Record Construct result")
            }),
            "{errors:?}"
        );
    }

    #[test]
    fn concrete_owned_generic_record_projection_consumes_move_project() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_projection.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("owned generic record projection must materialize");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("owned generic record projection instance");
        let MirGenericInstanceContract::OwnedRecordProjection { contract } = &instance.contract
        else {
            panic!("owned generic record projection must carry its consuming receipt");
        };
        assert_eq!(contract.arity, 1);
        assert_eq!(contract.name, "value");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned generic record projection target");
        assert!(target.canonical_text().contains("move_project"));
        assert!(target
            .values
            .get(&target.parameters[0])
            .and_then(|value| program.type_catalog().get(&value.ty))
            .is_some_and(|descriptor| {
                descriptor.ownership == crate::core::mir::types::MirOwnership::Move
            }));
        let main = program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("owned generic projection caller");
        let (_, call_index, call_argument) = main
            .blocks
            .iter()
            .find_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(index, instruction)| match &instruction.kind {
                        MirInstructionKind::Call { arguments, .. } => arguments
                            .first()
                            .cloned()
                            .map(|argument| (block_id, index, argument)),
                        _ => None,
                    })
            })
            .expect("owned generic projection call");
        assert!(matches!(
            main.blocks
                .values()
                .find_map(|block| block.instructions.get(call_index.saturating_sub(1))),
            Some(MirInstruction {
                kind: MirInstructionKind::Move { result, .. },
                ..
            }) if result == &call_argument
        ));
        assert!(program
            .type_catalog()
            .validate_owned_string(&instance.arguments[0])
            .is_ok());
        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference owned generic record projection execution");
        assert_eq!(value, MirRuntimeValue::Int(41));
    }

    #[test]
    fn concrete_owned_mixed_generic_record_projection_consumes_move_project() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_mixed.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("mixed owned generic record projection must materialize");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("mixed owned generic record projection instance");
        let MirGenericInstanceContract::OwnedRecordProjection { contract } = &instance.contract
        else {
            panic!("mixed owned generic record projection must carry consuming receipt");
        };
        assert_eq!(contract.arity, 2);
        assert_eq!(contract.name, "value");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("mixed owned generic record projection target");
        assert!(target.canonical_text().contains("move_project"));
        let parameter_ty = target
            .values
            .get(&target.parameters[0])
            .map(|value| value.ty.clone())
            .expect("mixed owned record projection parameter TypeDesc");
        let descriptor = program
            .type_catalog()
            .get(&parameter_ty)
            .expect("mixed owned record projection TypeDesc");
        assert!(matches!(
            descriptor.layout,
            crate::core::mir::types::MirLayout::Record { ref fields, .. }
                if fields.len() == 2
                    && fields[0].ty == contract.field_ty
                    && program.type_catalog().get(&fields[1].ty).is_some_and(|sibling| {
                        sibling.ownership == crate::core::mir::types::MirOwnership::Copy
                    })
        ));
        assert!(program
            .type_catalog()
            .validate_owned_string(&instance.arguments[0])
            .is_ok());
        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference mixed owned generic projection execution");
        assert_eq!(value, MirRuntimeValue::Int(41));
    }

    #[test]
    fn owned_generic_record_projection_call_clone_cannot_cross_mir_gate() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_projection.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("owned generic record projection must materialize");
        let owner = NodeId("function:main".into());
        let mut main = program
            .functions()
            .get(&owner)
            .cloned()
            .expect("owned generic projection caller");
        let block =
            main.blocks
                .values_mut()
                .find(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(instruction.kind, MirInstructionKind::Call { .. })
                    })
                })
                .expect("owned generic projection call block");
        let call_index = block
            .instructions
            .iter()
            .position(|instruction| matches!(instruction.kind, MirInstructionKind::Call { .. }))
            .expect("owned generic projection call");
        let (result, source) = match &block.instructions[call_index.saturating_sub(1)].kind {
            MirInstructionKind::Move { result, source } => (result.clone(), source.clone()),
            other => panic!("expected materialized Move producer, got {other:?}"),
        };
        block.instructions[call_index.saturating_sub(1)].kind =
            MirInstructionKind::Clone { result, source };
        let mut functions = program.functions().clone();
        functions.insert(owner, main);
        let errors = MirProgram::with_type_catalog_and_instances(
            functions,
            program.type_catalog().clone(),
            program.instances().clone(),
        )
        .expect_err("owned generic call with a cloned source must fail closed");
        assert!(
            errors.iter().any(|error| {
                error
                    .message
                    .contains("owned generic record projection call transfer is invalid")
                    && error
                        .message
                        .contains("direct local Move or fresh Record Construct producer")
            }),
            "{errors:?}"
        );
    }

    #[test]
    fn concrete_owned_generic_record_projection_rvalue_call_moves_construct() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_record_owned_string_rvalue_call.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("owned generic record rvalue call must materialize");
        let main = program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("owned generic rvalue caller");
        let (_, call_index, call_argument) = main
            .blocks
            .iter()
            .find_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(index, instruction)| match &instruction.kind {
                        MirInstructionKind::Call { arguments, .. } => arguments
                            .first()
                            .cloned()
                            .map(|argument| (block_id, index, argument)),
                        _ => None,
                    })
            })
            .expect("owned generic rvalue call");
        assert!(matches!(
            main.blocks
                .values()
                .find_map(|block| block.instructions.get(call_index.saturating_sub(1))),
            Some(MirInstruction {
                kind: MirInstructionKind::Construct {
                    result,
                    kind: MirAggregateKind::Record { .. },
                    ..
                },
                ..
            }) if result == &call_argument
        ));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference owned generic rvalue projection execution");
        assert_eq!(value, MirRuntimeValue::Int(41));
    }

    #[test]
    fn owned_generic_record_projection_indirect_argument_fails_closed() {
        let source = "type Box<T> { value: T }\nfunc take<T>(boxed: Box<T>) -> T { boxed.value }\nfunc main() -> i32 { let picked = take(if true { Box { value: \"owned\" } } else { Box { value: \"other\" } }); drop(picked); 41 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked).expect_err(
            "owned generic record projection indirect argument must remain fail-closed",
        );
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(errors.iter().any(|error| {
                error
                    .message
                    .contains("owned generic record projection call requires a direct local Clone or fresh Record Construct producer")
            }), "{errors:?}"),
            other => panic!("owned generic record indirect argument crossed the MIR gate: {other:?}"),
        }
    }

    #[test]
    fn concrete_two_field_generic_record_projection_executes_with_copy_residual() {
        let source =
            include_str!("../../../tests/fixtures/mir_native_generic_record_projection_pair.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("two-field generic record projection must materialize");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("two-field generic record projection instance");
        let MirGenericInstanceContract::ScalarRecordProjection { contract } = &instance.contract
        else {
            panic!("two-field generic record projection must carry a record receipt");
        };
        assert_eq!(contract.arity, 2);
        assert_eq!(contract.name, "left");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("two-field generic record projection target");
        let parameter_ty = target
            .values
            .get(&target.parameters[0])
            .map(|value| value.ty.clone())
            .expect("two-field record projection parameter TypeDesc");
        let descriptor = program
            .type_catalog()
            .get(&parameter_ty)
            .expect("two-field record projection TypeDesc");
        assert!(matches!(
            descriptor.layout,
            crate::core::mir::types::MirLayout::Record { ref fields, .. }
                if fields.len() == 2 && fields.iter().all(|field| field.ty == contract.field_ty)
        ));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference two-field generic record projection execution");
        assert_eq!(value, MirRuntimeValue::Int(41));
    }

    #[test]
    fn concrete_mixed_generic_record_projection_executes_with_fixed_copy_sibling() {
        let source =
            include_str!("../../../tests/fixtures/mir_native_generic_record_projection_mixed.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("mixed generic record projection must materialize");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("mixed generic record projection instance");
        let MirGenericInstanceContract::ScalarRecordProjection { contract } = &instance.contract
        else {
            panic!("mixed generic record projection must carry a record receipt");
        };
        assert_eq!(contract.arity, 2);
        assert_eq!(contract.name, "value");
        let target = program
            .functions()
            .get(&instance.function)
            .expect("mixed generic record projection target");
        let parameter_ty = target
            .values
            .get(&target.parameters[0])
            .map(|value| value.ty.clone())
            .expect("mixed record projection parameter TypeDesc");
        let descriptor = program
            .type_catalog()
            .get(&parameter_ty)
            .expect("mixed record projection TypeDesc");
        assert!(matches!(
            descriptor.layout,
            crate::core::mir::types::MirLayout::Record { ref fields, .. }
                if fields.len() == 2
                    && fields[0].ty == contract.field_ty
                    && fields[1].ty != contract.field_ty
        ));
        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference mixed generic record projection execution");
        assert_eq!(value, MirRuntimeValue::Bool(true));
    }

    #[test]
    fn three_field_generic_record_projection_fails_closed() {
        let source = "type Triple<T> { first: T, second: T, third: T }\nfunc get<T>(triple: Triple<T>) -> T { triple.first }\nfunc main() -> i32 { let triple = Triple { first: 41, second: 7, third: 9 }; get(triple) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("three-field generic record projection must remain fail-closed");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(
                errors
                    .iter()
                    .any(|error| error.message.contains("generic record projection")),
                "unexpected errors: {errors:?}"
            ),
            other => panic!("unsupported generic record shape crossed MIR gate: {other:?}"),
        }
    }

    #[test]
    fn mixed_managed_generic_record_projection_fails_closed() {
        let source = "type Tagged<T> { value: T, tag: string }\nfunc get<T>(tagged: Tagged<T>) -> T { tagged.value }\nfunc main() -> i32 { let tagged = Tagged { value: 41, tag: \"managed\" }; let picked = get(tagged); picked }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("mixed managed generic record projection must fail closed");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(
                errors
                    .iter()
                    .any(|error| { error.message.contains("generic record projection") }),
                "unexpected errors: {errors:?}"
            ),
            other => panic!("mixed managed generic record crossed MIR gate: {other:?}"),
        }
    }

    #[test]
    fn mixed_owned_generic_record_projection_with_noncopy_sibling_fails_closed() {
        let source = "type Tagged<T> { value: T, tag: string }\nfunc get<T>(tagged: Tagged<T>) -> T { tagged.value }\nfunc main() -> i32 { let tagged = Tagged { value: \"owned\", tag: \"residual\" }; let picked = get(tagged); drop(picked); 41 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("non-Copy sibling must prevent an owned projection");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(
                errors.iter().any(|error| {
                    error.message.contains("generic record projection")
                        && error.message.contains("direct field Project")
                }),
                "unexpected errors: {errors:?}"
            ),
            other => panic!("non-Copy sibling crossed the MIR gate: {other:?}"),
        }
    }

    #[test]
    fn unsupported_owned_generic_record_projection_shape_fails_closed() {
        let source = "type Pair<T> { left: T, right: T }\nfunc get<T>(pair: Pair<T>) -> T { pair.left }\nfunc main() -> i32 { let pair = Pair { left: \"owned\", right: \"keep\" }; let picked = get(pair); drop(picked); 41 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("multi-field owned generic record projection must remain fail-closed");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(
                errors
                    .iter()
                    .any(|error| error.message.contains("generic record projection")),
                "unexpected errors: {errors:?}"
            ),
            other => panic!("unsupported owned generic record shape crossed MIR gate: {other:?}"),
        }
    }

    #[test]
    fn concrete_generic_identity_reuses_variant_call_abi_receipts() {
        let source =
            include_str!("../../../tests/fixtures/mir_native_generic_variant_identity.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("flat Copy Option/Result generic identities must materialize");
        assert_eq!(program.instances().len(), 2);
        assert!(program.instances().values().all(|instance| matches!(
            instance.contract,
            MirGenericInstanceContract::ScalarIdentity
        )));

        let main = program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("main MIR function");
        let calls = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .filter_map(|instruction| match &instruction.kind {
                MirInstructionKind::Call {
                    type_arguments,
                    variant_call_contract: Some(receipt),
                    ..
                } => Some((type_arguments, receipt)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|(type_arguments, receipt)| {
            type_arguments.len() == 1
                && receipt.type_arguments == **type_arguments
                && receipt.callee.0.starts_with("function:mir:instance:")
        }));
        assert!(calls
            .iter()
            .any(|(_, receipt)| receipt.nominal.as_str() == "builtin:type:Option"));
        assert!(calls
            .iter()
            .any(|(_, receipt)| receipt.nominal.as_str() == "builtin:type:Result"));

        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference generic variant identity execution");
        assert_eq!(value, MirRuntimeValue::Int(18));
    }

    #[test]
    fn reference_executes_materialized_generic_variant_identity_branch_paths() {
        let source = include_str!(
            "../../../tests/fixtures/mir_native_generic_variant_identity_multipath.mimi"
        );
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("generic identity branch must lower to canonical MIR");
        let identity = program
            .instances()
            .values()
            .next()
            .expect("identity instance");
        let target = program
            .functions()
            .get(&identity.function)
            .expect("materialized identity target");
        assert!(target.blocks.len() > 1, "fixture must retain branch CFG");
        assert!(target.blocks.values().any(|block| {
            matches!(
                block.terminator,
                crate::core::mir::MirTerminator::Branch { .. }
            )
        }));

        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference generic branch identity execution");
        assert_eq!(value, MirRuntimeValue::Int(7));
    }

    #[test]
    fn reference_executes_total_direct_variant_call_paths() {
        let source = include_str!("../../../tests/fixtures/mir_native_variant_call_multipath.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let interpreter = MirReferenceInterpreter::new(&program);
        let owner = NodeId("function:checked".into());
        assert_eq!(
            interpreter
                .execute(&owner, &[MirRuntimeValue::Bool(true)])
                .expect("reference true path"),
            MirRuntimeValue::Int(4)
        );
        assert_eq!(
            interpreter
                .execute(&owner, &[MirRuntimeValue::Bool(false)])
                .expect("reference false path"),
            MirRuntimeValue::Int(0)
        );
    }

    #[test]
    fn concrete_scalar_set_facade_instances_are_typed_and_executable() {
        let source = "func set_size<T>(s: Set<T>) -> i32 { s.size() }\nfunc set_contains<T>(s: Set<T>, value: T) -> bool { s.contains(value) }\nfunc set_insert<T>(s: Set<T>, value: T) -> Set<T> { s.insert(value) }\nfunc set_remove<T>(s: Set<T>, value: T) -> Set<T> { s.remove(value) }\nfunc set_to_list<T>(s: Set<T>) -> List<T> { s.to_list() }\nfunc main() -> i32 { let values: Set<i32> = {1, 2, 1}; let inserted = set_insert(values, 3); if set_size(inserted) != 3 { return 1 } if !set_contains(inserted, 2) { return 2 } let removed = set_remove(inserted, 1); let list = set_to_list(removed); if len(list) != 2 { return 3 } 0 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("scalar Set facade calls must materialize as canonical MIR");

        assert_eq!(program.instances().len(), 5);
        assert!(program.instances().values().all(|instance| matches!(
            instance.contract,
            MirGenericInstanceContract::ScalarSetFacade { .. }
        )));
        let main = program
            .functions()
            .get(&NodeId("function:main".into()))
            .expect("main MIR function");
        let instance_targets = program
            .instances()
            .values()
            .map(|instance| instance.function.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut rewritten_call_count = 0;
        for instruction in main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
        {
            if let MirInstructionKind::Call {
                callee: ResolvedCallee::Function(callee),
                type_arguments,
                ..
            } = &instruction.kind
            {
                rewritten_call_count += 1;
                assert!(instance_targets.contains(callee));
                assert_eq!(type_arguments.len(), 1);
            }
        }
        assert_eq!(rewritten_call_count, 5);

        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference Set facade execution");
        assert_eq!(value, MirRuntimeValue::Int(0));
    }

    #[test]
    fn unsupported_generic_set_body_fails_closed_before_backend() {
        let source =
            "func bad<T>(s: Set<T>) -> Set<T> { s }\nfunc main() -> i32 { let values: Set<i32> = {1, 2}; let result = bad(values); drop(result); 0 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("an unproven generic Set body must remain fail-closed");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(errors.iter().any(|error| {
                error
                    .message
                    .contains("generic Set facade must lower to exactly one canonical SetOp")
            })),
            other => panic!("unsupported generic Set shape crossed the MIR gate: {other:?}"),
        }
    }

    #[test]
    fn concrete_owned_string_generic_identity_materializes_explicit_drop_glue() {
        let source =
            include_str!("../../../tests/fixtures/mir_native_generic_owned_string_identity.mimi");
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked)
            .expect("owned String generic identity must materialize canonical MIR");
        let instance = program
            .instances()
            .values()
            .next()
            .expect("owned String identity instance");
        assert!(matches!(
            instance.contract,
            MirGenericInstanceContract::OwnedStringIdentity
        ));
        let target = program
            .functions()
            .get(&instance.function)
            .expect("owned String identity target");
        assert!(target
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.kind, MirInstructionKind::Clone { .. })));
        assert!(target
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .any(|instruction| matches!(instruction.kind, MirInstructionKind::Drop { .. })));
        let string_ty = instance.arguments.first().expect("String argument");
        assert!(program
            .type_catalog()
            .validate_owned_string(string_ty)
            .is_ok());

        let value = MirReferenceInterpreter::new(&program)
            .execute(&NodeId("function:main".into()), &[])
            .expect("reference owned String generic identity execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn unsupported_non_scalar_generic_identity_branch_fails_closed_before_backend() {
        let source =
            "func identity<T>(value: T) -> T { if true { value } else { value } }\nfunc main() -> string { identity(\"owned\") }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("non-scalar generic identity must remain outside this MIR island");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(errors.iter().any(|error| {
                error.message.contains(
                    "owned String generic identity specialization must clone its parameter",
                )
            })),
            other => panic!("unsupported generic shape crossed the MIR gate: {other:?}"),
        }
    }

    #[test]
    fn unsupported_non_copy_generic_variant_identity_fails_closed_before_backend() {
        let source = r#"
            func identity<T>(value: T) -> T { value }

            func main() -> i32 {
                let value: Option<string> = Some("owned")
                let roundtrip = identity(value)
                drop(roundtrip)
                0
            }
        "#;
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("non-Copy generic variant identity must remain outside this MIR island");
        match error {
            MirProgramBuildError::Lowering(errors) => assert!(errors.iter().any(|error| {
                error
                    .message
                    .contains("outside scalar contract or flat Copy variant contract")
            })),
            other => panic!("unsupported generic variant crossed the MIR gate: {other:?}"),
        }
    }

    #[test]
    fn canonical_program_gate_rejects_an_absent_call_target_before_backend() {
        let source =
            "func add_one(value: i32) -> i32 { value + 1 }\nfunc main() -> i32 { add_one(41) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let canonical = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut main = canonical.functions().get(&owner).cloned().expect("main");
        let call = main
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find_map(|instruction| match &mut instruction.kind {
                crate::core::mir::MirInstructionKind::Call { callee, .. } => Some(callee),
                _ => None,
            })
            .expect("call instruction");
        *call = crate::core::ir::ResolvedCallee::Function(crate::core::NodeId(
            "function:missing".into(),
        ));

        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, main)]),
            canonical.type_catalog().clone(),
        )
        .expect_err("an absent call target must fail before a backend");
        assert!(errors.iter().any(|error| {
            error
                .message
                .contains("absent from the canonical MIR program")
        }));
    }

    #[test]
    fn canonical_program_gate_rejects_call_signature_drift_before_backend() {
        let source =
            "func add_one(value: i32) -> i32 { value + 1 }\nfunc main() -> i32 { add_one(41) }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let canonical = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        let owner = crate::core::NodeId("function:main".into());
        let mut main = canonical.functions().get(&owner).cloned().expect("main");
        let bool_ty = canonical
            .type_catalog()
            .iter()
            .find_map(|(id, descriptor)| {
                (descriptor.abi == crate::core::mir::types::MirAbiClass::Bool).then(|| id.clone())
            })
            .expect("bool TypeDesc");
        let argument = main
            .blocks
            .values()
            .flat_map(|block| block.instructions.iter())
            .find_map(|instruction| match &instruction.kind {
                crate::core::mir::MirInstructionKind::Call { arguments, .. } => {
                    arguments.first().cloned()
                }
                _ => None,
            })
            .expect("call argument");
        main.values.get_mut(&argument).expect("argument value").ty = bool_ty;

        let mut functions = canonical.functions().clone();
        functions.insert(owner, main);
        let errors = MirProgram::with_type_catalog(functions, canonical.type_catalog().clone())
            .expect_err("call signature drift must fail before a backend");
        assert!(
            errors.iter().any(|error| {
                error.message.contains("call argument 0 type")
                    && error.message.contains("disagrees with callee")
            }),
            "{errors:?}"
        );
    }

    #[test]
    fn executes_record_rvalue_projection_from_canonical_layout() {
        let source =
            "type Point { x: i32, y: bool }\nfunc main() -> i32 { Point { y: true, x: 40 }.x }";
        let (owner, program) = canonical_program_with_main(source);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(40));
    }

    #[test]
    fn executes_record_update_and_place_projection_from_canonical_layout() {
        let source = "type Point { x: i32, y: bool }\nfunc main() -> i32 { let p = Point { x: 40, y: true }; let q = Point { y: false, ..p }; q.x }";
        let (owner, program) = canonical_program_with_main(source);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(40));
    }

    #[test]
    fn executes_copy_option_and_result_variants_from_canonical_mir() {
        for (source, expected) in [
            (
                "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v, None => 0 } }",
                41,
            ),
            (
                "func main() -> i32 { let value: Result<i32, i32> = Err(7); match value { Ok(v) => v, Err(e) => e } }",
                7,
            ),
            (
                "func main() -> i32 { let value: Option<i32> = None; match value { Some(v) => v, None => 0 } }",
                0,
            ),
            (
                "func main() -> i32 { let value: Result<i32, i32> = Ok(41); match value { Ok(v) => v, Err(e) => e } }",
                41,
            ),
        ] {
            let (owner, program) = canonical_program_with_main(source);
            let value = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference execution");
            assert_eq!(value, MirRuntimeValue::Int(expected));
        }
    }

    #[test]
    fn executes_i64_copy_variant_branch_merge_from_canonical_mir() {
        let source = "func choose(flag: bool) -> Option<i64> { if flag { Some(41) } else { None } }\nfunc main() -> i64 { let value = choose(true); match value { Some(v) => v + (1 as i64), None => (0 as i64) } }";
        let (owner, program) = canonical_program_with_main(source);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn executes_move_option_and_result_payloads_from_canonical_mir() {
        for (source, expected) in [
            (
                "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }",
                "owned",
            ),
            (
                "func main() -> string { let value: Result<string, string> = Err(\"error\"); match value { Ok(v) => v, Err(e) => e } }",
                "error",
            ),
        ] {
            let (owner, program) = canonical_program_with_main(source);
            let value = MirReferenceInterpreter::new(&program)
                .execute(&owner, &[])
                .expect("reference execution");
            assert_eq!(value, MirRuntimeValue::String(expected.into()));
        }
    }

    #[test]
    fn consuming_switch_drops_unbound_variant_payload() {
        let source =
            "func main() -> i32 { let value: Option<string> = Some(\"owned\"); match value { Some(_) => 42, None => 0 } }";
        let (owner, program) = canonical_program_with_main(source);
        let value = MirReferenceInterpreter::new(&program)
            .execute(&owner, &[])
            .expect("reference execution");
        assert_eq!(value, MirRuntimeValue::Int(42));
    }

    #[test]
    fn canonical_program_gate_rejects_duplicate_variant_switch_case() {
        let source =
            "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v, None => 0 } }";
        let (_, program) = canonical_program_with_main(source);
        let owner = crate::core::NodeId("function:main".into());
        let mut function = program.functions().get(&owner).cloned().expect("main");
        function
            .blocks
            .values_mut()
            .find_map(|block| match &mut block.terminator {
                crate::core::mir::MirTerminator::Switch { arms, .. } => Some(arms),
                _ => None,
            })
            .expect("variant switch")
            .get_mut(1)
            .expect("second variant arm")
            .case = crate::core::mir::MirSwitchCase::Variant(crate::core::NodeId(
            "builtin:variant:Option::Some".into(),
        ));
        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            program.type_catalog().clone(),
        )
        .expect_err("duplicate variant MIR must fail before execution");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("repeated")),
            "{errors:?}"
        );
    }

    #[test]
    fn canonical_program_gate_rejects_unknown_consuming_switch_binding() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let (_, program) = canonical_program_with_main(source);
        let owner = crate::core::NodeId("function:main".into());
        let mut function = program.functions().get(&owner).cloned().expect("main");
        function
            .blocks
            .values_mut()
            .find_map(|block| match &mut block.terminator {
                crate::core::mir::MirTerminator::SwitchMove { arms, .. } => Some(arms),
                _ => None,
            })
            .expect("consuming variant switch")
            .first_mut()
            .expect("Some arm")
            .bindings
            .first_mut()
            .expect("payload binding")
            .projection
            .field = crate::core::NodeId("builtin:variant:Option::Some/missing".into());
        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            program.type_catalog().clone(),
        )
        .expect_err("unknown consuming binding must fail before execution");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("absent from variant")),
            "{errors:?}"
        );
    }

    #[test]
    fn canonical_program_gate_rejects_stale_variant_projection_receipt() {
        let source =
            "func main() -> i32 { let value: Option<i32> = Some(41); match value { Some(v) => v, None => 0 } }";
        let (_, program) = canonical_program_with_main(source);
        let owner = crate::core::NodeId("function:main".into());
        let mut function = program.functions().get(&owner).cloned().expect("main");
        let binding = function
            .blocks
            .values_mut()
            .find_map(|block| match &mut block.terminator {
                crate::core::mir::MirTerminator::Switch { arms, .. } => {
                    arms.first_mut().and_then(|arm| arm.bindings.first_mut())
                }
                _ => None,
            })
            .expect("Some payload binding");
        binding.projection.nominal =
            crate::core::ir::NominalTypeId::new("builtin:type:Result").expect("Result nominal");
        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            program.type_catalog().clone(),
        )
        .expect_err("stale projection receipt must fail before execution");
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("disagrees with TypeDesc")),
            "{errors:?}"
        );
    }

    #[test]
    fn canonical_program_gate_rejects_copy_only_switch_for_non_copy_variant() {
        let source =
            "func main() -> string { let value: Option<string> = Some(\"owned\"); match value { Some(v) => v, None => \"fallback\" } }";
        let (_, program) = canonical_program_with_main(source);
        let owner = crate::core::NodeId("function:main".into());
        let mut function = program.functions().get(&owner).cloned().expect("main");
        let mut replaced = false;
        for block in function.blocks.values_mut() {
            let replacement = match &block.terminator {
                crate::core::mir::MirTerminator::SwitchMove { scrutinee, arms } => {
                    Some((scrutinee.clone(), arms.clone()))
                }
                _ => None,
            };
            if let Some((scrutinee, arms)) = replacement {
                block.terminator = crate::core::mir::MirTerminator::Switch { scrutinee, arms };
                replaced = true;
                break;
            }
        }
        assert!(replaced, "consuming variant switch");
        let errors = MirProgram::with_type_catalog(
            BTreeMap::from([(owner, function)]),
            program.type_catalog().clone(),
        )
        .expect_err("non-Copy variant must not use read-only Switch");
        assert!(
            errors.iter().any(|error| {
                error.message.contains("non-Copy") || error.message.contains("aggregate match glue")
            }),
            "{errors:?}"
        );
    }
}
