//! Small, deterministic reference executor for canonical MIR.
//!
//! This is not the production VM. It is intentionally boring and independent
//! of LLVM/runtime code so that bytecode and native lowering can be compared
//! against a third semantic oracle. The supported operation set grows with
//! MIR lowering; unsupported operations fail explicitly.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::core::ir::{ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp};
use crate::core::{NodeId, ResolvedPlace};

use super::types::{MirLayout, MirTypeCatalog};
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
        let functions =
            super::lower::lower_program(program).map_err(MirProgramBuildError::Lowering)?;
        let type_catalog =
            MirTypeCatalog::from_checked_program(program).map_err(MirProgramBuildError::Types)?;
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
        let mut functions = BTreeMap::new();
        let mut lowering_errors = Vec::new();
        for (owner, callable) in program.callables() {
            if excluded_sources.contains(&callable.body.root.origin.user_span().source_id) {
                continue;
            }
            match super::lower::lower_callable(callable) {
                Ok(function) => {
                    functions.insert(owner.clone(), function);
                }
                Err(mut errors) => lowering_errors.append(&mut errors),
            }
        }
        if !lowering_errors.is_empty() {
            return Err(MirProgramBuildError::Lowering(lowering_errors));
        }
        let type_catalog =
            MirTypeCatalog::from_checked_program(program).map_err(MirProgramBuildError::Types)?;
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
            if type_catalog.get(&function.result).is_none() {
                errors.push(super::MirValidationError {
                    subject: function.owner.0.clone(),
                    message: "function result type is absent from MIR type catalog".into(),
                });
            }
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
                        _ => {}
                    }
                }
            }
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
                    incoming = self.read_values(function, &values, arguments)?;
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
                    incoming = self.read_values(function, &values, arguments)?;
                    current = target.clone();
                }
                MirTerminator::Switch { scrutinee, arms } => {
                    let scrutinee = self.read_value(function, &values, scrutinee)?;
                    let arm = self.select_switch_arm(function, &scrutinee, arms)?;
                    incoming = self.read_values(function, &values, &arm.arguments)?;
                    current = arm.target.clone();
                }
                MirTerminator::Return { value } => {
                    return value
                        .as_ref()
                        .map(|value| self.read_value(function, &values, value))
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
            | MirInstructionKind::Clone { result, source }
            | MirInstructionKind::Convert { result, source } => {
                let value = self.read_value(function, values, source)?;
                values.insert(result.clone(), value);
            }
            MirInstructionKind::Move { result, source } => {
                let value = values.remove(source).ok_or_else(|| {
                    self.error(
                        &function.owner,
                        format!("move source '{}' is unavailable", source),
                    )
                })?;
                values.insert(result.clone(), value);
            }
            MirInstructionKind::Drop { value } => {
                if values.remove(value).is_none() {
                    return Err(self.error(
                        &function.owner,
                        format!("drop value '{}' is unavailable", value),
                    ));
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
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let fields = self.read_values(function, values, fields)?;
                let value = match kind {
                    MirAggregateKind::Tuple => MirRuntimeValue::Tuple(fields),
                    MirAggregateKind::Record {
                        nominal,
                        fields: field_ids,
                    } => self.construct_record(function, result, nominal, field_ids, fields)?,
                };
                values.insert(result.clone(), value);
            }
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind: MirAggregateKind::Record { nominal, fields },
                fields: update_values,
            } => {
                let base_value = self.read_value(function, values, base)?;
                let update_values = self.read_values(function, values, update_values)?;
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
            MirInstructionKind::Call {
                result,
                callee,
                arguments,
            } => {
                let arguments = self.read_values(function, values, arguments)?;
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

    fn read_values(
        &self,
        function: &MirFunction,
        values: &HashMap<MirValueId, MirRuntimeValue>,
        ids: &[MirValueId],
    ) -> Result<Vec<MirRuntimeValue>, MirExecutionError> {
        ids.iter()
            .map(|id| self.read_value(function, values, id))
            .collect()
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
                MirSwitchCase::Default => default = Some(arm),
                MirSwitchCase::Variant(_) => {}
                MirSwitchCase::Literal(_) => {}
            }
        }
        default.ok_or_else(|| self.error(&function.owner, "switch has no matching arm"))
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
    use super::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
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
    fn checked_program_constructor_closes_type_catalog_before_execution() {
        let source = "func main() -> i32 { 42 }";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let program = MirProgram::from_checked_program(&checked).expect("canonical MIR");
        assert!(!program.type_catalog().is_empty());
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
}
