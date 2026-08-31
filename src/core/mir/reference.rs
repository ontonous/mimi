//! Small, deterministic reference executor for canonical MIR.
//!
//! This is not the production VM. It is intentionally boring and independent
//! of LLVM/runtime code so that bytecode and native lowering can be compared
//! against a third semantic oracle. The supported operation set grows with
//! MIR lowering; unsupported operations fail explicitly.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::core::ir::{ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp};
use crate::core::{NodeId, ResolvedPlace};

use super::types::{MirGlueOperation, MirLayout, MirTypeCatalog};
use super::{
    MirAggregateKind, MirFunction, MirInstruction, MirInstructionKind, MirProjection, MirSwitchArm,
    MirSwitchCase, MirTerminator, MirValueId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirRuntimeValue {
    Int(i64),
    FloatBits(u64),
    Bool(bool),
    String(String),
    Tuple(Vec<MirRuntimeValue>),
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
        let functions = super::lower::lower_program_with_type_catalog(program, &type_catalog)
            .map_err(MirProgramBuildError::Lowering)?;
        Self::with_type_catalog(functions, type_catalog).map_err(MirProgramBuildError::Validation)
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
        Self::with_type_catalog(functions, type_catalog).map_err(MirProgramBuildError::Validation)
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
            })
        } else {
            Err(errors)
        }
    }

    pub fn with_type_catalog(
        functions: BTreeMap<NodeId, MirFunction>,
        type_catalog: MirTypeCatalog,
    ) -> Result<Self, Vec<super::MirValidationError>> {
        let mut errors = Vec::new();
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
                if descriptor.ownership == super::types::MirOwnership::Copy {
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
            errors.extend(validate_linear_consumption(function, &type_catalog));
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
                        } => {
                            let Some(base_value) = function.values.get(base) else {
                                continue;
                            };
                            let Some(result_value) = function.values.get(result) else {
                                continue;
                            };
                            if let Err(message) = type_catalog.validate_projection(
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
                        super::MirInstructionKind::Const { .. }
                        | super::MirInstructionKind::Call { .. }
                        | super::MirInstructionKind::BuiltinCall { .. }
                        | super::MirInstructionKind::Borrow { .. }
                        | super::MirInstructionKind::EndBorrow { .. }
                        | super::MirInstructionKind::Binary { .. }
                        | super::MirInstructionKind::Unary { .. }
                        | super::MirInstructionKind::Convert { .. }
                        | super::MirInstructionKind::Nop => {}
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
                        type_catalog.validate_switch_move(&scrutinee_value.ty, arms)
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
                        let variant = match &arm.case {
                            super::MirSwitchCase::Variant(variant) => {
                                type_catalog.variant(&scrutinee_value.ty, variant)
                            }
                            _ => None,
                        };
                        if variant.is_none() && !arm.bindings.is_empty() {
                            errors.push(super::MirValidationError {
                                subject: arm.edge.to_string(),
                                message: "switch payload bindings require a canonical variant case"
                                    .into(),
                            });
                            continue;
                        }
                        if let Some(variant) = variant {
                            for (index, binding) in arm.bindings.iter().enumerate() {
                                let Some(parameter) = target
                                    .parameters
                                    .get(arm.arguments.len() + index)
                                    .and_then(|parameter| function.values.get(&parameter.value))
                                else {
                                    continue;
                                };
                                let Some(field) = variant
                                    .fields
                                    .iter()
                                    .find(|field| field.id == binding.field)
                                else {
                                    errors.push(super::MirValidationError {
                                        subject: arm.edge.to_string(),
                                        message: format!(
                                            "switch binding field '{}' is absent from variant TypeDesc",
                                            binding.field.0
                                        ),
                                    });
                                    continue;
                                };
                                if parameter.ty != field.ty {
                                    errors.push(super::MirValidationError {
                                        subject: arm.edge.to_string(),
                                        message: format!(
                                            "switch binding '{}' type '{}' disagrees with payload type '{}'",
                                            binding.parameter,
                                            parameter.ty.as_str(),
                                            field.ty.as_str()
                                        ),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        if errors.is_empty() {
            errors.extend(validate_call_graph(&functions));
        }
        if errors.is_empty() {
            Ok(Self {
                functions,
                type_catalog,
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
        }
    }
    errors
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
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    for function in functions.values() {
        for block in function.blocks.values() {
            for instruction in &block.instructions {
                let super::MirInstructionKind::Call {
                    result,
                    callee,
                    arguments,
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

/// Validate explicit ownership boundaries before any execution backend sees
/// MIR. This pass is intentionally conservative: non-Copy values can only be
/// consumed once along a block-local path, and each mutually exclusive CFG
/// edge is checked from the same pre-terminator state. Aggregate destructuring
/// and partial moves remain fail-closed until their own field-level contract
/// is materialized.
fn validate_linear_consumption(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
) -> Vec<super::MirValidationError> {
    let mut errors = Vec::new();
    for block in function.blocks.values() {
        let mut consumed = BTreeSet::new();
        for instruction in &block.instructions {
            let sources: Vec<&MirValueId> = match &instruction.kind {
                super::MirInstructionKind::Move { source, .. }
                | super::MirInstructionKind::Drop { value: source } => vec![source],
                super::MirInstructionKind::MoveProject { base, .. } => vec![base],
                super::MirInstructionKind::Call { arguments, .. }
                | super::MirInstructionKind::BuiltinCall { arguments, .. }
                | super::MirInstructionKind::Construct {
                    fields: arguments, ..
                } => arguments.iter().collect(),
                super::MirInstructionKind::ConstructVariant { fields, .. }
                | super::MirInstructionKind::ConstructVariantMove { fields, .. } => {
                    fields.iter().map(|(_, value)| value).collect()
                }
                super::MirInstructionKind::UpdateRecord {
                    base,
                    fields: arguments,
                    ..
                } => {
                    let mut sources = Vec::with_capacity(arguments.len() + 1);
                    sources.push(base);
                    sources.extend(arguments.iter());
                    sources
                }
                _ => Vec::new(),
            };
            consume_values(
                function,
                type_catalog,
                &mut consumed,
                &sources,
                instruction.id.to_string(),
                &mut errors,
            );
        }
        match &block.terminator {
            super::MirTerminator::Goto {
                arguments, edge, ..
            } => consume_edge_values(
                function,
                type_catalog,
                &consumed,
                arguments,
                edge.to_string(),
                &mut errors,
            ),
            super::MirTerminator::Branch {
                then_arguments,
                then_edge,
                else_arguments,
                else_edge,
                ..
            } => {
                consume_edge_values(
                    function,
                    type_catalog,
                    &consumed,
                    then_arguments,
                    then_edge.to_string(),
                    &mut errors,
                );
                consume_edge_values(
                    function,
                    type_catalog,
                    &consumed,
                    else_arguments,
                    else_edge.to_string(),
                    &mut errors,
                );
            }
            super::MirTerminator::Switch {
                scrutinee, arms, ..
            } => {
                if is_non_copy(function, type_catalog, scrutinee) {
                    errors.push(super::MirValidationError {
                        subject: block.id.to_string(),
                        message: format!(
                            "switch scrutinee '{}' is non-Copy but aggregate match glue is not materialized",
                            scrutinee
                        ),
                    });
                }
                for arm in arms {
                    consume_edge_values(
                        function,
                        type_catalog,
                        &consumed,
                        &arm.arguments,
                        arm.edge.to_string(),
                        &mut errors,
                    );
                }
            }
            super::MirTerminator::SwitchMove {
                scrutinee, arms, ..
            } => {
                consume_values(
                    function,
                    type_catalog,
                    &mut consumed,
                    &[scrutinee],
                    block.id.to_string(),
                    &mut errors,
                );
                for arm in arms {
                    consume_edge_values(
                        function,
                        type_catalog,
                        &consumed,
                        &arm.arguments,
                        arm.edge.to_string(),
                        &mut errors,
                    );
                }
            }
            super::MirTerminator::Return { value: Some(value) } => consume_values(
                function,
                type_catalog,
                &mut consumed,
                &[value],
                block.id.to_string(),
                &mut errors,
            ),
            super::MirTerminator::Return { value: None }
            | super::MirTerminator::Trap { .. }
            | super::MirTerminator::Fault { .. }
            | super::MirTerminator::Unreachable => {}
        }
    }
    errors
}

fn consume_edge_values(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    before_edge: &BTreeSet<MirValueId>,
    values: &[MirValueId],
    subject: String,
    errors: &mut Vec<super::MirValidationError>,
) {
    let mut consumed = before_edge.clone();
    let sources = values.iter().collect::<Vec<_>>();
    consume_values(
        function,
        type_catalog,
        &mut consumed,
        &sources,
        subject,
        errors,
    );
}

fn consume_values(
    function: &MirFunction,
    type_catalog: &MirTypeCatalog,
    consumed: &mut BTreeSet<MirValueId>,
    values: &[&MirValueId],
    subject: String,
    errors: &mut Vec<super::MirValidationError>,
) {
    for value in values {
        if !is_non_copy(function, type_catalog, value) {
            continue;
        }
        if !consumed.insert((*value).clone()) {
            errors.push(super::MirValidationError {
                subject: subject.clone(),
                message: format!("use after consuming non-Copy value '{}'", value),
            });
        }
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
}

impl<'a> MirReferenceInterpreter<'a> {
    pub fn new(program: &'a MirProgram) -> Self {
        Self {
            program,
            max_steps: 1_000_000,
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
        let function = self
            .program
            .functions
            .get(owner)
            .ok_or_else(|| self.error(owner, "function is absent from MIR program"))?;
        let mut steps = 0;
        self.execute_function(function, arguments, &mut steps)
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
            } => {
                let value = self.read_value(function, values, base)?;
                let base_ty = function.values.get(base).map(|value| &value.ty);
                let projected = project_value(
                    &function.owner,
                    value,
                    base_ty,
                    projection,
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
                };
                values.insert(result.clone(), output);
            }
            MirInstructionKind::Call {
                result,
                callee,
                arguments,
            } => {
                let arguments = self.take_transfer_values(function, values, arguments)?;
                let ResolvedCallee::Function(owner) = callee else {
                    return Err(self.error(
                        &function.owner,
                        format!("callee '{callee:?}' is not a MIR function"),
                    ));
                };
                let callee = self.program.functions.get(owner).ok_or_else(|| {
                    self.error(&function.owner, format!("callee '{}' is absent", owner.0))
                })?;
                let output = self.execute_function(callee, &arguments, steps)?;
                if let Some(result) = result {
                    values.insert(result.clone(), output);
                }
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
                &mir_projection,
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
            MirLayout::Handle if matches!(&value, MirRuntimeValue::String(_)) => Ok(()),
            MirLayout::Opaque => Err(self.error(
                &function.owner,
                "non-Copy opaque value has no canonical drop implementation",
            )),
            MirLayout::Option { .. } | MirLayout::Result { .. } => {
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
                let Some((expected_nominal, expected_variants)) =
                    self.program.type_catalog().variant_layout(ty)
                else {
                    return Err(self.error(
                        &function.owner,
                        "variant drop value has no canonical TypeDesc layout",
                    ));
                };
                if nominal.as_str() != expected_nominal {
                    return Err(self.error(
                        &function.owner,
                        "variant drop nominal disagrees with TypeDesc",
                    ));
                }
                let Some(expected_variant) = expected_variants
                    .iter()
                    .find(|candidate| candidate.id == variant)
                else {
                    return Err(self.error(
                        &function.owner,
                        "variant drop discriminant is absent from TypeDesc",
                    ));
                };
                let descriptor = self
                    .program
                    .type_catalog()
                    .get(ty)
                    .ok_or_else(|| self.error(&function.owner, "drop value has no TypeDesc"))?;
                let Some(plan) = descriptor
                    .variant_drop_plan
                    .as_ref()
                    .and_then(|plans| plans.iter().find(|plan| plan.variant == variant))
                else {
                    return Err(self.error(
                        &function.owner,
                        "variant drop value has no variant drop plan",
                    ));
                };
                if payload.len() != expected_variant.fields.len()
                    || plan.fields.len() != expected_variant.fields.len()
                {
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
        let variant = self
            .program
            .type_catalog()
            .variant(scrutinee_ty, actual_variant)
            .ok_or_else(|| {
                self.error(&function.owner, "runtime variant is absent from TypeDesc")
            })?;
        for binding in &arm.bindings {
            let field_index = variant
                .fields
                .iter()
                .position(|field| field.id == binding.field)
                .ok_or_else(|| self.error(&function.owner, "switch binding field is absent"))?;
            let field = payload.get(field_index).cloned().ok_or_else(|| {
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
        let Some((expected_nominal, _)) = self.program.type_catalog().variant_layout(&scrutinee_ty)
        else {
            return Err(self.error(
                &function.owner,
                "switch-move scrutinee has no canonical variant layout",
            ));
        };
        if actual_nominal.as_str() != expected_nominal {
            return Err(self.error(
                &function.owner,
                "switch-move variant nominal disagrees with TypeDesc",
            ));
        }
        let variant = self
            .program
            .type_catalog()
            .variant(&scrutinee_ty, &actual_variant)
            .cloned()
            .ok_or_else(|| {
                self.error(
                    &function.owner,
                    "switch-move variant is absent from TypeDesc",
                )
            })?;
        if payload.len() != variant.fields.len() {
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
        let descriptor = self
            .program
            .type_catalog()
            .get(&scrutinee_ty)
            .ok_or_else(|| self.error(&function.owner, "switch-move has no TypeDesc"))?;
        let plan = descriptor
            .variant_drop_plan
            .as_ref()
            .and_then(|plans| plans.iter().find(|plan| plan.variant == actual_variant))
            .cloned()
            .ok_or_else(|| self.error(&function.owner, "switch-move variant has no drop plan"))?;
        let mut bound_indices = BTreeMap::new();
        for binding in &arm.bindings {
            let index = variant
                .fields
                .iter()
                .position(|field| field.id == binding.field)
                .ok_or_else(|| {
                    self.error(
                        &function.owner,
                        "switch-move binding field is absent from TypeDesc",
                    )
                })?;
            if bound_indices.insert(binding.field.clone(), index).is_some() {
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
            let index = *bound_indices.get(&binding.field).ok_or_else(|| {
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

fn project_value(
    function: &NodeId,
    value: MirRuntimeValue,
    base_ty: Option<&crate::core::ResolvedTypeId>,
    projection: &MirProjection,
    type_catalog: &MirTypeCatalog,
) -> Result<MirRuntimeValue, MirExecutionError> {
    match (value, projection) {
        (MirRuntimeValue::Tuple(values), MirProjection::Tuple(index)) => values
            .get(*index)
            .cloned()
            .ok_or_else(|| execution_error(function, "tuple projection is out of bounds")),
        (MirRuntimeValue::Record { nominal, fields }, MirProjection::Field(field)) => {
            let Some(base_ty) = base_ty else {
                return Err(execution_error(
                    function,
                    "record projection has no base type",
                ));
            };
            let Some(descriptor) = type_catalog.get(base_ty) else {
                return Err(execution_error(
                    function,
                    "record projection base has no TypeDesc",
                ));
            };
            let MirLayout::Record {
                nominal: expected_nominal,
                fields: layout_fields,
            } = &descriptor.layout
            else {
                return Err(execution_error(
                    function,
                    "record projection base has no record layout",
                ));
            };
            if &nominal != expected_nominal {
                return Err(execution_error(
                    function,
                    "record runtime nominal disagrees with TypeDesc",
                ));
            }
            let Some(index) = layout_fields
                .iter()
                .position(|candidate| candidate.id == *field)
            else {
                return Err(execution_error(
                    function,
                    "record projection field is absent from TypeDesc",
                ));
            };
            fields
                .get(index)
                .cloned()
                .ok_or_else(|| execution_error(function, "record field vector is too short"))
        }
        (value, MirProjection::Dereference) => Ok(value),
        (_, MirProjection::Index(_)) => Err(execution_error(
            function,
            "indexed projection is not implemented by the reference slice",
        )),
        _ => Err(execution_error(
            function,
            "projection does not match aggregate value",
        )),
    }
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
    let MirLayout::Record {
        nominal: expected_nominal,
        fields: layout_fields,
    } = &type_catalog
        .get(base_ty)
        .ok_or_else(|| execution_error(function, "move projection base has no TypeDesc"))?
        .layout
    else {
        return Err(execution_error(
            function,
            "move projection base has no record layout",
        ));
    };
    if &nominal != expected_nominal || fields.len() != layout_fields.len() {
        return Err(execution_error(
            function,
            "move projection record disagrees with TypeDesc",
        ));
    }
    let MirProjection::Field(field) = projection else {
        return Err(execution_error(
            function,
            "move projection requires a direct record field",
        ));
    };
    let selected = layout_fields
        .iter()
        .position(|candidate| candidate.id == *field)
        .ok_or_else(|| execution_error(function, "move projection field is absent"))?;
    if layout_fields[selected].ty != *result_ty {
        return Err(execution_error(
            function,
            "move projection result type disagrees with TypeDesc",
        ));
    }
    Ok(std::mem::replace(
        fields
            .get_mut(selected)
            .ok_or_else(|| execution_error(function, "move projection field is out of bounds"))?,
        MirRuntimeValue::Unit,
    ))
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
