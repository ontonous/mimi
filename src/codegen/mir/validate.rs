//! Canonical MIR native admission validator.
//!
//! Validation is complete before LLVM declarations are created.  This module
//! is intentionally the only owner of native shape eligibility.

use super::*;

pub(super) struct NativeMirValidator<'a> {
    program: &'a MirProgram,
    errors: Vec<NativeMirError>,
    symbols: BTreeSet<String>,
}

impl<'a> NativeMirValidator<'a> {
    pub(super) fn new(program: &'a MirProgram) -> Self {
        Self {
            program,
            errors: Vec::new(),
            symbols: BTreeSet::new(),
        }
    }

    pub(super) fn validate(mut self) -> Result<(), Vec<NativeMirError>> {
        for function in self.program.functions().values() {
            let symbol = match mir_symbol(&function.owner) {
                Ok(symbol) => symbol,
                Err(message) => {
                    self.errors
                        .push(NativeMirError::new(function.owner.0.clone(), message));
                    continue;
                }
            };
            if !self.symbols.insert(symbol.clone()) {
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
            self.reject_reference_callable_boundary(function, parameter, "reference parameter");
        }
        self.validate_signature_type(&function.result, "result", true);
        self.reject_reference_type(&function.result, "reference result");
        if crate::core::mir::is_owned_string_return_candidate(function, catalog) {
            if let Err(message) =
                crate::core::mir::validate_owned_string_return_shape(function, catalog)
            {
                self.errors
                    .push(NativeMirError::new(function.owner.0.clone(), message));
            }
        }

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
                    | crate::core::mir::MirOwnershipEventKind::BorrowMut
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

    fn reject_reference_callable_boundary(
        &mut self,
        function: &MirFunction,
        value: &MirValueId,
        subject: &str,
    ) {
        let Some(info) = function.values.get(value) else {
            return;
        };
        self.reject_reference_type(&info.ty, subject);
    }

    fn reject_reference_type(&mut self, ty: &crate::core::ResolvedTypeId, subject: &str) {
        if self
            .program
            .type_catalog()
            .get(ty)
            .is_some_and(|desc| matches!(&desc.kind, MirTypeKind::Reference { .. }))
        {
            self.errors.push(NativeMirError::new(
                subject,
                "borrowed pointer cannot cross the native callable ABI in the local-borrow contract",
            ));
        }
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
        _allow_unit_result: bool,
    ) {
        let Some(desc) = self.program.type_catalog().get(ty) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("type '{}' is absent from MIR TypeDesc catalog", ty.as_str()),
            ));
            return;
        };
        let is_list = matches!(desc.layout, MirLayout::List { .. });
        let is_set = matches!(desc.layout, MirLayout::Set { .. });
        let is_reference = matches!(&desc.kind, MirTypeKind::Reference { mutable: false });
        let is_owned_string = matches!(
            &desc.kind,
            MirTypeKind::Primitive(crate::core::PrimitiveType::String)
        );
        let is_record = matches!(desc.layout, MirLayout::Record { .. });
        let is_variant = matches!(
            desc.layout,
            MirLayout::Option { .. } | MirLayout::Result { .. }
        );
        let is_unit = desc.abi == MirAbiClass::Unit
            && desc.layout == MirLayout::Unit
            && desc.ownership == MirOwnership::Copy
            && desc.glue
                == (MirGlueContract {
                    move_out: MirGlueKind::Noop,
                    clone: MirGlueKind::Noop,
                    drop: MirGlueKind::Noop,
                });
        let supported = if is_reference {
            match self.program.type_catalog().validate_reference_type(ty) {
                Ok(_) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_owned_string {
            match self.program.type_catalog().validate_owned_string(ty) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_list {
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
        } else if is_set {
            match self
                .program
                .type_catalog()
                .validate_set_glue(ty, crate::core::mir::types::MirGlueOperation::MoveOut)
            {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if matches!(desc.layout, MirLayout::Tuple(_)) {
            match validate_native_recursive_tuple_type(self.program.type_catalog(), ty) {
                Ok(()) => true,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    false
                }
            }
        } else if is_record {
            if desc.ownership == MirOwnership::Copy {
                self.validate_flat_copy_record(ty, subject)
            } else {
                match validate_native_non_copy_record_type(self.program.type_catalog(), ty) {
                    Ok(()) => true,
                    Err(message) => {
                        self.errors.push(NativeMirError::new(subject, message));
                        false
                    }
                }
            }
        } else if is_variant {
            if desc.ownership == MirOwnership::Copy {
                self.validate_flat_copy_variant(ty, subject, desc)
            } else {
                match native_non_copy_variant_payload_type(self.program.type_catalog(), ty) {
                    Ok(_) => true,
                    Err(message) => {
                        let mut message = message;
                        message.subject = subject.to_owned();
                        self.errors.push(message);
                        false
                    }
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
                || is_unit
                || self.validate_flat_copy_record(ty, subject)
        };
        if !supported {
            let contract = if is_owned_string {
                "native canonical owned String contract"
            } else if is_list {
                "native canonical List contract"
            } else if is_set {
                "native canonical Set<T> contract"
            } else if matches!(desc.layout, MirLayout::Tuple(_)) {
                "native canonical recursive tuple contract"
            } else if is_record {
                "native canonical non-Copy record contract"
            } else if is_variant {
                if desc.ownership == MirOwnership::Copy {
                    "flat Copy variant contract"
                } else if matches!(desc.layout, MirLayout::Result { .. }) {
                    "native non-Copy Result<string, i32> variant contract"
                } else {
                    "native non-Copy Option<string> variant contract"
                }
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
        if !is_list
            && !is_set
            && !is_reference
            && !is_owned_string
            && !matches!(desc.layout, MirLayout::Tuple(_))
            && !is_record
            && !is_variant
        {
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
    ) -> bool {
        match self.program.type_catalog().validate_flat_copy_record(ty) {
            Ok(()) => true,
            Err(message) => {
                self.errors.push(NativeMirError::new(subject, message));
                false
            }
        }
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
                    (MirAbiClass::StringHandle, ResolvedLiteral::String(_)) => {
                        if let Err(message) = catalog.validate_owned_string(
                            &function.values.get(result).expect("validated result").ty,
                        ) {
                            self.errors.push(NativeMirError::new(subject, message));
                            false
                        } else {
                            true
                        }
                    }
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
                    if desc.ownership != MirOwnership::Copy
                        && matches!(instruction, MirInstructionKind::Copy { .. })
                    {
                        let message = if matches!(desc.layout, MirLayout::List { .. }) {
                            "List values require explicit Move or Clone glue; shallow Copy is not permitted"
                        } else if matches!(desc.layout, MirLayout::Set { .. }) {
                            "Set values require explicit Move or Clone glue; shallow Copy is not permitted"
                        } else if matches!(
                            &desc.kind,
                            MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                        ) {
                            "owned String values require explicit Move or Clone glue; shallow Copy is not permitted"
                        } else {
                            "non-Copy values require explicit Move or Clone glue; shallow Copy is not permitted"
                        };
                        self.errors.push(NativeMirError::new(subject, message));
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
                list_index_contract,
            } => self.validate_project(
                function,
                result,
                base,
                projection,
                list_index_contract.as_ref(),
                subject,
            ),
            MirInstructionKind::MoveProject {
                result,
                base,
                projection,
            } => self.validate_move_project(function, result, base, projection, subject),
            MirInstructionKind::VariantProject {
                result,
                base,
                contract,
            } => self.validate_variant_project(function, result, base, contract.as_ref(), subject),
            MirInstructionKind::Borrow {
                result,
                source,
                mutable,
            } => {
                self.validate_value(function, result, "borrow result");
                self.validate_value(function, source, "borrow source");
                let (Some(result_value), Some(source_value)) =
                    (function.values.get(result), function.values.get(source))
                else {
                    return;
                };
                if let Err(message) =
                    catalog.validate_borrow(&source_value.ty, &result_value.ty, *mutable)
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::EndBorrow { borrow } => {
                self.validate_value(function, borrow, "end-borrow value");
                let Some(value) = function.values.get(borrow) else {
                    return;
                };
                if let Err(message) = catalog.validate_reference_type(&value.ty) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
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
                        if !matches!(desc.layout, MirLayout::List { .. })
                            && !matches!(desc.layout, MirLayout::Set { .. })
                            && !matches!(desc.layout, MirLayout::Tuple(_))
                            && !matches!(desc.layout, MirLayout::Record { .. })
                            && !matches!(
                                desc.layout,
                                MirLayout::Option { .. } | MirLayout::Result { .. }
                            )
                            && !matches!(
                                &desc.kind,
                                MirTypeKind::Primitive(crate::core::PrimitiveType::String)
                            )
                        {
                            self.errors.push(NativeMirError::new(
                                subject,
                                "only canonical owned String/List/Set/tuple/variant drop glue is emitted by this native slice",
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
            MirInstructionKind::UpdateRecord {
                result,
                base,
                kind,
                fields,
            } => self.validate_update_record(function, result, base, kind, fields, subject),
            MirInstructionKind::ConstructVariant {
                result,
                nominal,
                variant,
                fields,
            } => {
                self.validate_construct_variant(function, result, nominal, variant, fields, subject)
            }
            MirInstructionKind::ConstructVariantMove {
                result,
                nominal,
                variant,
                fields,
            } => self.validate_construct_variant_move(
                function, result, nominal, variant, fields, subject,
            ),
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
            MirInstructionKind::ListOp {
                result,
                operation,
                list,
                argument,
                list_operation_contract,
            } => {
                self.validate_value(function, result, "List operation result");
                self.validate_value(function, list, "List operation receiver");
                if let Some(argument) = argument {
                    self.validate_value(function, argument, "List operation argument");
                }
                let (Some(result_value), Some(list_value)) =
                    (function.values.get(result), function.values.get(list))
                else {
                    return;
                };
                let Some(receipt) = list_operation_contract.as_ref() else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List operation has no canonical receipt",
                    ));
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| value.ty.clone());
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_operation_receipt_with_argument(
                        &result_value.ty,
                        &list_value.ty,
                        argument_ty.as_ref(),
                        *operation,
                        receipt,
                    )
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::VariantPredicate {
                result,
                predicate,
                variant,
                contract,
            } => {
                self.validate_value(function, result, "variant predicate result");
                self.validate_value(function, variant, "variant predicate source");
                let (Some(result_value), Some(variant_value)) =
                    (function.values.get(result), function.values.get(variant))
                else {
                    return;
                };
                let Some(receipt) = contract.as_ref() else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "variant predicate has no canonical receipt",
                    ));
                    return;
                };
                if let Err(message) = catalog.validate_variant_predicate_receipt(
                    &result_value.ty,
                    &variant_value.ty,
                    *predicate,
                    receipt,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if let Err(message) = catalog.validate_flat_copy_variant(&variant_value.ty) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirInstructionKind::ConstructSet { result, elements } => {
                self.validate_value(function, result, "Set result");
                let element_types = elements
                    .iter()
                    .filter_map(|element| {
                        function.values.get(element).map(|value| value.ty.clone())
                    })
                    .collect::<Vec<_>>();
                if element_types.len() != elements.len() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "Set element is absent from MIR value catalog",
                    ));
                } else if let Some(result_value) = function.values.get(result) {
                    if let Err(message) =
                        catalog.validate_set_construct(&result_value.ty, &element_types)
                    {
                        self.errors.push(NativeMirError::new(subject, message));
                    }
                }
            }
            MirInstructionKind::SetOp {
                result,
                operation,
                set,
                argument,
            } => {
                self.validate_value(function, result, "Set operation result");
                self.validate_value(function, set, "Set operation receiver");
                if let Some(argument) = argument {
                    self.validate_value(function, argument, "Set operation argument");
                }
                let (Some(result_value), Some(set_value)) =
                    (function.values.get(result), function.values.get(set))
                else {
                    return;
                };
                let argument_ty = argument
                    .as_ref()
                    .and_then(|value| function.values.get(value))
                    .map(|value| &value.ty);
                if let Err(message) = catalog.validate_set_operation(
                    &result_value.ty,
                    &set_value.ty,
                    argument_ty,
                    *operation,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
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
                    MirBuiltinKind::Abs
                        | MirBuiltinKind::Min
                        | MirBuiltinKind::Max
                        | MirBuiltinKind::PrintlnBool
                        | MirBuiltinKind::PrintlnInt
                );
                if !supported_kind {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "builtin kind is not in native MIR contract",
                    ));
                }
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(desc) = function
                        .values
                        .get(argument)
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
                                "builtin '{}' argument {index} TypeDesc/ABI is outside native scalar contract",
                                contract.name,
                            ),
                        ));
                    }
                }
                let Some(result_desc) = function
                    .values
                    .get(result)
                    .and_then(|value| self.program.type_catalog().get(&value.ty))
                else {
                    return;
                };
                let valid_result = if contract.result_must_be_unit {
                    result_desc.layout == MirLayout::Unit
                        && result_desc.abi == MirAbiClass::Unit
                        && result_desc.ownership == MirOwnership::Copy
                        && result_desc.glue
                            == (MirGlueContract {
                                move_out: MirGlueKind::Noop,
                                clone: MirGlueKind::Noop,
                                drop: MirGlueKind::Noop,
                            })
                } else {
                    contract.accepts_abi(result_desc.abi)
                        && contract.accepts_layout(&result_desc.layout)
                        && result_desc.ownership == MirOwnership::Copy
                        && (!matches!(
                            kind,
                            MirBuiltinKind::Abs | MirBuiltinKind::Min | MirBuiltinKind::Max
                        ) || result_desc.abi
                            == MirAbiClass::Integer {
                                bits: 64,
                                signed: true,
                            })
                };
                if !valid_result {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "builtin '{}' result TypeDesc/ABI is outside native scalar contract",
                            contract.name
                        ),
                    ));
                }
            }
            MirInstructionKind::Call {
                result,
                callee,
                type_arguments,
                arguments,
                variant_call_contract,
            } => self.validate_call(
                function,
                result.as_ref(),
                callee,
                type_arguments,
                arguments,
                variant_call_contract.as_ref(),
                subject,
            ),
            MirInstructionKind::FlowTransition {
                result,
                transition,
                arguments,
            } => self.validate_flow_transition(function, result, transition, arguments, subject),
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
        list_index_contract: Option<&crate::core::mir::types::MirListIndexProjectionContract>,
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
            MirProjection::Tuple(index) => {
                if let Err(message) = self.program.type_catalog().validate_projection(
                    &base_value.ty,
                    &result_value.ty,
                    projection,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if *index > u32::MAX as usize {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "tuple projection index exceeds native aggregate ABI",
                    ));
                }
            }
            MirProjection::Index(index) => {
                let Some(index_value) = function.values.get(index) else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index is absent from MIR value catalog",
                    ));
                    return;
                };
                let Some(receipt) = list_index_contract else {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index projection has no canonical receipt",
                    ));
                    return;
                };
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_list_index_projection_receipt(
                        &base_value.ty,
                        &index_value.ty,
                        &result_value.ty,
                        receipt,
                    )
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
            MirProjection::Field(_) => {
                if list_index_contract.is_some() {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "List index receipt is attached to a non-index projection",
                    ));
                    return;
                }
                if let Err(message) = self.program.type_catalog().validate_projection(
                    &base_value.ty,
                    &result_value.ty,
                    projection,
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
                if self
                    .program
                    .type_catalog()
                    .get(&base_value.ty)
                    .is_some_and(|desc| {
                        desc.ownership != MirOwnership::Copy
                            && self
                                .program
                                .type_catalog()
                                .get(&result_value.ty)
                                .is_some_and(|result| result.ownership != MirOwnership::Copy)
                    })
                {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "non-Copy record field projection requires an explicit MoveProject contract",
                    ));
                }
            }
            MirProjection::Dereference => {
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_dereference(&base_value.ty, &result_value.ty)
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        }
    }

    fn validate_move_project(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        projection: &MirProjection,
        subject: &str,
    ) {
        self.validate_value(function, result, "move projection result");
        self.validate_value(function, base, "move projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        if let Err(message) = self.program.type_catalog().validate_move_projection(
            &base_value.ty,
            &result_value.ty,
            projection,
        ) {
            self.errors.push(NativeMirError::new(subject, message));
            return;
        }
        if let Err(message) =
            validate_native_non_copy_record_type(self.program.type_catalog(), &base_value.ty)
        {
            self.errors.push(NativeMirError::new(subject, message));
        }
    }

    fn validate_variant_project(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        contract: Option<&crate::core::mir::types::MirVariantProjectionTrapContract>,
        subject: &str,
    ) {
        self.validate_value(function, result, "variant projection result");
        self.validate_value(function, base, "variant projection base");
        let (Some(base_value), Some(result_value)) =
            (function.values.get(base), function.values.get(result))
        else {
            return;
        };
        let Some(contract) = contract else {
            self.errors.push(NativeMirError::new(
                subject,
                "direct variant projection has no canonical trap receipt",
            ));
            return;
        };
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_variant_projection_trap_receipt(&base_value.ty, &result_value.ty, contract)
        {
            self.errors.push(NativeMirError::new(subject, message));
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
        self.validate_value(function, result, "aggregate result");
        for field in fields {
            self.validate_value(function, field, "aggregate field value");
        }
        if !matches!(
            kind,
            MirAggregateKind::Record { .. } | MirAggregateKind::Tuple
        ) {
            self.errors.push(NativeMirError::new(
                subject,
                "aggregate construction is outside the native MIR aggregate contract",
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

    fn validate_update_record(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        base: &MirValueId,
        kind: &MirAggregateKind,
        fields: &[MirValueId],
        subject: &str,
    ) {
        self.validate_value(function, result, "record update result");
        self.validate_value(function, base, "record update base");
        for field in fields {
            self.validate_value(function, field, "record update field value");
        }
        let (Some(result_value), Some(base_value)) =
            (function.values.get(result), function.values.get(base))
        else {
            return;
        };
        if self
            .program
            .type_catalog()
            .get(&result_value.ty)
            .is_some_and(|desc| desc.ownership != MirOwnership::Copy)
        {
            self.errors.push(NativeMirError::new(
                subject,
                "non-Copy record update requires an explicit transfer/update contract",
            ));
            return;
        }
        if result_value.ty != base_value.ty {
            self.errors.push(NativeMirError::new(
                subject,
                "record update base and result types disagree",
            ));
            return;
        }
        let field_types = fields
            .iter()
            .filter_map(|field| function.values.get(field).map(|value| value.ty.clone()))
            .collect::<Vec<_>>();

        if field_types.len() != fields.len() {
            return;
        }
        if let Err(message) = self.program.type_catalog().validate_record_update(
            &result_value.ty,
            &base_value.ty,
            kind,
            &field_types,
        ) {
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

    fn validate_construct_variant_move(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        nominal: &crate::core::NominalTypeId,
        variant: &crate::core::NodeId,
        fields: &[(crate::core::NodeId, MirValueId)],
        subject: &str,
    ) {
        self.validate_value(function, result, "move variant result");
        for (_, value) in fields {
            self.validate_value(function, value, "move variant payload value");
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
            native_non_copy_variant_payload_type(self.program.type_catalog(), &result_value.ty)
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
            let variant_id = match &arm.case {
                MirSwitchCase::Variant(variant) => Some(variant),
                MirSwitchCase::Default | MirSwitchCase::Literal(_) => None,
            };
            for (index, binding) in arm.bindings.iter().enumerate() {
                if !binding_fields.insert(binding.projection.field.clone()) {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "switch payload field '{}' is bound more than once",
                            binding.projection.field.0
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
                let Some(variant_id) = variant_id else {
                    continue;
                };
                if binding.parameter != target.parameters[arm.arguments.len() + index].value {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch binding parameter disagrees with target block parameter",
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_flat_copy_payload_projection_receipt(
                        &scrutinee_value.ty,
                        variant_id,
                        &parameter.ty,
                        &binding.projection,
                    )
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        }
    }

    fn validate_switch_move(
        &mut self,
        function: &MirFunction,
        scrutinee: &MirValueId,
        arms: &[MirSwitchArm],
        subject: &str,
    ) {
        self.validate_value(function, scrutinee, "switch-move scrutinee");
        let Some(scrutinee_value) = function.values.get(scrutinee) else {
            return;
        };
        if let Err(message) =
            native_non_copy_variant_payload_type(self.program.type_catalog(), &scrutinee_value.ty)
        {
            let mut message = message;
            message.subject = subject.to_owned();
            self.errors.push(message);
            return;
        }
        if let Err(message) = self
            .program
            .type_catalog()
            .validate_variant_switch_move_contract(&scrutinee_value.ty, arms)
        {
            self.errors.push(NativeMirError::new(subject, message));
            return;
        }

        let variants = match self
            .program
            .type_catalog()
            .variant_layout(&scrutinee_value.ty)
        {
            Some((_, variants)) => variants,
            None => {
                self.errors.push(NativeMirError::new(
                    subject,
                    "native non-Copy SwitchMove has no canonical variant layout",
                ));
                return;
            }
        };
        let required = variants
            .iter()
            .map(|variant| variant.id.clone())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        if arms.len() != required.len() {
            self.errors.push(NativeMirError::new(
                subject,
                "native non-Copy SwitchMove requires exactly one explicit arm for each TypeDesc variant",
            ));
        }

        for arm in arms {
            let MirSwitchCase::Variant(variant_id) = &arm.case else {
                self.errors.push(NativeMirError::new(
                    subject,
                    "native non-Copy SwitchMove requires explicit variant arms; default/literal cases are not covered",
                ));
                continue;
            };
            let variant = match self
                .program
                .type_catalog()
                .validated_variant_switch_case(&scrutinee_value.ty, variant_id)
            {
                Ok((_, variant)) => variant,
                Err(message) => {
                    self.errors.push(NativeMirError::new(subject, message));
                    continue;
                }
            };
            if !seen.insert(variant.id.clone()) {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("switch-move variant '{}' is repeated", variant.name),
                ));
            }
            let Some(target) = function.blocks.get(&arm.target) else {
                self.errors.push(NativeMirError::new(
                    subject,
                    format!("switch-move edge target '{}' is absent", arm.target),
                ));
                continue;
            };
            for (index, argument) in arm.arguments.iter().enumerate() {
                self.validate_value(function, argument, "switch-move edge argument");
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
                        "switch-move edge argument type disagrees with block parameter",
                    ));
                }
            }
            if target.parameters.len() != arm.arguments.len() + arm.bindings.len() {
                self.errors.push(NativeMirError::new(
                    subject,
                    "switch-move edge arguments and payload bindings disagree with block parameter arity",
                ));
            }
            // The native non-Copy TypeDesc gate has already proved the
            // complete admitted variant shape. Only this edge's own
            // single-binding physical shape remains to be checked here.
            if arm.bindings.len() > 1 {
                self.errors.push(NativeMirError::new(
                    subject,
                    "native non-Copy SwitchMove supports at most one payload field and one binding",
                ));
            }
            let mut binding_fields = BTreeSet::new();
            for (index, binding) in arm.bindings.iter().enumerate() {
                if !binding_fields.insert(binding.projection.field.clone()) {
                    self.errors.push(NativeMirError::new(
                        subject,
                        format!(
                            "switch-move payload field '{}' is bound more than once",
                            binding.projection.field.0
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
                if binding.parameter != target.parameters[arm.arguments.len() + index].value {
                    self.errors.push(NativeMirError::new(
                        subject,
                        "switch-move binding parameter disagrees with target block parameter",
                    ));
                }
                if let Err(message) = self
                    .program
                    .type_catalog()
                    .validate_variant_payload_projection_receipt(
                        &scrutinee_value.ty,
                        variant_id,
                        &parameter.ty,
                        &binding.projection,
                    )
                {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        }
        if seen != required {
            self.errors.push(NativeMirError::new(
                subject,
                "native non-Copy SwitchMove does not cover exactly the canonical TypeDesc variants",
            ));
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
        type_arguments: &[crate::core::ResolvedTypeId],
        arguments: &[MirValueId],
        variant_call_contract: Option<&crate::core::mir::types::MirVariantCallAbiContract>,
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
        let parameter_types = target
            .parameters
            .iter()
            .filter_map(|parameter| target.values.get(parameter))
            .map(|value| value.ty.clone())
            .collect::<Vec<_>>();
        let flat_variant_result = self
            .program
            .type_catalog()
            .validate_flat_copy_variant(&target.result)
            .is_ok();
        let move_owned_result = self
            .program
            .type_catalog()
            .validate_result_string_i32_variant(&target.result)
            .is_ok();
        if flat_variant_result || move_owned_result {
            let Some(receipt) = variant_call_contract else {
                self.errors.push(NativeMirError::new(
                    subject,
                    if flat_variant_result {
                        "call returning flat Copy Option/Result has no canonical ABI receipt"
                    } else {
                        "call returning move-owned Result<string, i32> has no canonical ABI receipt"
                    },
                ));
                return;
            };
            if let Err(message) = self
                .program
                .type_catalog()
                .validate_variant_call_abi_receipt(
                    owner,
                    type_arguments,
                    &parameter_types,
                    &target.result,
                    receipt,
                )
            {
                self.errors.push(NativeMirError::new(subject, message));
            }
            if move_owned_result {
                if let Err(message) = crate::core::mir::validate_move_owned_result_return_merge(
                    target,
                    self.program.type_catalog(),
                ) {
                    self.errors.push(NativeMirError::new(subject, message));
                }
            }
        } else if variant_call_contract.is_some() {
            self.errors.push(NativeMirError::new(
                subject,
                "variant call ABI receipt is attached to an unsupported variant result",
            ));
        } else if self
            .program
            .type_catalog()
            .get(&target.result)
            .is_some_and(|descriptor| {
                descriptor.kind == MirTypeKind::Result && descriptor.ownership != MirOwnership::Copy
            })
        {
            self.errors.push(NativeMirError::new(
                subject,
                "non-Copy Result call result is outside the canonical call ABI contract",
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

    fn validate_flow_transition(
        &mut self,
        function: &MirFunction,
        result: &MirValueId,
        transition: &crate::core::NodeId,
        arguments: &[MirValueId],
        subject: &str,
    ) {
        let Some(contract) = self.program.transitions().get(transition) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!(
                    "flow transition '{}' has no canonical contract",
                    transition.0
                ),
            ));
            return;
        };
        if contract.effect != crate::core::mir::MirTransitionEffect::SilentLocal
            || contract.targets.len() != 1
            || contract.failure.is_some()
            || contract.is_fallback
            || contract.is_ffi_pinned
            || contract.targets.first() != Some(&contract.result)
        {
            self.errors.push(NativeMirError::new(
                subject,
                "FlowTransition is outside the silent-local native contract",
            ));
        }
        let Some(target) = self.program.functions().get(&contract.owner) else {
            self.errors.push(NativeMirError::new(
                subject,
                format!("flow transition '{}' target body is absent", transition.0),
            ));
            return;
        };
        if arguments.len() != target.parameters.len() {
            self.errors.push(NativeMirError::new(
                subject,
                "FlowTransition argument arity disagrees with its canonical body",
            ));
        }
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            self.validate_value(function, argument, "FlowTransition argument");
            let Some(argument) = function.values.get(argument) else {
                continue;
            };
            let Some(parameter) = target.values.get(parameter) else {
                continue;
            };
            if argument.ty != parameter.ty {
                self.errors.push(NativeMirError::new(
                    subject,
                    "FlowTransition argument TypeDesc disagrees with its canonical body",
                ));
            }
        }
        self.validate_value(function, result, "FlowTransition result");
        if function
            .values
            .get(result)
            .is_some_and(|value| value.ty != contract.result || value.ty != target.result)
        {
            self.errors.push(NativeMirError::new(
                subject,
                "FlowTransition result TypeDesc disagrees with its canonical contract",
            ));
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
            MirTerminator::SwitchMove { scrutinee, arms } => {
                self.validate_switch_move(function, scrutinee, arms, &subject);
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
