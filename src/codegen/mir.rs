//! AST-free native consumer for the closed scalar/flat-record/flat-variant and
//! scalar-List
//! Canonical MIR slice.
//!
//! This module intentionally accepts only `MirProgram`.  It does not import
//! surface AST or `CheckedProgram`, and it never calls the legacy emitter.  A
//! small eligibility validator runs before LLVM declarations are created so a
//! shape is either covered by the scalar MIR contract or rejected explicitly.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use inkwell::basic_block::BasicBlock;
use inkwell::context::Context;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, PhiValue,
};
use inkwell::IntPredicate;

use crate::codegen::{call_try_basic_value, CodeGenerator};
use crate::core::ir::{ResolvedBinaryOp, ResolvedCallee, ResolvedLiteral, ResolvedUnaryOp};
use crate::core::mir::reference::MirProgram;
use crate::core::mir::types::{
    MirAbiClass, MirBuiltinContract, MirBuiltinKind, MirConversionKind, MirGlueContract,
    MirGlueKind, MirLayout, MirOwnership, MirTypeCatalog, MirTypeDesc,
};
use crate::core::mir::{
    MirAggregateKind, MirBlockId, MirFunction, MirInstructionKind, MirProjection, MirSwitchArm,
    MirSwitchCase, MirTerminator, MirValueId,
};
use crate::diagnostic::Diagnostic;
use crate::span::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeMirError {
    subject: String,
    message: String,
}

impl NativeMirError {
    fn new(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            message: message.into(),
        }
    }

    fn diagnostic(self) -> Diagnostic {
        Diagnostic::error(
            format!(
                "canonical MIR native backend rejected {}: {}",
                self.subject, self.message
            ),
            Span::UNKNOWN,
        )
    }
}

/// Compile a validated scalar/flat-aggregate MIR program directly to LLVM.
///
/// This is an explicit migration entry point. It is not used by the default
/// `build` path until the wider MIR shape and differential gates are closed.
impl<'ctx> CodeGenerator<'ctx> {
    pub fn compile_mir_native(&mut self, program: &MirProgram) -> Result<(), Vec<Diagnostic>> {
        if let Err(errors) = NativeMirValidator::new(program).validate() {
            return Err(errors.into_iter().map(NativeMirError::diagnostic).collect());
        }

        NativeMirEmitter::new(self, program)
            .compile()
            .map_err(|error| vec![error.diagnostic()])
    }
}

struct NativeMirValidator<'a> {
    program: &'a MirProgram,
    errors: Vec<NativeMirError>,
    symbols: BTreeSet<String>,
}

impl<'a> NativeMirValidator<'a> {
    fn new(program: &'a MirProgram) -> Self {
        Self {
            program,
            errors: Vec::new(),
            symbols: BTreeSet::new(),
        }
    }

    fn validate(mut self) -> Result<(), Vec<NativeMirError>> {
        for function in self.program.functions().values() {
            let symbol = match mir_symbol(&function.owner) {
                Ok(symbol) => symbol,
                Err(message) => {
                    self.errors
                        .push(NativeMirError::new(function.owner.0.clone(), message));
                    continue;
                }
            };
            if !self.symbols.insert(symbol.to_owned()) {
                self.errors.push(NativeMirError::new(
                    function.owner.0.clone(),
                    format!("native symbol '{symbol}' is duplicated"),
                ));
            }
            self.validate_function(function);
        }
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors)
        }
    }

    fn validate_function(&mut self, function: &MirFunction) {
        let catalog = self.program.type_catalog();
        for parameter in &function.parameters {
            self.validate_value(function, parameter, "parameter");
        }
        self.validate_signature_type(&function.result, "result", true);

        for block in function.blocks.values() {
            for parameter in &block.parameters {
                self.validate_value(function, &parameter.value, "block parameter");
            }
            for instruction in &block.instructions {
                self.validate_instruction(
                    function,
                    instruction_kind(instruction),
                    instruction.id.as_str(),
                );
            }
            self.validate_terminator(function, &block.terminator, &block.id);
        }

        for event in &function.ownership.events {
            if matches!(
                event.kind,
                crate::core::mir::MirOwnershipEventKind::TransferSession
                    | crate::core::mir::MirOwnershipEventKind::TransferChild
                    | crate::core::mir::MirOwnershipEventKind::BorrowShared
                    | crate::core::mir::MirOwnershipEventKind::BorrowMut
                    | crate::core::mir::MirOwnershipEventKind::BorrowEnd
            ) {
                self.errors.push(NativeMirError::new(
                    function.owner.0.clone(),
                    format!(
                        "ownership effect '{}' has no scalar native MIR contract",
                        event.kind.as_str()
                    ),
                ));
            }
            if let Some(value) = &event.value {
                self.validate_value(function, value, "ownership event");
            }
        }

        // Keep this local use to make the validator's TypeDesc dependency
        // explicit: the native backend never reconstructs ownership from an
        // LLVM type.  The detailed operation checks below use the same catalog.
        let _ = catalog;
    }

    fn validate_value(&mut self, function: &MirFunction, value: &MirValueId, subject: &str) {
        let Some(info) = function.values.get(value) else {
            self.errors.push(NativeMirError::new(
                value.to_string(),
                format!("{subject} is absent from MIR value catalog"),
            ));
            return;
        };
        self.validate_signature_type(&info.ty, &format!("{subject} '{value}'"), false);
    }

    fn validate_signature_type(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        allow_unit_result: bool,
    ) {
        let Some(desc) = self.program.type_catalog().get(ty) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()),
            ));
            return;
        };
        let is_list = matches!(desc.layout, MirLayout::List { .. });
        let supported = if is_list {
            match self
                .program
                .type_catalog()
                .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)
            {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else {
            (matches!(
                desc.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true,
                } | MirAbiClass::Bool
            ) && desc.layout == MirLayout::Scalar)
                || (allow_unit_result
                    && desc.abi == MirAbiClass::Unit
                    && desc.layout == MirLayout::Unit)
                || self.validate_flat_copy_record(ty, subject, desc)
                || self.validate_flat_copy_variant(ty, subject, desc)
        };
        if !supported {
            let contract = if is_list {
                "native canonical List contract"
            } else {
                "Copy scalar native contract"
            };
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "TypeDesc ABI {:?}, layout {:?}, ownership {:?} is outside the {contract}",
                    desc.abi, desc.layout, desc.ownership,
                ),
            ));
        }
        if !is_list {
            if desc.ownership != MirOwnership::Copy {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!(
                        "ownership {:?} requires explicit native glue and is not in this scalar slice",
                        desc.ownership
                    ),
                ));
            }
            if desc.glue
                != (crate::core::mir::types::MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                })
            {
                self.errors.push(NativeMirError::new(
                    subject,
                    "Copy scalar TypeDesc does not carry the canonical no-op glue contract",
                ));
            }
        }
    }

    fn validate_flat_copy_record(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        desc: &MirTypeDesc,
    ) -> bool {
        let MirLayout::Record { fields, .. } = &desc.layout else {
            return false;
        };
        if desc.abi != MirAbiClass::Aggregate {
            return false;
        }
        if fields.is_empty() {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "record TypeDesc '{}' has no fields in the native ABI",
                    ty.as_str()
                ),
            ));
            return false;
        }
        let mut valid = true;
        let mut field_ids = BTreeSet::new();
        for field in fields {
            if !field_ids.insert(&field.id) {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("record field identity '{}' is duplicated", field.id.0),
                ));
                valid = false;
            }
            let Some(field_desc) = self.program.type_catalog().get(&field.ty) else {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("record field '{}' TypeDesc is absent", field.name),
                ));
                valid = false;
                continue;
            };
            if !is_native_scalar_descriptor(field_desc) {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!(
                        "record field '{}' ABI {:?}/layout {:?} is outside the flat Copy record contract",
                        field.name, field_desc.abi, field_desc.layout
                    ),
                ));
                valid = false;
            }
        }
        valid
    }

    fn validate_flat_copy_variant(
        &mut self,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
        _desc: &MirTypeDesc,
    ) -> bool {
        match native_copy_variant_payload_type(self.program.type_catalog(), ty) {
            Ok(_) => true,
            Err(message) => {
                let mut message = message;
                message.subject = subject.to_owned();
                self.errors.push(message);
                false
            }
        }
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
                self.validate_value(function, result, "constant result");
                let Some(desc) = function
                    .values
                    .get(result)
                    .and_then(|value| catalog.get(&value.ty))
                else {
                    return;
                };
                let valid = match (desc.abi, literal) {
                    (MirAbiClass::Integer { bits: 32 | 64, .. }, ResolvedLiteral::Int(value)) => {
                        desc.abi
                            != MirAbiClass::Integer {
                                bits: 32,
                                signed: true,
                            }
                            || i32::try_from(*value).is_ok()
                    }
                    (MirAbiClass::Bool, ResolvedLiteral::Bool(_)) => true,
                    _ => false,
                };
                if !valid {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "literal {literal:?} does not match native TypeDesc ABI {:?}",
                            desc.abi
                        ),
                    ));
                }
            }
            MirInstructionKind::Copy { result, source }
            | MirInstructionKind::Move { result, source }
            | MirInstructionKind::Clone { result, source } => {
                self.validate_value(function, result, "result");
                self.validate_value(function, source, "source");
                let (Some(result_value), Some(source_value)) =
                    (function.values.get(result), function.values.get(source))
                else {
                    return;
                };
                if result_value.ty != source_value.ty {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "result and source types disagree",
                    ));
                }
                if let Some(desc) = catalog.get(&source_value.ty) {
                    if matches!(desc.layout, MirLayout::List { .. })
                        && matches!(instruction, MirInstructionKind::Copy { .. })
                    {
                        self.errors.push(NativeMirError::new(
                            subject,
                            "List values require explicit Move or Clone glue; shallow Copy is not permitted",
                        ));
                    } else if !matches!(instruction, MirInstructionKind::Copy { .. }) {
                        let operation = match instruction {
                            MirInstructionKind::Move { .. } => {
                                crate::core::mir::types::MirGlueOperation::MoveOut
                            }
                            MirInstructionKind::Clone { .. } => {
                                crate::core::mir::types::MirGlueOperation::Clone
                            }
                            _ => unreachable!(),
                        };
                        if desc.ownership != MirOwnership::Copy {
                            if let Err(message) = catalog.validate_glue(&source_value.ty, operation)
                            {
                                self.errors.push(NativeMirError::new(subject, message));
                            }
                        }
                    }
                }
            }
            MirInstructionKind::Convert { result, source } => {
                self.validate_value(function, result, "conversion result");
                self.validate_value(function, source, "conversion source");
                let Some(source_ty) = function.values.get(source).map(|value| value.ty.clone())
                else {
                    return;
                };
                let Some(result_ty) = function.values.get(result).map(|value| value.ty.clone())
                else {
                    return;
                };
                match catalog.validate_conversion(&source_ty, &result_ty) {
                    Ok(contract) => {
                        if !matches!(
                            contract.kind,
                            MirConversionKind::ScalarIdentity | MirConversionKind::SignedI32ToI64
                        ) {
                            self.errors.push(NativeMirError::new(
                                subject,
                                "conversion kind is not in the native scalar contract",
                            ));
                        }
                    }
                    Err(message) => self.errors.push(NativeMirError::new(subject, message)),
                }
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => self.validate_unary(function, result, *op, operand, subject),
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => self.validate_binary(function, result, *op, left, right, subject),
            MirInstructionKind::Project {
                result,
                base,
                projection,
            } => self.validate_project(function, result, base, projection, subject),
            MirInstructionKind::Drop { value } => {
                self.validate_value(function, value, "drop value");
                let Some(value) = function.values.get(value) else {
                    return;
                };
                if let Some(desc) = catalog.get(&value.ty) {
                    if desc.ownership != MirOwnership::Copy {
                        if let Err(message) = catalog.validate_glue(
                            &value.ty,
                            crate::core::mir::types::MirGlueOperation::Drop,
                        ) {
                            self.errors.push(NativeMirError::new(subject, message));
                        }
                        if !matches!(desc.layout, MirLayout::List { .. }) {
                            self.errors.push(NativeMirError::new(
                                subject,
                                "only canonical List drop glue is emitted by this native slice",
                            ));
                        }
                    }
                }
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => self.validate_construct(function, result, kind, fields, subject),
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                self.validate_construct_variant(function, result, nominal, variant, fields, subject)
            }
            MirInstructionKind::ConstructList { result, elements } => {
                self.validate_value(function, result, "List result");
                let element_types = elements
                    .iter()
                    .filter_map(|element| {
                        function.values.get(element).map(|value| value.ty.clone())
                    })
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List element is absent from MIR value catalog",
                    ));
                } else if let Some(result_value) = function.values.get(result) {
                    if let Err(message) =
                        catalog.validate_list_construct(&result_value.ty, &element_types)
                    {
                        self.errors.push(NativeMirError::new(subject, message));
                    }
                }
            }
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
            } => {
                let contract = MirBuiltinContract::for_kind(*kind);
                if arguments.len() != contract.arity {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "builtin '{}' has {} arguments; contract requires {}",
                            contract.name,
                            arguments.len(),
                            contract.arity
                        ),
                    ));
                }
                for argument in arguments {
                    self.validate_value(function, argument, "builtin argument");
                }
                self.validate_value(function, result, "builtin result");
                let supported_kind = matches!(
                    kind,
                    MirBuiltinKind::Abs | MirBuiltinKind::Min | MirBuiltinKind::Max
                );
                if !supported_kind {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "builtin kind is not in native MIR contract",
                    ));
                }
                for value in arguments.iter().chain(std::iter::once(result)) {
                    let Some(desc) = function
                        .values
                        .get(value)
                        .and_then(|value| self.program.type_catalog().get(&value.ty))
                    else {
                        continue;
                    };
                    if !contract.accepts_abi(desc.abi)
                        || !contract.accepts_layout(&desc.layout)
                        || desc.ownership != MirOwnership::Copy
                        || (matches!(
                            kind,
                            MirBuiltinKind::Abs | MirBuiltinKind::Min | MirBuiltinKind::Max
                        ) && desc.abi
                            != MirAbiClass::Integer {
                                bits: 64,
                                signed: true,
                            })
                    {
                        self.errors.push(NativeMirError::new(
                            subject,
                            format!(
                                "builtin '{}' TypeDesc/ABI is outside native scalar contract",
                                contract.name
                            ),
                        ));
                    }
                }
            }
            MirInstructionKind::Call {
                result,
                callee,
                arguments,
            } => self.validate_call(function, result.as_ref(), callee, arguments, subject),
            MirInstructionKind::Nop => {}
            _ => self.errors.push(NativeMirError::new(
                subject,
                "instruction shape is not in the native scalar MIR slice",
            )),
        }
    }

    fn validate_same_copy_values(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        source: &MirValueId,
        subject: &str,
    ) {
        self.validate_value(function, result, "result");
        self.validate_value(function, source, "source");
        let (Some(result), Some(source)) =
            (function.values.get(result), function.values.get(source))
        else {
            return;
        };
        if result.ty != source.ty {
            self.errors.push(NativeMirError::new(
                subject,
                "result and source types disagree",
            ));
        }
    }

    fn validate_project(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        subject: &str,
    ) {
        self.validate_value(function, result, "projection result");
        self.validate_value(function, base, "projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        match projection {
            MirProjection::Index(index) => {
                let Some(index_value) = function.values.get(index) else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index is absent from MIR value catalog",
                    ));
                    return;
                };
                if let Err(message) = self.program.type_catalog().validate_list_index(
                    &base_value.ty,
                    &result_value.ty,
                    &index_value.ty,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirProjection::Field(_) => {
                if let Err(message) = self.program.type_catalog().validate_projection(
                    &base_value.ty,
                    &result_value.ty,
                    projection,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            _ => self.errors.push(NativeMirError::new(
                subject,
                "projection shape is outside the native MIR contract",
            )),
        }
    }

    fn validate_construct(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        kind: &MirAggregateKind,
        fields: &[MirValueId],
        subject: &str,
    ) {
        self.validate_value(function, result, "record result");
        for field in fields {
            self.validate_value(function, field, "record field value");
        }
        if !matches!(kind, MirAggregateKind::Record { .. }) {
            self.errors.push(NativeMirError::new(
                subject,
                "tuple construction is outside the flat Copy record native contract",
            ));
            return;
        }
        let Some(result_value) = function.values.get(result) else {
            return;
        };
        let field_types = fields
            .iter()
            .filter_map(|field| function.values.get(field).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();
        if field_types.len() != fields.len() {
            return;
        }
        if let Err(message) =
            self.program
                .type_catalog()
                .validate_aggregate(&result_value.ty, kind, &field_types)
        {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_construct_variant(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        nominal: &crate::core::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
        subject: &str,
    ) {
        self.validate_value(function, result, "variant result");
        for (_, value) in fields {
            self.validate_value(function, value, "variant payload value");
        }
        let Some(result_value) = function.values.get(result) else {
            return;
        };
        let field_ids = fields
            .iter()
            .map(|(field, _)| field.clone())
            .collect::<Vec<_>>();
        let field_types = fields
            .iter()
            .filter_map(|(_, value)| function.values.get(value).map(|info| info.ty.clone()))
            .collect::<Vec<_>>();
        if field_types.len() != fields.len() {
            return;
        }
        if let Err(message) = self.program.type_catalog().validate_variant_construct(
            &result_value.ty,
            nominal,
            variant,
            &field_ids,
            &field_types,
        ) {
            self.errors.push(NativeMirError::new(subject, message));
        }
        if let Err(message) =
            native_copy_variant_payload_type(self.program.type_catalog(), &result_value.ty)
        {
            let mut message = message;
            message.subject = subject.to_owned();
            self.errors.push(message);
        }
    }

    fn validate_switch(
        &mut self,
        function: &MirFunction,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        subject: &str,
    ) {
        self.validate_value(function, scrutinee, "switch scrutinee");
        let Some(scrutinee_value) = function.values.get(scrutinee) else {
            return;
        };
        if let Err(message) =
            native_copy_variant_payload_type(self.program.type_catalog(), &scrutinee_value.ty)
        {
            let mut message = message;
            message.subject = subject.to_owned();
            self.errors.push(message);
            return;
        }
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_switch(&scrutinee_value.ty, arms)
        {
            self.errors.push(NativeMirError::new(subject, message));
        }
        for arm in arms {
            let Some(target) = function.blocks.get(&arm.target) else {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("switch edge target '{}' is absent", arm.target),
                ));
                continue;
            };
            for (index, argument) in arm.arguments.iter().enumerate() {
                self.validate_value(function, argument, "switch edge argument");
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
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch edge argument type disagrees with block parameter",
                    ));
                }
            }
            if target.parameters.len() != arm.arguments.len() + arm.bindings.len() {
                self.errors.push(NativeMirError::new(
                    subject,
                    "switch edge arguments and payload bindings disagree with block parameter arity",
                ));
            }
            let mut binding_fields = BTreeSet::new();
            let variant = match &arm.case {
                MirSwitchCase::Variant(variant) => self
                    .program
                    .type_catalog()
                    .variant(&scrutinee_value.ty, variant),
                MirSwitchCase::Default | MirSwitchCase::Literal(_) => None,
            };
            for (index, binding) in arm.bindings.iter().enumerate() {
                if !binding_fields.insert(binding.field.clone()) {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "switch payload field '{}' is bound more than once",
                            binding.field.0
                        ),
                    ));
                }
                let Some(parameter) = target
                    .parameters
                    .get(arm.arguments.len() + index)
                    .and_then(|parameter| function.values.get(&parameter.value))
                else {
                    continue;
                };
                let Some(variant) = variant else {
                    continue;
                };
                let Some(field) = variant
                    .fields
                    .iter()
                    .find(|field| field.id == binding.field)
                else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "switch payload field '{}' is absent from TypeDesc variant",
                            binding.field.0
                        ),
                    ));
                    continue;
                };
                if parameter.ty != field.ty {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch payload binding type disagrees with TypeDesc",
                    ));
                }
            }
        }
    }

    fn validate_unary(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        op: ResolvedUnaryOp,
        operand: &MirValueId,
        subject: &str,
    ) {
        self.validate_same_copy_values(function, result, operand, subject);
        let Some(desc) = function
            .values
            .get(operand)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
        else {
            return;
        };
        let valid = match op {
            ResolvedUnaryOp::Negate => matches!(
                desc.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true
                }
            ),
            ResolvedUnaryOp::Not => matches!(
                desc.abi,
                MirAbiClass::Integer {
                    bits: 32 | 64,
                    signed: true
                } | MirAbiClass::Bool
            ),
            _ => false,
        };
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!("unary operator {op:?} is outside native scalar contract"),
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
        self.validate_value(function, result, "binary result");
        self.validate_value(function, left, "binary left operand");
        self.validate_value(function, right, "binary right operand");
        let (Some(result), Some(left), Some(right)) = (
            function.values.get(result),
            function.values.get(left),
            function.values.get(right),
        ) else {
            return;
        };
        let Some(left_desc) = self.program.type_catalog().get(&left.ty) else {
            return;
        };
        let Some(right_desc) = self.program.type_catalog().get(&right.ty) else {
            return;
        };
        let Some(result_desc) = self.program.type_catalog().get(&result.ty) else {
            return;
        };
        let same_operands = left.ty == right.ty;
        let integer = matches!(
            left_desc.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true
            }
        );
        let valid = match op {
            ResolvedBinaryOp::Add | ResolvedBinaryOp::Subtract => {
                same_operands && integer && result.ty == left.ty
            }
            ResolvedBinaryOp::Equal | ResolvedBinaryOp::NotEqual => {
                same_operands
                    && matches!(
                        left_desc.abi,
                        MirAbiClass::Integer {
                            bits: 32 | 64,
                            signed: true
                        } | MirAbiClass::Bool
                    )
                    && result_desc.abi == MirAbiClass::Bool
            }
            ResolvedBinaryOp::Less
            | ResolvedBinaryOp::Greater
            | ResolvedBinaryOp::LessEqual
            | ResolvedBinaryOp::GreaterEqual => {
                same_operands && integer && result_desc.abi == MirAbiClass::Bool
            }
            ResolvedBinaryOp::LogicalAnd | ResolvedBinaryOp::LogicalOr => {
                left_desc.abi == MirAbiClass::Bool
                    && right_desc.abi == MirAbiClass::Bool
                    && result_desc.abi == MirAbiClass::Bool
            }
            _ => false,
        };
        if !valid {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "binary operator {op:?} and TypeDesc shapes are outside native scalar contract"
                ),
            ));
        }
    }

    fn validate_call(
        &mut self,
        function: &MirFunction,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let ResolvedCallee::Function(owner) = callee else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("callee {callee:?} is not a canonical function"),
            ));
            return;
        };
        let Some(target) = self.program.functions().get(owner) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("callee '{}' is absent from MIR program", owner.0),
            ));
            return;
        };
        if arguments.len() != target.parameters.len() {
            self.errors.push(NativeMirError::new(
                subject,
                "call arity disagrees with canonical callee",
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            self.validate_value(function, argument, "call argument");
            let Some(argument) = function.values.get(argument) else {
                continue;
            };
            let Some(parameter) = target.values.get(parameter) else {
                continue;
            };
            if argument.ty != parameter.ty {
                self.errors.push(NativeMirError::new(
                    subject,
                    "call argument ABI type disagrees with callee parameter",
                ));
            }
        }
        match (result, target.result.as_str()) {
            (Some(result), _) => {
                self.validate_value(function, result, "call result");
                if function
                    .values
                    .get(result)
                    .is_some_and(|value| value.ty != target.result)
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "call result ABI type disagrees with callee result",
                    ));
                }
            }
            (None, ty)
                if self
                    .program
                    .type_catalog()
                    .get(&target.result)
                    .is_some_and(|desc| desc.abi != MirAbiClass::Unit) =>
            {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("non-unit callee '{ty}' has no MIR result value"),
                ));
            }
            (None, _) => {}
        }
    }

    fn validate_terminator(
        &mut self,
        function: &MirFunction,
        terminator: &MirTerminator,
        subject: &MirBlockId,
    ) {
        let subject = subject.to_string();
        match terminator {
            MirTerminator::Goto {
                target, arguments, ..
            } => self.validate_edge(function, target, arguments, &subject),
            MirTerminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                self.validate_bool(function, condition, &subject);
                self.validate_edge(function, then_target, then_arguments, &subject);
                self.validate_edge(function, else_target, else_arguments, &subject);
            }
            MirTerminator::Switch { scrutinee, arms } => {
                self.validate_switch(function, scrutinee, arms, &subject);
            }
            MirTerminator::SwitchMove { .. } => {
                self.errors.push(NativeMirError::new(
                    subject,
                    "consuming switch has no native Copy-only variant contract",
                ));
            }
            MirTerminator::Return { value } => match value {
                Some(value) => {
                    self.validate_value(function, value, "return value");
                    if function
                        .values
                        .get(value)
                        .is_some_and(|value| value.ty != function.result)
                    {
                        self.errors.push(NativeMirError::new(
                            subject,
                            "return value type disagrees with function result",
                        ));
                    }
                }
                None if self
                    .program
                    .type_catalog()
                    .get(&function.result)
                    .is_some_and(|desc| desc.abi != MirAbiClass::Unit) =>
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "non-unit function has a void MIR return",
                    ));
                }
                None => {}
            },
            MirTerminator::Trap { code } => {
                if let Err(message) = crate::core::mir::types::validate_trap_code(code) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirTerminator::Fault { .. } | MirTerminator::Unreachable => {
                self.errors.push(NativeMirError::new(
                    subject,
                    "fault/unreachable terminator has no native scalar contract",
                ));
            }
        }
    }

    fn validate_bool(&mut self, function: &MirFunction, value: &MirValueId, subject: &str) {
        self.validate_value(function, value, "branch condition");
        if function
            .values
            .get(value)
            .and_then(|value| self.program.type_catalog().get(&value.ty))
            .is_some_and(|desc| desc.abi != MirAbiClass::Bool)
        {
            self.errors.push(NativeMirError::new(
                subject,
                "branch condition is not canonical bool",
            ));
        }
    }

    fn validate_edge(
        &mut self,
        function: &MirFunction,
        target: &MirBlockId,
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let Some(block) = function.blocks.get(target) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("edge target '{}' is absent", target),
            ));
            return;
        };
        if arguments.len() != block.parameters.len() {
            self.errors.push(NativeMirError::new(
                subject,
                "CFG edge arguments disagree with block parameter arity",
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&block.parameters) {
            self.validate_value(function, argument, "edge argument");
            let (Some(argument), Some(parameter)) = (
                function.values.get(argument),
                function.values.get(&parameter.value),
            ) else {
                continue;
            };
            if argument.ty != parameter.ty {
                self.errors.push(NativeMirError::new(
                    subject,
                    "CFG edge argument type disagrees with block parameter",
                ));
            }
        }
    }
}

fn instruction_kind(instruction: &crate::core::mir::MirInstruction) -> &MirInstructionKind {
    &instruction.kind
}

fn mir_symbol(owner: &crate::core::NodeId) -> Result<&str, String> {
    let symbol = owner
        .0
        .strip_prefix("function:")
        .ok_or_else(|| "callable identity is not a function owner".to_string())?;
    if symbol.trim().is_empty() || symbol.contains("::") {
        return Err("only simple function symbols are in the native MIR slice".into());
    }
    if symbol.starts_with("mimi_") {
        return Err("function symbol collides with reserved runtime namespace".into());
    }
    Ok(symbol)
}

struct NativeMirEmitter<'a, 'ctx> {
    generator: &'a mut CodeGenerator<'ctx>,
    program: &'a MirProgram,
    functions: BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
}

impl<'a, 'ctx> NativeMirEmitter<'a, 'ctx> {
    fn new(generator: &'a mut CodeGenerator<'ctx>, program: &'a MirProgram) -> Self {
        Self {
            generator,
            program,
            functions: BTreeMap::new(),
        }
    }

    fn compile(mut self) -> Result<(), NativeMirError> {
        self.declare_functions()?;
        let owners = self.program.functions().keys().cloned().collect::<Vec<_>>();
        for owner in owners {
            let function = self.program.functions().get(&owner).ok_or_else(|| {
                NativeMirError::new(
                    owner.0.clone(),
                    "function disappeared during native emission",
                )
            })?;
            let llvm_function = *self.functions.get(&owner).ok_or_else(|| {
                NativeMirError::new(owner.0.clone(), "LLVM function declaration is absent")
            })?;
            NativeMirFunctionEmitter::new(
                self.generator,
                self.program,
                &self.functions,
                function,
                llvm_function,
            )
            .emit()?;
        }
        self.generator
            .module
            .verify()
            .map_err(|error| NativeMirError::new("LLVM module", error.to_string()))?;
        Ok(())
    }

    fn declare_functions(&mut self) -> Result<(), NativeMirError> {
        for (owner, function) in self.program.functions() {
            let symbol = mir_symbol(owner)
                .map_err(|message| NativeMirError::new(owner.0.clone(), message))?;
            let parameter_types = function
                .parameters
                .iter()
                .map(|parameter| {
                    let ty = function.values.get(parameter).ok_or_else(|| {
                        NativeMirError::new(owner.0.clone(), "parameter is absent from MIR values")
                    })?;
                    native_basic_type(self.generator.context, self.program.type_catalog(), &ty.ty)
                        .map(BasicMetadataTypeEnum::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let result_desc = self
                .program
                .type_catalog()
                .get(&function.result)
                .ok_or_else(|| NativeMirError::new(owner.0.clone(), "result TypeDesc is absent"))?;
            let function_type = if result_desc.abi == MirAbiClass::Unit {
                self.generator
                    .context
                    .void_type()
                    .fn_type(&parameter_types, false)
            } else {
                native_basic_type(
                    self.generator.context,
                    self.program.type_catalog(),
                    &function.result,
                )?
                .fn_type(&parameter_types, false)
            };
            let value =
                self.generator
                    .module
                    .add_function(symbol, function_type, Some(Linkage::External));
            self.functions.insert(owner.clone(), value);
        }
        Ok(())
    }
}

struct NativeMirFunctionEmitter<'a, 'ctx> {
    generator: &'a mut CodeGenerator<'ctx>,
    program: &'a MirProgram,
    functions: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
    function: &'a MirFunction,
    llvm_function: FunctionValue<'ctx>,
    blocks: BTreeMap<MirBlockId, BasicBlock<'ctx>>,
    values: HashMap<MirValueId, BasicValueEnum<'ctx>>,
    phis: HashMap<MirValueId, PhiValue<'ctx>>,
    pending_incoming: Vec<(MirValueId, NativePhiSource<'ctx>, BasicBlock<'ctx>)>,
}

enum NativePhiSource<'ctx> {
    Mir(MirValueId),
    Value(BasicValueEnum<'ctx>),
}

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn new(
        generator: &'a mut CodeGenerator<'ctx>,
        program: &'a MirProgram,
        functions: &'a BTreeMap<crate::core::NodeId, FunctionValue<'ctx>>,
        function: &'a MirFunction,
        llvm_function: FunctionValue<'ctx>,
    ) -> Self {
        Self {
            generator,
            program,
            functions,
            function,
            llvm_function,
            blocks: BTreeMap::new(),
            values: HashMap::new(),
            phis: HashMap::new(),
            pending_incoming: Vec::new(),
        }
    }

    fn emit(mut self) -> Result<(), NativeMirError> {
        self.create_blocks_and_parameters()?;
        let blocks = self.function.blocks.values().cloned().collect::<Vec<_>>();
        for block in &blocks {
            let llvm_block = *self
                .blocks
                .get(&block.id)
                .ok_or_else(|| NativeMirError::new(block.id.to_string(), "LLVM block is absent"))?;
            self.generator.builder.position_at_end(llvm_block);
            for instruction in &block.instructions {
                self.emit_instruction(&instruction.kind, instruction.id.as_str())?;
            }
            self.emit_terminator(&block.terminator, &block.id)?;
        }
        self.add_phi_incomings()?;
        Ok(())
    }

    fn create_blocks_and_parameters(&mut self) -> Result<(), NativeMirError> {
        for block in self.function.blocks.values() {
            let llvm_block = self
                .generator
                .context
                .append_basic_block(self.llvm_function, block.id.as_str());
            self.blocks.insert(block.id.clone(), llvm_block);
        }
        for (index, parameter) in self.function.parameters.iter().enumerate() {
            let value = self
                .llvm_function
                .get_nth_param(index as u32)
                .ok_or_else(|| {
                    NativeMirError::new(parameter.to_string(), "LLVM function parameter is absent")
                })?;
            self.values.insert(parameter.clone(), value);
        }
        for block in self.function.blocks.values() {
            let llvm_block = *self.blocks.get(&block.id).expect("created above");
            self.generator.builder.position_at_end(llvm_block);
            for parameter in &block.parameters {
                let info = self.function.values.get(&parameter.value).ok_or_else(|| {
                    NativeMirError::new(parameter.value.to_string(), "block parameter is absent")
                })?;
                let ty = native_basic_type(
                    self.generator.context,
                    self.program.type_catalog(),
                    &info.ty,
                )?;
                let phi = self
                    .generator
                    .builder
                    .build_phi(ty, parameter.value.as_str())
                    .map_err(|error| {
                        NativeMirError::new(parameter.value.to_string(), error.to_string())
                    })?;
                self.values
                    .insert(parameter.value.clone(), phi.as_basic_value());
                self.phis.insert(parameter.value.clone(), phi);
            }
        }
        Ok(())
    }

    fn emit_instruction(
        &mut self,
        instruction: &MirInstructionKind,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        match instruction {
            MirInstructionKind::Const { result, literal } => {
                let value = self.emit_const(result, literal, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Copy { result, source } => {
                let value = self.value(source, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Move { result, source } => {
                let value = self.value(source, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Clone { result, source } => {
                let value = if matches!(
                    self.value_desc(source, subject)?.layout,
                    MirLayout::List { .. }
                ) {
                    self.emit_list_clone(source, subject)?
                } else {
                    self.value(source, subject)?
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Convert { result, source } => {
                let source_value = self.value(source, subject)?;
                let source_ty = self.value_type(source, subject)?;
                let result_ty = self.value_type(result, subject)?;
                let contract = self
                    .program
                    .type_catalog()
                    .validate_conversion(&source_ty, &result_ty)
                    .map_err(|message| NativeMirError::new(subject, message))?;
                let value = match contract.kind {
                    MirConversionKind::ScalarIdentity => source_value,
                    MirConversionKind::SignedI32ToI64 => {
                        let source = source_value.into_int_value();
                        self.generator
                            .builder
                            .build_int_s_extend(
                                source,
                                self.generator.context.i64_type(),
                                "mir_i32_to_i64",
                            )
                            .map(BasicValueEnum::from)
                            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                    }
                };
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Unary {
                result,
                op,
                operand,
            } => {
                let value = self.emit_unary(result, *op, operand, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Binary {
                result,
                op,
                left,
                right,
            } => {
                let value = self.emit_binary(result, *op, left, right, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Project {
                result,
                base,
                projection,
            } => {
                let value = self.emit_project(result, base, projection, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Construct {
                result,
                kind,
                fields,
            } => {
                let value = self.emit_construct(result, kind, fields, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                let value =
                    self.emit_construct_variant(result, nominal, variant, fields, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::ConstructList { result, elements } => {
                let value = self.emit_list_construct(result, elements, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Drop { value } => {
                self.emit_drop(value, subject)?;
            }
            MirInstructionKind::BuiltinCall {
                result,
                kind,
                arguments,
            } => {
                let value = self.emit_builtin(result, *kind, arguments, subject)?;
                self.values.insert(result.clone(), value);
            }
            MirInstructionKind::Call {
                result,
                callee,
                arguments,
            } => {
                self.emit_call(result.as_ref(), callee, arguments, subject)?;
            }
            MirInstructionKind::Nop => {}
            _ => {
                return Err(NativeMirError::new(
                    subject,
                    "unvalidated instruction reached native emitter",
                ))
            }
        }
        Ok(())
    }

    fn emit_construct(
        &mut self,
        result: &MirValueId,
        kind: &MirAggregateKind,
        fields: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let MirAggregateKind::Record {
            nominal,
            fields: field_ids,
        } = kind
        else {
            return Err(NativeMirError::new(
                subject,
                "tuple construction reached the flat record emitter",
            ));
        };
        let result_ty = self.value_type(result, subject)?;
        let descriptor = self
            .program
            .type_catalog()
            .get(&result_ty)
            .ok_or_else(|| NativeMirError::new(subject, "record result TypeDesc is absent"))?;
        let MirLayout::Record {
            nominal: expected_nominal,
            fields: layout_fields,
        } = &descriptor.layout
        else {
            return Err(NativeMirError::new(
                subject,
                "record construction result has no canonical record layout",
            ));
        };
        if nominal != expected_nominal || field_ids.len() != fields.len() {
            return Err(NativeMirError::new(
                subject,
                "record construction does not match its TypeDesc layout",
            ));
        }
        let struct_ty = native_basic_type(
            self.generator.context,
            self.program.type_catalog(),
            &result_ty,
        )?
        .into_struct_type();
        let mut aggregate = struct_ty.get_undef();
        for (field_id, source) in field_ids.iter().zip(fields) {
            let index = layout_fields
                .iter()
                .position(|field| field.id == *field_id)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject,
                        format!("record field '{}' is absent from TypeDesc", field_id.0),
                    )
                })?;
            let value = self.value(source, subject)?;
            aggregate = self
                .generator
                .builder
                .build_insert_value(aggregate, value, index as u32, "mir_record_insert")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?
                .into_struct_value();
        }
        Ok(aggregate.into())
    }

    fn emit_construct_variant(
        &mut self,
        result: &MirValueId,
        nominal: &crate::core::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let (expected_nominal, variants) = self
            .program
            .type_catalog()
            .variant_layout(&result_ty)
            .ok_or_else(|| {
            NativeMirError::new(subject, "variant result has no canonical TypeDesc layout")
        })?;
        if nominal.as_str() != expected_nominal {
            return Err(NativeMirError::new(
                subject,
                format!(
                    "variant nominal '{}' disagrees with canonical nominal '{}'",
                    nominal.as_str(),
                    expected_nominal
                ),
            ));
        }
        let variant_desc = variants
            .iter()
            .find(|candidate| candidate.id == *variant)
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    format!("variant '{}' is absent from TypeDesc", variant.0),
                )
            })?;
        let field_ids = fields
            .iter()
            .map(|(field, _)| field.clone())
            .collect::<Vec<_>>();
        let field_types = fields
            .iter()
            .map(|(_, value)| self.value_type(value, subject))
            .collect::<Result<Vec<_>, _>>()?;
        self.program
            .type_catalog()
            .validate_variant_construct(&result_ty, nominal, variant, &field_ids, &field_types)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let payload_ty = native_copy_variant_payload_type(self.program.type_catalog(), &result_ty)?;
        let struct_ty = native_basic_type(
            self.generator.context,
            self.program.type_catalog(),
            &result_ty,
        )?
        .into_struct_type();
        let mut aggregate = struct_ty.get_undef();
        aggregate = self
            .generator
            .builder
            .build_insert_value(
                aggregate,
                self.generator
                    .context
                    .i8_type()
                    .const_int(u64::from(variant_desc.discriminant), false),
                0,
                "mir_variant_tag",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        let payload = if fields.is_empty() {
            native_basic_type(
                self.generator.context,
                self.program.type_catalog(),
                &payload_ty,
            )?
            .const_zero()
        } else {
            if fields.len() != 1 || fields[0].0 != variant_desc.fields[0].id {
                return Err(NativeMirError::new(
                    subject,
                    "variant payload fields do not match the canonical TypeDesc field",
                ));
            }
            self.value(&fields[0].1, subject)?
        };
        aggregate = self
            .generator
            .builder
            .build_insert_value(aggregate, payload, 1, "mir_variant_payload")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_struct_value();
        Ok(aggregate.into())
    }

    fn emit_list_construct(
        &mut self,
        result: &MirValueId,
        elements: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let result_ty = self.value_type(result, subject)?;
        let catalog = self.program.type_catalog();
        let result_desc = catalog
            .get(&result_ty)
            .ok_or_else(|| NativeMirError::new(subject, "List result TypeDesc is absent"))?;
        let MirLayout::List { element } = &result_desc.layout else {
            return Err(NativeMirError::new(
                subject,
                "List construction result has no canonical List layout",
            ));
        };
        catalog
            .validate_list_construct(
                &result_ty,
                &elements
                    .iter()
                    .map(|value| self.value_type(value, subject))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .map_err(|message| NativeMirError::new(subject, message))?;
        let kind = native_list_kind(catalog, &result_ty)?;
        let kind_value = self
            .generator
            .context
            .i8_type()
            .const_int(kind as u64, false);
        let new_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_new_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let list = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    new_fn,
                    &[BasicMetadataValueEnum::from(kind_value)],
                    "mir_list_new",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "List constructor returned void"))?
        .into_pointer_value();
        self.emit_list_null_abort(list, subject, "canonical MIR List allocation failed")?;

        let push_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_push_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let element_desc = catalog
            .get(element)
            .ok_or_else(|| NativeMirError::new(subject, "List element TypeDesc is absent"))?;
        for value in elements {
            let scalar = self.emit_list_scalar_as_i64(value, element_desc, subject)?;
            let status = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        push_fn,
                        &[
                            BasicMetadataValueEnum::from(list),
                            BasicMetadataValueEnum::from(kind_value),
                            BasicMetadataValueEnum::from(scalar),
                        ],
                        "mir_list_push",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "List append returned void"))?
            .into_int_value();
            let failed = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    status,
                    self.generator.context.i8_type().const_zero(),
                    "mir_list_push_failed",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let fail = self
                .generator
                .context
                .append_basic_block(self.llvm_function, "mir_list_push_abort");
            let ok = self
                .generator
                .context
                .append_basic_block(self.llvm_function, "mir_list_push_ok");
            self.generator
                .builder
                .build_conditional_branch(failed, fail, ok)
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.position_at_end(fail);
            self.emit_abort_with_message("[E0800] canonical MIR List append failed", subject)?;
            self.generator.builder.position_at_end(ok);
        }
        Ok(list.into())
    }

    fn emit_list_clone(
        &mut self,
        source: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let source_ty = self.value_type(source, subject)?;
        let kind = native_list_kind(self.program.type_catalog(), &source_ty)?;
        let source_value = self.value(source, subject)?.into_pointer_value();
        let kind_value = self
            .generator
            .context
            .i8_type()
            .const_int(kind as u64, false);
        let clone_fn = self
            .generator
            .get_runtime_fn("mimi_mir_list_clone_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let clone = call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(
                    clone_fn,
                    &[
                        BasicMetadataValueEnum::from(source_value),
                        BasicMetadataValueEnum::from(kind_value),
                    ],
                    "mir_list_clone",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
        )
        .ok_or_else(|| NativeMirError::new(subject, "List clone returned void"))?
        .into_pointer_value();
        self.emit_list_null_abort(clone, subject, "canonical MIR List clone failed")?;
        Ok(clone.into())
    }

    fn emit_drop(&mut self, value: &MirValueId, subject: &str) -> Result<(), NativeMirError> {
        let desc = self.value_desc(value, subject)?;
        if desc.ownership == MirOwnership::Copy {
            return Ok(());
        }
        let ty = self.value_type(value, subject)?;
        let kind = native_list_kind(self.program.type_catalog(), &ty)?;
        let function = self
            .generator
            .get_runtime_fn("mimi_mir_list_drop_scalar")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let kind_value = self
            .generator
            .context
            .i8_type()
            .const_int(kind as u64, false);
        self.generator
            .builder
            .build_call(
                function,
                &[
                    BasicMetadataValueEnum::from(self.value(value, subject)?.into_pointer_value()),
                    BasicMetadataValueEnum::from(kind_value),
                ],
                "mir_list_drop",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    fn emit_list_null_abort(
        &mut self,
        value: inkwell::values::PointerValue<'ctx>,
        subject: &str,
        message: &str,
    ) -> Result<(), NativeMirError> {
        let is_null = self
            .generator
            .builder
            .build_is_null(value, "mir_list_is_null")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let fail = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_list_null_abort");
        let ok = self
            .generator
            .context
            .append_basic_block(self.llvm_function, "mir_list_nonnull");
        self.generator
            .builder
            .build_conditional_branch(is_null, fail, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(fail);
        self.emit_abort_with_message(message, subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(())
    }

    fn emit_list_scalar_as_i64(
        &mut self,
        value: &MirValueId,
        element_desc: &MirTypeDesc,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        let value = self.value(value, subject)?.into_int_value();
        match element_desc.abi {
            MirAbiClass::Integer {
                bits: 64,
                signed: true,
            } => Ok(value),
            MirAbiClass::Integer {
                bits: 32,
                signed: true,
            } => self
                .generator
                .builder
                .build_int_s_extend(value, self.generator.context.i64_type(), "mir_list_i32")
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            MirAbiClass::Bool => self
                .generator
                .builder
                .build_int_z_extend(value, self.generator.context.i64_type(), "mir_list_bool")
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            abi => Err(NativeMirError::new(
                subject,
                format!("List element ABI {abi:?} is not scalar native storage"),
            )),
        }
    }

    fn emit_project(
        &mut self,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        if let MirProjection::Index(index) = projection {
            let base_ty = self.value_type(base, subject)?;
            let result_ty = self.value_type(result, subject)?;
            let index_ty = self.value_type(index, subject)?;
            let catalog = self.program.type_catalog();
            catalog
                .validate_list_index(&base_ty, &result_ty, &index_ty)
                .map_err(|message| NativeMirError::new(subject, message))?;
            let kind = native_list_kind(catalog, &base_ty)?;
            let index_desc = catalog
                .get(&index_ty)
                .ok_or_else(|| NativeMirError::new(subject, "List index TypeDesc is absent"))?;
            let index_value = self.value(index, subject)?.into_int_value();
            let index_value = match index_desc.abi {
                MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                } => index_value,
                MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                } => self
                    .generator
                    .builder
                    .build_int_s_extend(
                        index_value,
                        self.generator.context.i64_type(),
                        "mir_list_index_i32",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                _ => {
                    return Err(NativeMirError::new(
                        subject,
                        "List index is outside signed integer native storage",
                    ))
                }
            };
            let kind_value = self
                .generator
                .context
                .i8_type()
                .const_int(kind as u64, false);
            let get_fn = self
                .generator
                .get_runtime_fn("mimi_mir_list_get_scalar")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let raw = call_try_basic_value(
                &self
                    .generator
                    .builder
                    .build_call(
                        get_fn,
                        &[
                            BasicMetadataValueEnum::from(
                                self.value(base, subject)?.into_pointer_value(),
                            ),
                            BasicMetadataValueEnum::from(kind_value),
                            BasicMetadataValueEnum::from(index_value),
                        ],
                        "mir_list_get",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
            )
            .ok_or_else(|| NativeMirError::new(subject, "List projection returned void"))?
            .into_int_value();
            let result_desc = catalog
                .get(&result_ty)
                .ok_or_else(|| NativeMirError::new(subject, "List result TypeDesc is absent"))?;
            return match result_desc.abi {
                MirAbiClass::Integer {
                    bits: 64,
                    signed: true,
                } => Ok(raw.into()),
                MirAbiClass::Integer {
                    bits: 32,
                    signed: true,
                } => self
                    .generator
                    .builder
                    .build_int_truncate(
                        raw,
                        self.generator.context.i32_type(),
                        "mir_list_i32_result",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string())),
                MirAbiClass::Bool => self
                    .generator
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        raw,
                        self.generator.context.i64_type().const_zero(),
                        "mir_list_bool_result",
                    )
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string())),
                _ => Err(NativeMirError::new(
                    subject,
                    "List projection result is outside scalar native storage",
                )),
            };
        }
        let MirProjection::Field(field_id) = projection else {
            return Err(NativeMirError::new(
                subject,
                "projection shape is outside the native aggregate adapter",
            ));
        };
        let base_ty = self.value_type(base, subject)?;
        let descriptor =
            self.program.type_catalog().get(&base_ty).ok_or_else(|| {
                NativeMirError::new(subject, "projection base TypeDesc is absent")
            })?;
        let MirLayout::Record { fields, .. } = &descriptor.layout else {
            return Err(NativeMirError::new(
                subject,
                "projection base has no canonical record layout",
            ));
        };
        let index = fields
            .iter()
            .position(|field| field.id == *field_id)
            .ok_or_else(|| {
                NativeMirError::new(
                    subject,
                    format!("record field '{}' is absent from TypeDesc", field_id.0),
                )
            })?;
        let aggregate = self.value(base, subject)?.into_struct_value();
        self.generator
            .builder
            .build_extract_value(aggregate, index as u32, "mir_record_project")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    fn emit_const(
        &self,
        result: &MirValueId,
        literal: &ResolvedLiteral,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let ty = self.value_type(result, subject)?;
        let desc = self
            .program
            .type_catalog()
            .get(&ty)
            .ok_or_else(|| NativeMirError::new(subject, "constant TypeDesc is absent"))?;
        match (desc.abi, literal) {
            (MirAbiClass::Integer { bits: 32 | 64, .. }, ResolvedLiteral::Int(value)) => {
                let int_ty = match desc.abi {
                    MirAbiClass::Integer { bits: 32, .. } => self.generator.context.i32_type(),
                    _ => self.generator.context.i64_type(),
                };
                Ok(int_ty.const_int(*value as u64, true).into())
            }
            (MirAbiClass::Bool, ResolvedLiteral::Bool(value)) => Ok(self
                .generator
                .context
                .bool_type()
                .const_int(u64::from(*value), false)
                .into()),
            _ => Err(NativeMirError::new(
                subject,
                "literal is outside native scalar ABI",
            )),
        }
    }

    fn emit_unary(
        &mut self,
        result: &MirValueId,
        op: ResolvedUnaryOp,
        operand: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let value = self.value(operand, subject)?.into_int_value();
        match op {
            ResolvedUnaryOp::Not => self
                .generator
                .builder
                .build_not(value, "mir_not")
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            ResolvedUnaryOp::Negate => {
                let int_ty = value.get_type();
                let min = int_ty.const_int(1u64 << (int_ty.get_bit_width() - 1), false);
                let is_min = self
                    .generator
                    .builder
                    .build_int_compare(IntPredicate::EQ, value, min, "mir_abs_min")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let function = self.llvm_function;
                let trap = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_neg_overflow");
                let ok = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_neg_ok");
                self.generator
                    .builder
                    .build_conditional_branch(is_min, trap, ok)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.emit_overflow_trap(trap, "neg", subject)?;
                self.generator.builder.position_at_end(ok);
                self.generator
                    .builder
                    .build_int_sub(int_ty.const_zero(), value, "mir_neg")
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            _ => Err(NativeMirError::new(
                subject,
                format!("unary operator {op:?} is not emitted"),
            )),
        }
        .map(|value| {
            let _ = result;
            value
        })
    }

    fn emit_binary(
        &mut self,
        result: &MirValueId,
        op: ResolvedBinaryOp,
        left: &MirValueId,
        right: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let left_value = self.value(left, subject)?.into_int_value();
        let right_value = self.value(right, subject)?.into_int_value();
        match op {
            ResolvedBinaryOp::Add => self
                .emit_checked_add_sub(left_value, right_value, false, subject)
                .map(BasicValueEnum::from),
            ResolvedBinaryOp::Subtract => self
                .emit_checked_add_sub(left_value, right_value, true, subject)
                .map(BasicValueEnum::from),
            ResolvedBinaryOp::Equal => {
                self.compare(IntPredicate::EQ, left_value, right_value, subject)
            }
            ResolvedBinaryOp::NotEqual => {
                self.compare(IntPredicate::NE, left_value, right_value, subject)
            }
            ResolvedBinaryOp::Less => {
                self.compare(IntPredicate::SLT, left_value, right_value, subject)
            }
            ResolvedBinaryOp::Greater => {
                self.compare(IntPredicate::SGT, left_value, right_value, subject)
            }
            ResolvedBinaryOp::LessEqual => {
                self.compare(IntPredicate::SLE, left_value, right_value, subject)
            }
            ResolvedBinaryOp::GreaterEqual => {
                self.compare(IntPredicate::SGE, left_value, right_value, subject)
            }
            ResolvedBinaryOp::LogicalAnd => self
                .generator
                .builder
                .build_and(left_value, right_value, "mir_and")
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            ResolvedBinaryOp::LogicalOr => self
                .generator
                .builder
                .build_or(left_value, right_value, "mir_or")
                .map(BasicValueEnum::from)
                .map_err(|error| NativeMirError::new(subject, error.to_string())),
            _ => Err(NativeMirError::new(
                subject,
                format!("binary operator {op:?} is not emitted"),
            )),
        }
        .map(|value| {
            let _ = result;
            value
        })
    }

    fn compare(
        &self,
        predicate: IntPredicate,
        left: inkwell::values::IntValue<'ctx>,
        right: inkwell::values::IntValue<'ctx>,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.generator
            .builder
            .build_int_compare(predicate, left, right, "mir_cmp")
            .map(BasicValueEnum::from)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))
    }

    fn emit_checked_add_sub(
        &mut self,
        left: inkwell::values::IntValue<'ctx>,
        right: inkwell::values::IntValue<'ctx>,
        subtract: bool,
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        let result = if subtract {
            self.generator.builder.build_int_sub(left, right, "mir_sub")
        } else {
            self.generator.builder.build_int_add(left, right, "mir_add")
        }
        .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let zero = left.get_type().const_zero();
        let left_nonnegative = self
            .generator
            .builder
            .build_int_compare(IntPredicate::SGE, left, zero, "mir_left_nonnegative")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let right_nonnegative = self
            .generator
            .builder
            .build_int_compare(IntPredicate::SGE, right, zero, "mir_right_nonnegative")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let result_nonnegative = self
            .generator
            .builder
            .build_int_compare(IntPredicate::SGE, result, zero, "mir_result_nonnegative")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let overflow = if subtract {
            let left_pos_right_neg = self
                .generator
                .builder
                .build_and(
                    left_nonnegative,
                    self.generator
                        .builder
                        .build_not(right_nonnegative, "mir_not_right_nonnegative")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                    "mir_sub_case_a",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let left_neg_right_pos = self
                .generator
                .builder
                .build_and(
                    self.generator
                        .builder
                        .build_not(left_nonnegative, "mir_not_left_nonnegative")
                        .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                    right_nonnegative,
                    "mir_sub_case_b",
                )
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let result_neg = self
                .generator
                .builder
                .build_not(result_nonnegative, "mir_not_result_nonnegative")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let result_pos = result_nonnegative;
            self.generator.builder.build_or(
                self.generator
                    .builder
                    .build_and(left_pos_right_neg, result_neg, "mir_sub_overflow_a")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                self.generator
                    .builder
                    .build_and(left_neg_right_pos, result_pos, "mir_sub_overflow_b")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                "mir_sub_overflow",
            )
        } else {
            let same_nonnegative = self
                .generator
                .builder
                .build_and(left_nonnegative, right_nonnegative, "mir_add_case_a")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let left_negative = self
                .generator
                .builder
                .build_not(left_nonnegative, "mir_not_left_nonnegative_add")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let right_negative = self
                .generator
                .builder
                .build_not(right_nonnegative, "mir_not_right_nonnegative_add")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let same_negative = self
                .generator
                .builder
                .build_and(left_negative, right_negative, "mir_add_case_b")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            let result_negative = self
                .generator
                .builder
                .build_not(result_nonnegative, "mir_not_result_nonnegative_add")
                .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
            self.generator.builder.build_or(
                self.generator
                    .builder
                    .build_and(same_nonnegative, result_negative, "mir_add_overflow_a")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                self.generator
                    .builder
                    .build_and(same_negative, result_nonnegative, "mir_add_overflow_b")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?,
                "mir_add_overflow",
            )
        }
        .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let function = self.llvm_function;
        let trap = self
            .generator
            .context
            .append_basic_block(function, "mir_arithmetic_overflow");
        let ok = self
            .generator
            .context
            .append_basic_block(function, "mir_arithmetic_ok");
        self.generator
            .builder
            .build_conditional_branch(overflow, trap, ok)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_overflow_trap(trap, if subtract { "sub" } else { "add" }, subject)?;
        self.generator.builder.position_at_end(ok);
        Ok(result)
    }

    fn emit_builtin(
        &mut self,
        result: &MirValueId,
        kind: MirBuiltinKind,
        arguments: &[MirValueId],
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let left = self
            .value(
                arguments
                    .first()
                    .ok_or_else(|| NativeMirError::new(subject, "builtin argument is absent"))?,
                subject,
            )?
            .into_int_value();
        match kind {
            MirBuiltinKind::Abs => {
                let min = left
                    .get_type()
                    .const_int(1u64 << (left.get_type().get_bit_width() - 1), false);
                let is_min = self
                    .generator
                    .builder
                    .build_int_compare(IntPredicate::EQ, left, min, "mir_abs_min")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let function = self.llvm_function;
                let trap = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_abs_overflow");
                let ok = self
                    .generator
                    .context
                    .append_basic_block(function, "mir_abs_ok");
                self.generator
                    .builder
                    .build_conditional_branch(is_min, trap, ok)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.emit_overflow_trap(trap, "abs", subject)?;
                self.generator.builder.position_at_end(ok);
                let negated = self
                    .generator
                    .builder
                    .build_int_sub(left.get_type().const_zero(), left, "mir_abs_negated")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                let is_nonnegative = self
                    .generator
                    .builder
                    .build_int_compare(
                        IntPredicate::SGE,
                        left,
                        left.get_type().const_zero(),
                        "mir_abs_nonnegative",
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_select(is_nonnegative, left, negated, "mir_abs")
                    .map(BasicValueEnum::from)
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
            MirBuiltinKind::Min | MirBuiltinKind::Max => {
                let right = self
                    .value(
                        arguments.get(1).ok_or_else(|| {
                            NativeMirError::new(subject, "builtin right argument is absent")
                        })?,
                        subject,
                    )?
                    .into_int_value();
                let predicate = if kind == MirBuiltinKind::Min {
                    IntPredicate::SLT
                } else {
                    IntPredicate::SGT
                };
                let condition = self
                    .generator
                    .builder
                    .build_int_compare(predicate, left, right, "mir_minmax_cmp")
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
                self.generator
                    .builder
                    .build_select(
                        condition,
                        left,
                        right,
                        match kind {
                            MirBuiltinKind::Min => "mir_min",
                            MirBuiltinKind::Max => "mir_max",
                            MirBuiltinKind::Abs => unreachable!(),
                        },
                    )
                    .map_err(|error| NativeMirError::new(subject, error.to_string()))
            }
        }
        .map(|value| {
            let _ = result;
            value
        })
    }

    fn emit_call(
        &mut self,
        result: Option<&MirValueId>,
        callee: &ResolvedCallee,
        arguments: &[MirValueId],
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ResolvedCallee::Function(owner) = callee else {
            return Err(NativeMirError::new(
                subject,
                format!("callee {callee:?} is not a canonical function"),
            ));
        };
        let function = *self.functions.get(owner).ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("callee '{}' is absent from native declarations", owner.0),
            )
        })?;
        let values = arguments
            .iter()
            .map(|argument| {
                self.value(argument, subject)
                    .map(BasicMetadataValueEnum::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let call = self
            .generator
            .builder
            .build_call(function, &values, "mir_call")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        if let Some(result) = result {
            let desc = self.value_desc(result, subject)?;
            if desc.abi != MirAbiClass::Unit {
                let value = call_try_basic_value(&call).ok_or_else(|| {
                    NativeMirError::new(subject, "non-unit MIR call returned void")
                })?;
                self.values.insert(result.clone(), value);
            }
        }
        Ok(())
    }

    fn emit_terminator(
        &mut self,
        terminator: &MirTerminator,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let current = self.generator.builder.get_insert_block().ok_or_else(|| {
            NativeMirError::new(
                subject.to_string(),
                "terminator has no LLVM insertion block",
            )
        })?;
        match terminator {
            MirTerminator::Goto {
                target, arguments, ..
            } => {
                self.queue_edge(target, arguments, current, subject)?;
                self.generator
                    .builder
                    .build_unconditional_branch(*self.blocks.get(target).expect("validated target"))
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            }
            MirTerminator::Branch {
                condition,
                then_target,
                then_arguments,
                else_target,
                else_arguments,
                ..
            } => {
                let condition = self
                    .value(condition, &subject.to_string())?
                    .into_int_value();
                self.queue_edge(then_target, then_arguments, current, subject)?;
                self.queue_edge(else_target, else_arguments, current, subject)?;
                self.generator
                    .builder
                    .build_conditional_branch(
                        condition,
                        *self.blocks.get(then_target).expect("validated target"),
                        *self.blocks.get(else_target).expect("validated target"),
                    )
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            }
            MirTerminator::Switch { scrutinee, arms } => {
                self.emit_switch(scrutinee, arms, subject)?;
            }
            MirTerminator::Return { value } => match value {
                Some(value) => {
                    let value = self.value(value, &subject.to_string())?;
                    self.generator
                        .builder
                        .build_return(Some(&value as &dyn BasicValue))
                        .map_err(|error| {
                            NativeMirError::new(subject.to_string(), error.to_string())
                        })?;
                }
                None => {
                    self.generator.builder.build_return(None).map_err(|error| {
                        NativeMirError::new(subject.to_string(), error.to_string())
                    })?;
                }
            },
            MirTerminator::Trap { code } => {
                if let Err(message) = crate::core::mir::types::validate_trap_code(code) {
                    return Err(NativeMirError::new(subject.to_string(), message));
                }
                self.emit_abort_with_message(code, &subject.to_string())?;
            }
            _ => {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "unvalidated terminator reached native emitter",
                ))
            }
        }
        Ok(())
    }

    fn emit_switch(
        &mut self,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let scrutinee_value = self.value(scrutinee, &subject.to_string())?;
        let scrutinee_ty = self.value_type(scrutinee, &subject.to_string())?;
        let variant_arms = arms
            .iter()
            .filter(|arm| matches!(arm.case, MirSwitchCase::Variant(_)))
            .cloned()
            .collect::<Vec<_>>();
        let default_arm = arms
            .iter()
            .find(|arm| matches!(arm.case, MirSwitchCase::Default))
            .cloned();

        if variant_arms.is_empty() {
            let default_arm = default_arm.ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "variant switch has no native arm")
            })?;
            let current = self.generator.builder.get_insert_block().ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "switch has no LLVM insertion block")
            })?;
            self.queue_edge(
                &default_arm.target,
                &default_arm.arguments,
                current,
                subject,
            )?;
            self.generator
                .builder
                .build_unconditional_branch(
                    *self
                        .blocks
                        .get(&default_arm.target)
                        .expect("validated default target"),
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            return Ok(());
        }

        let tag = self
            .generator
            .builder
            .build_extract_value(
                scrutinee_value.into_struct_value(),
                0,
                "mir_variant_tag_load",
            )
            .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?
            .into_int_value();
        for (index, arm) in variant_arms.iter().enumerate() {
            let MirSwitchCase::Variant(variant_id) = &arm.case else {
                unreachable!("variant arms were filtered above")
            };
            let variant = self
                .program
                .type_catalog()
                .variant(&scrutinee_ty, variant_id)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject.to_string(),
                        format!("variant '{}' is absent from TypeDesc", variant_id.0),
                    )
                })?
                .clone();
            let condition = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    tag,
                    self.generator
                        .context
                        .i8_type()
                        .const_int(u64::from(variant.discriminant), false),
                    "mir_variant_case",
                )
                .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            let current = self.generator.builder.get_insert_block().ok_or_else(|| {
                NativeMirError::new(subject.to_string(), "switch case has no LLVM block")
            })?;
            self.queue_variant_edge(
                &arm.target,
                &arm.arguments,
                &arm.bindings,
                &variant,
                scrutinee_value,
                current,
                subject,
            )?;
            let target = *self
                .blocks
                .get(&arm.target)
                .expect("validated variant target");
            if index + 1 < variant_arms.len() {
                let next = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_variant_next");
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, next)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.generator.builder.position_at_end(next);
            } else if let Some(default_arm) = &default_arm {
                self.queue_edge(
                    &default_arm.target,
                    &default_arm.arguments,
                    current,
                    subject,
                )?;
                self.generator
                    .builder
                    .build_conditional_branch(
                        condition,
                        target,
                        *self
                            .blocks
                            .get(&default_arm.target)
                            .expect("validated default target"),
                    )
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            } else {
                let unreachable = self
                    .generator
                    .context
                    .append_basic_block(self.llvm_function, "mir_variant_unreachable");
                self.generator
                    .builder
                    .build_conditional_branch(condition, target, unreachable)
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
                self.generator.builder.position_at_end(unreachable);
                self.generator
                    .builder
                    .build_unreachable()
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?;
            }
        }
        Ok(())
    }

    fn queue_edge(
        &mut self,
        target: &MirBlockId,
        arguments: &[MirValueId],
        predecessor: BasicBlock<'ctx>,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let block = self
            .function
            .blocks
            .get(target)
            .ok_or_else(|| NativeMirError::new(subject.to_string(), "edge target is absent"))?;
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            self.pending_incoming.push((
                parameter.value.clone(),
                NativePhiSource::Mir(argument.clone()),
                predecessor,
            ));
        }
        Ok(())
    }

    fn queue_variant_edge(
        &mut self,
        target: &MirBlockId,
        arguments: &[MirValueId],
        bindings: &[crate::core::mir::MirSwitchBinding],
        variant: &crate::core::mir::types::MirVariantDesc,
        scrutinee: BasicValueEnum<'ctx>,
        predecessor: BasicBlock<'ctx>,
        subject: &MirBlockId,
    ) -> Result<(), NativeMirError> {
        let block = self
            .function
            .blocks
            .get(target)
            .ok_or_else(|| NativeMirError::new(subject.to_string(), "edge target is absent"))?;
        if block.parameters.len() != arguments.len() + bindings.len() {
            return Err(NativeMirError::new(
                subject.to_string(),
                "variant edge does not match target block parameter arity",
            ));
        }
        for (parameter, argument) in block.parameters.iter().zip(arguments) {
            self.pending_incoming.push((
                parameter.value.clone(),
                NativePhiSource::Mir(argument.clone()),
                predecessor,
            ));
        }
        let payload = if bindings.is_empty() {
            None
        } else {
            let field = variant
                .fields
                .iter()
                .find(|field| field.id == bindings[0].field)
                .ok_or_else(|| {
                    NativeMirError::new(
                        subject.to_string(),
                        "variant payload binding field is absent from TypeDesc",
                    )
                })?;
            if field.id != bindings[0].field || variant.fields.len() != 1 {
                return Err(NativeMirError::new(
                    subject.to_string(),
                    "variant payload binding is outside the single-payload native contract",
                ));
            }
            Some(
                self.generator
                    .builder
                    .build_extract_value(
                        scrutinee.into_struct_value(),
                        1,
                        "mir_variant_payload_load",
                    )
                    .map_err(|error| NativeMirError::new(subject.to_string(), error.to_string()))?,
            )
        };
        if let Some(payload) = payload {
            for (index, binding) in bindings.iter().enumerate() {
                if index != 0 || binding.field != variant.fields[0].id {
                    return Err(NativeMirError::new(
                        subject.to_string(),
                        "variant payload binding is outside the single-payload native contract",
                    ));
                }
                let parameter = &block.parameters[arguments.len() + index];
                self.pending_incoming.push((
                    parameter.value.clone(),
                    NativePhiSource::Value(payload),
                    predecessor,
                ));
            }
        }
        Ok(())
    }

    fn add_phi_incomings(&mut self) -> Result<(), NativeMirError> {
        for (parameter, source, predecessor) in &self.pending_incoming {
            let value = match source {
                NativePhiSource::Mir(source) => *self.values.get(source).ok_or_else(|| {
                    NativeMirError::new(source.to_string(), "phi incoming value was not emitted")
                })?,
                NativePhiSource::Value(value) => *value,
            };
            let phi = self.phis.get(parameter).ok_or_else(|| {
                NativeMirError::new(parameter.to_string(), "phi parameter is absent")
            })?;
            phi.add_incoming(&[(&value as &dyn BasicValue, *predecessor)]);
        }
        Ok(())
    }

    fn emit_overflow_trap(
        &mut self,
        block: BasicBlock<'ctx>,
        operation: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        self.generator.builder.position_at_end(block);
        let function = self
            .generator
            .get_runtime_fn("mimi_trap_overflow")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let message = self
            .generator
            .builder
            .build_global_string_ptr(operation, "mir_overflow_operation")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                function,
                &[BasicMetadataValueEnum::from(message.as_pointer_value())],
                "mir_overflow_trap",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    fn emit_abort_with_message(
        &mut self,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let function = self.generator.get_or_declare_abort_fn();
        let message = self
            .generator
            .builder
            .build_global_string_ptr(message, "mir_trap_message")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                function,
                &[BasicMetadataValueEnum::from(message.as_pointer_value())],
                "mir_trap",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unreachable()
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        Ok(())
    }

    fn value(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        self.values.get(id).copied().ok_or_else(|| {
            NativeMirError::new(
                subject,
                format!("value '{id}' is not available at native emission site"),
            )
        })
    }

    fn value_type(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
        self.function
            .values
            .get(id)
            .map(|value| value.ty.clone())
            .ok_or_else(|| {
                NativeMirError::new(subject, format!("value '{id}' has no canonical type"))
            })
    }

    fn value_desc(
        &self,
        id: &MirValueId,
        subject: &str,
    ) -> Result<&crate::core::mir::types::MirTypeDesc, NativeMirError> {
        let ty = self.value_type(id, subject)?;
        self.program
            .type_catalog()
            .get(&ty)
            .ok_or_else(|| NativeMirError::new(subject, format!("value '{id}' has no TypeDesc")))
    }
}

fn is_native_scalar_descriptor(desc: &MirTypeDesc) -> bool {
    desc.layout == MirLayout::Scalar
        && matches!(
            desc.abi,
            MirAbiClass::Integer {
                bits: 32 | 64,
                signed: true,
            } | MirAbiClass::Bool
        )
        && desc.ownership == MirOwnership::Copy
        && desc.glue
            == (MirGlueContract {
                move_out: MirGlueKind::Noop,
                clone: MirGlueKind::Noop,
                drop: MirGlueKind::Noop,
            })
}

/// Map a TypeDesc-proven canonical scalar List element to the native runtime
/// tag.  This is the only place where the native ABI names the runtime's
/// serialized `ListElementKind` values; it never infers the element type from
/// an LLVM pointer or from surface syntax.
fn native_list_kind(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<i8, NativeMirError> {
    catalog
        .validate_list_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)
        .map_err(|message| NativeMirError::new(ty.as_str(), message))?;
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "List TypeDesc is absent"))?;
    let MirLayout::List { element } = &desc.layout else {
        return Err(NativeMirError::new(
            ty.as_str(),
            "native List ABI requested for a non-List TypeDesc",
        ));
    };
    let element_desc = catalog.get(element).ok_or_else(|| {
        NativeMirError::new(
            ty.as_str(),
            format!("List element TypeDesc '{}' is absent", element.as_str()),
        )
    })?;
    match element_desc.abi {
        MirAbiClass::Integer {
            bits: 32 | 64,
            signed: true,
        } => Ok(1),
        MirAbiClass::Bool => Ok(3),
        abi => Err(NativeMirError::new(
            ty.as_str(),
            format!("List element ABI {abi:?} is outside the native scalar List ABI"),
        )),
    }
}

/// Return the one native payload type shared by a bounded built-in variant.
///
/// The physical representation is deliberately narrower than the general MIR
/// variant contract: `{ i8 discriminant, scalar payload }`.  The payload slot
/// is present even for a zero-field variant and is zero-filled by the emitter;
/// that keeps the LLVM ABI stable while making Option/Result shapes with
/// owned, nested, mixed, or unit payloads fail closed here.
fn native_copy_variant_payload_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<crate::core::ResolvedTypeId, NativeMirError> {
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "variant TypeDesc is absent"))?;
    let variants = match &desc.layout {
        MirLayout::Option { variants, .. } | MirLayout::Result { variants, .. } => variants,
        layout => {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!("layout {layout:?} is outside the flat Copy variant contract"),
            ))
        }
    };
    if desc.abi != MirAbiClass::Aggregate {
        return Err(NativeMirError::new(
            ty.as_str(),
            format!(
                "variant ABI {:?} is outside the flat Copy variant contract",
                desc.abi
            ),
        ));
    }
    if desc.ownership != MirOwnership::Copy {
        return Err(NativeMirError::new(
            ty.as_str(),
            format!(
                "variant ownership {:?} requires explicit native glue and is outside the flat Copy variant contract",
                desc.ownership
            ),
        ));
    }
    if desc.glue
        != (MirGlueContract {
            move_out: MirGlueKind::Noop,
            clone: MirGlueKind::Noop,
            drop: MirGlueKind::Noop,
        })
    {
        return Err(NativeMirError::new(
            ty.as_str(),
            "variant TypeDesc does not carry the canonical no-op glue contract",
        ));
    }
    if variants.is_empty() {
        return Err(NativeMirError::new(
            ty.as_str(),
            "variant TypeDesc has no variants in the flat Copy variant contract",
        ));
    }

    let mut discriminants = BTreeSet::new();
    let mut variant_ids = BTreeSet::new();
    let mut field_ids = BTreeSet::new();
    let mut payload_type: Option<crate::core::ResolvedTypeId> = None;
    for variant in variants {
        if !discriminants.insert(variant.discriminant) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant discriminant {} is duplicated in the flat Copy variant contract",
                    variant.discriminant
                ),
            ));
        }
        if variant.discriminant > u8::MAX as u16 {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant discriminant {} does not fit the native i8 tag contract",
                    variant.discriminant
                ),
            ));
        }
        if !variant_ids.insert(variant.id.clone()) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant identity '{}' is duplicated in the flat Copy variant contract",
                    variant.id.0
                ),
            ));
        }
        if variant.fields.len() > 1 {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant '{}' has {} payload fields; the flat Copy variant contract allows at most one",
                    variant.name,
                    variant.fields.len()
                ),
            ));
        }
        let Some(field) = variant.fields.first() else {
            continue;
        };
        if !field_ids.insert(field.id.clone()) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant payload field identity '{}' is duplicated in the flat Copy variant contract",
                    field.id.0
                ),
            ));
        }
        let field_desc = catalog.get(&field.ty).ok_or_else(|| {
            NativeMirError::new(
                ty.as_str(),
                format!("variant payload TypeDesc '{}' is absent", field.ty.as_str()),
            )
        })?;
        if !is_native_scalar_descriptor(field_desc) {
            return Err(NativeMirError::new(
                ty.as_str(),
                format!(
                    "variant '{}' payload ABI {:?}/layout {:?} is outside the flat Copy variant contract",
                    variant.name, field_desc.abi, field_desc.layout
                ),
            ));
        }
        if let Some(expected) = &payload_type {
            if expected != &field.ty {
                return Err(NativeMirError::new(
                    ty.as_str(),
                    format!(
                        "variant payload type '{}' disagrees with '{}'; mixed payload ABI is outside the flat Copy variant contract",
                        field.ty.as_str(),
                        expected.as_str()
                    ),
                ));
            }
        } else {
            payload_type = Some(field.ty.clone());
        }
    }
    payload_type.ok_or_else(|| {
        NativeMirError::new(
            ty.as_str(),
            "variant has no scalar payload; unit/zero-payload variants are outside the flat Copy variant contract",
        )
    })
}

fn native_basic_type<'ctx>(
    context: &'ctx Context,
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, NativeMirError> {
    let desc = catalog
        .get(ty)
        .ok_or_else(|| NativeMirError::new(ty.as_str(), "TypeDesc is absent"))?;
    match desc.abi {
        MirAbiClass::Integer {
            bits: 32,
            signed: true,
        } => Ok(context.i32_type().into()),
        MirAbiClass::Integer {
            bits: 64,
            signed: true,
        } => Ok(context.i64_type().into()),
        MirAbiClass::Bool => Ok(context.bool_type().into()),
        MirAbiClass::OpaqueHandle => match &desc.layout {
            MirLayout::List { .. } => Ok(context.ptr_type(inkwell::AddressSpace::default()).into()),
            layout => Err(NativeMirError::new(
                ty.as_str(),
                format!("opaque-handle layout {layout:?} is outside native contract"),
            )),
        },
        MirAbiClass::Aggregate => match &desc.layout {
            MirLayout::Record { fields, .. } if !fields.is_empty() => {
                let mut field_types = Vec::with_capacity(fields.len());
                for field in fields {
                    let field_desc = catalog.get(&field.ty).ok_or_else(|| {
                        NativeMirError::new(
                            field.name.clone(),
                            "record field TypeDesc is absent from native catalog",
                        )
                    })?;
                    if !is_native_scalar_descriptor(field_desc) {
                        return Err(NativeMirError::new(
                            field.name.clone(),
                            "record field is outside the flat Copy record ABI",
                        ));
                    }
                    field_types.push(native_basic_type(context, catalog, &field.ty)?);
                }
                Ok(context.struct_type(&field_types, false).into())
            }
            MirLayout::Option { .. } | MirLayout::Result { .. } => {
                let payload_ty = native_copy_variant_payload_type(catalog, ty)?;
                let payload = native_basic_type(context, catalog, &payload_ty)?;
                Ok(context
                    .struct_type(&[context.i8_type().into(), payload], false)
                    .into())
            }
            layout => Err(NativeMirError::new(
                ty.as_str(),
                format!("aggregate layout {layout:?} is outside native contract"),
            )),
        },
        MirAbiClass::Unit => Err(NativeMirError::new(
            ty.as_str(),
            "unit has no LLVM BasicType",
        )),
        abi => Err(NativeMirError::new(
            ty.as_str(),
            format!("ABI {abi:?} is outside native scalar contract"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::CodeGenerator;
    use crate::core::mir::reference::MirProgram;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use inkwell::context::Context;

    fn canonical_program(source: &str) -> MirProgram {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        MirProgram::from_checked_program(&checked).expect("canonical MIR")
    }

    #[test]
    fn native_validator_rejects_before_llvm_declarations() {
        let program = canonical_program("func main() -> f64 { 1.0 }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("unsupported MIR must fail before native emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("canonical MIR native backend rejected")
                && diagnostic.message.contains("ABI Float")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "L2 requires validation before LLVM function declarations"
        );
    }

    #[test]
    fn native_validator_rejects_non_copy_record_before_llvm_declarations() {
        let program = canonical_program(
            "type Box { text: string }\nfunc main() -> i32 { let value = Box { text: \"x\" }; drop(value); 0 }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_record_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("non-Copy record must fail closed in scalar native slice");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("flat Copy record contract")),
            "missing flat-record rejection: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("ownership Move")),
            "missing ownership rejection: {diagnostics:?}"
        );
        assert!(
            generator.module.get_function("main").is_none(),
            "non-Copy aggregate must be rejected before LLVM declarations"
        );
    }

    #[test]
    fn canonical_mir_rejects_non_scalar_list_before_any_backend() {
        let tokens = Lexer::new("func main() -> i32 { let values = [\"x\"]; drop(values); 0 }")
            .tokenize()
            .expect("lex");
        let file = Parser::new(tokens).parse_file().expect("parse");
        let checked = crate::core::check_program(&file).expect("check");
        let error = MirProgram::from_checked_program(&checked)
            .expect_err("non-scalar List must fail before any backend");
        let crate::core::mir::reference::MirProgramBuildError::Validation(errors) = error else {
            panic!("unexpected canonical List rejection: {error:?}");
        };
        assert!(errors
            .iter()
            .any(|error| error.message.contains("Copy scalar contract")));
    }

    #[test]
    fn native_emitter_accepts_scalar_list_projection_contract() {
        let program =
            canonical_program("func main() -> i32 { let values = [10, 20, 30]; values[1] }");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_list_emitter_test");

        generator
            .compile_mir_native(&program)
            .expect("scalar List MIR should have a native contract");
        assert!(generator.module.get_function("main").is_some());
        assert!(generator
            .module
            .get_function("mimi_mir_list_get_scalar")
            .is_some());
        generator
            .module
            .verify()
            .expect("native List module verifies");
    }

    #[test]
    fn native_validator_rejects_mixed_variant_payload_before_llvm_declarations() {
        let program = canonical_program(
            "func main() -> i64 { let value: Result<i64, bool> = Ok(41); match value { Ok(v) => v, Err(_) => (0 as i64) } }",
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "mir_native_variant_validator_test");

        let diagnostics = generator
            .compile_mir_native(&program)
            .expect_err("mixed variant payloads must fail before native emission");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("flat Copy variant contract")
                && diagnostic.message.contains("mixed payload ABI")
        }));
        assert!(
            generator.module.get_function("main").is_none(),
            "L2 requires variant validation before LLVM function declarations"
        );
    }
}
