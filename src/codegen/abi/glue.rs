//! Derived value clone/drop glue for the native backend.
//!
//! A glue plan is produced only from [`OwnershipClass`].  Expression shape,
//! source spelling and LLVM pointer heuristics are deliberately absent here.
//! The LLVM module is the registry: deterministic internal symbol names make
//! repeated requests idempotent without another mutable cache.

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};

use crate::codegen::abi::layout::builtin_variant;
use crate::codegen::abi::ownership::{LinearOwnershipKind, OwnershipClass};
use crate::codegen::{call_try_basic_value, CodeGenerator};
use crate::error::CompileError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::codegen) enum GluePlan {
    Trivial,
    StringBox,
    List(Box<GluePlan>),
    Option(Box<GluePlan>),
    Result {
        ok: Box<GluePlan>,
        error: Box<GluePlan>,
    },
    Tuple(Vec<GluePlan>),
    Record(Vec<GluePlan>),
    Array(Box<GluePlan>),
    OpaqueNoop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::codegen) enum GlueDeriveError {
    Linear(LinearOwnershipKind),
    Generic,
    Unknown,
    Unsupported(&'static str),
}

impl std::fmt::Display for GlueDeriveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Linear(kind) => write!(formatter, "linear value ({kind:?})"),
            Self::Generic => formatter.write_str("uninstantiated generic value"),
            Self::Unknown => formatter.write_str("unknown ownership shape"),
            Self::Unsupported(kind) => write!(formatter, "unsupported ownership class {kind}"),
        }
    }
}

impl GluePlan {
    pub(in crate::codegen) fn derive(class: &OwnershipClass) -> Result<Self, GlueDeriveError> {
        Ok(match class {
            OwnershipClass::Scalar => Self::Trivial,
            OwnershipClass::StringBox => Self::StringBox,
            OwnershipClass::List(element) => Self::List(Box::new(Self::derive(element)?)),
            OwnershipClass::Option(payload) => Self::Option(Box::new(Self::derive(payload)?)),
            OwnershipClass::Result { ok, error } => Self::Result {
                ok: Box::new(Self::derive(ok)?),
                error: Box::new(Self::derive(error)?),
            },
            OwnershipClass::Tuple(fields) => Self::Tuple(
                fields
                    .iter()
                    .map(Self::derive)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            OwnershipClass::Record(fields) => Self::Record(
                fields
                    .iter()
                    .map(Self::derive)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            OwnershipClass::Union(_) => {
                return Err(GlueDeriveError::Unsupported("union"));
            }
            OwnershipClass::Array(element) => Self::Array(Box::new(Self::derive(element)?)),
            OwnershipClass::OpaqueHandle(_) => Self::OpaqueNoop,
            OwnershipClass::Linear { kind, .. } => return Err(GlueDeriveError::Linear(*kind)),
            OwnershipClass::Generic => return Err(GlueDeriveError::Generic),
            OwnershipClass::Unknown => return Err(GlueDeriveError::Unknown),
            OwnershipClass::Slice(_) => return Err(GlueDeriveError::Unsupported("slice")),
            OwnershipClass::Shared(_) => return Err(GlueDeriveError::Unsupported("shared/weak")),
            OwnershipClass::Closure => return Err(GlueDeriveError::Unsupported("closure")),
            OwnershipClass::DynamicObject => {
                return Err(GlueDeriveError::Unsupported("dynamic object"))
            }
        })
    }

    fn symbol_suffix(&self) -> String {
        match self {
            Self::Trivial => "scalar".into(),
            Self::StringBox => "string".into(),
            Self::List(element) => format!("list_{}", element.symbol_suffix()),
            Self::Option(payload) => format!("option_{}", payload.symbol_suffix()),
            Self::Result { ok, error } => {
                format!("result_{}_{}", ok.symbol_suffix(), error.symbol_suffix())
            }
            Self::Tuple(fields) => product_suffix("tuple", fields),
            Self::Record(fields) => product_suffix("record", fields),
            Self::Array(element) => format!("array_{}", element.symbol_suffix()),
            Self::OpaqueNoop => "opaque".into(),
        }
    }

    /// Whether this slice has an emitter for the entire shape.  Lists,
    /// variants, arrays, and opaque handles already have plans, but remain
    /// closed until their element/tag/runtime-handle contracts land with
    /// dedicated memory tests.
    fn is_emitted(&self) -> bool {
        match self {
            Self::Trivial | Self::StringBox => true,
            Self::Tuple(fields) | Self::Record(fields) => fields.iter().all(Self::is_emitted),
            Self::Option(payload) => payload.is_emitted(),
            Self::Result { ok, error } => {
                ok.is_emitted() && error.is_emitted_result_error_storage()
            }
            Self::List(_) | Self::Array(_) | Self::OpaqueNoop => false,
        }
    }

    /// The native Result ABI deliberately erases `E` into an i64 slot.  This
    /// slice supports the two storage contracts that can be proven without
    /// recovering source spelling: scalar bits inline and StringBox behind an
    /// owned `{ptr,len}` heap handle.  Product/list errors remain closed until
    /// their erased storage descriptor is carried by canonical ABI metadata.
    fn is_emitted_result_error_storage(&self) -> bool {
        matches!(self, Self::Trivial | Self::StringBox)
    }

    fn owns_heap(&self) -> bool {
        match self {
            Self::StringBox => true,
            Self::Tuple(fields) | Self::Record(fields) => fields.iter().any(Self::owns_heap),
            Self::List(_) | Self::Array(_) => true,
            Self::Option(payload) => payload.owns_heap(),
            Self::Result { ok, error } => ok.owns_heap() || error.owns_heap(),
            Self::Trivial | Self::OpaqueNoop => false,
        }
    }

    fn is_adoptable_return(&self) -> bool {
        self.is_emitted()
            && self.owns_heap()
            && matches!(
                self,
                Self::StringBox
                    | Self::Option(_)
                    | Self::Result { .. }
                    | Self::Tuple(_)
                    | Self::Record(_)
            )
    }
}

fn product_suffix(prefix: &str, fields: &[GluePlan]) -> String {
    let mut suffix = format!("{prefix}{}", fields.len());
    for field in fields {
        let child = field.symbol_suffix();
        suffix.push('_');
        suffix.push_str(&child.len().to_string());
        suffix.push('_');
        suffix.push_str(&child);
    }
    suffix
}

#[derive(Clone, Copy)]
struct GluePair<'ctx> {
    clone: FunctionValue<'ctx>,
    #[allow(dead_code)]
    drop: FunctionValue<'ctx>,
}

fn llvm_type_symbol_suffix(ty: BasicTypeEnum<'_>) -> String {
    // LLVM's own textual type is canonical within a module and captures every
    // ABI-relevant scalar width, packing bit, array length, and nested field.
    // Hex encoding makes it a collision-free symbol component without relying
    // on randomized Rust hashes or source-level type names.
    ty.print_to_string()
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn glue_shape_error(plan: &GluePlan, ty: BasicTypeEnum<'_>, detail: &str) -> CompileError {
    CompileError::Unsupported(format!(
        "derived value glue `{}` does not match LLVM type `{}`: {detail}",
        plan.symbol_suffix(),
        ty.print_to_string()
    ))
}

fn validate_plan_type(plan: &GluePlan, ty: BasicTypeEnum<'_>) -> Result<(), CompileError> {
    match plan {
        GluePlan::Trivial => match ty {
            BasicTypeEnum::IntType(_)
            | BasicTypeEnum::FloatType(_)
            | BasicTypeEnum::PointerType(_)
            | BasicTypeEnum::VectorType(_)
            | BasicTypeEnum::ScalableVectorType(_) => Ok(()),
            _ => Err(glue_shape_error(
                plan,
                ty,
                "a trivial ownership leaf must use a scalar LLVM ABI",
            )),
        },
        GluePlan::StringBox => {
            let BasicTypeEnum::StructType(string) = ty else {
                return Err(glue_shape_error(
                    plan,
                    ty,
                    "StringBox requires a struct ABI",
                ));
            };
            let fields = string.get_field_types();
            if fields.len() == 2
                && matches!(fields[0], BasicTypeEnum::PointerType(_))
                && matches!(fields[1], BasicTypeEnum::IntType(integer) if integer.get_bit_width() == 64)
            {
                Ok(())
            } else {
                Err(glue_shape_error(
                    plan,
                    ty,
                    "StringBox requires the canonical {ptr, i64} ABI",
                ))
            }
        }
        GluePlan::Option(payload) => {
            let BasicTypeEnum::StructType(option) = ty else {
                return Err(glue_shape_error(
                    plan,
                    ty,
                    "Option glue requires a by-value struct ABI",
                ));
            };
            let fields = option.get_field_types();
            if fields.len() != 2
                || !matches!(fields[0], BasicTypeEnum::IntType(tag) if tag.get_bit_width() == 1)
            {
                return Err(glue_shape_error(
                    plan,
                    ty,
                    "Option requires the canonical {i1, payload} ABI",
                ));
            }
            validate_plan_type(payload, fields[1])
        }
        GluePlan::Result { ok, error } => {
            let BasicTypeEnum::StructType(result) = ty else {
                return Err(glue_shape_error(
                    plan,
                    ty,
                    "Result glue requires a by-value struct ABI",
                ));
            };
            let fields = result.get_field_types();
            if fields.len() != 3
                || !matches!(fields[0], BasicTypeEnum::IntType(tag) if tag.get_bit_width() == 1)
            {
                return Err(glue_shape_error(
                    plan,
                    ty,
                    "Result requires the canonical {i1, ok, i64-error-handle} ABI",
                ));
            }
            validate_plan_type(ok, fields[1])?;
            validate_result_error_storage(error, fields[2], plan, ty)
        }
        GluePlan::Tuple(fields) | GluePlan::Record(fields) => {
            let BasicTypeEnum::StructType(product) = ty else {
                return Err(glue_shape_error(
                    plan,
                    ty,
                    "product glue requires a by-value struct ABI",
                ));
            };
            let llvm_fields = product.get_field_types();
            if fields.len() != llvm_fields.len() {
                return Err(glue_shape_error(
                    plan,
                    ty,
                    "ownership and LLVM product arities differ",
                ));
            }
            for (field, llvm_field) in fields.iter().zip(llvm_fields) {
                validate_plan_type(field, llvm_field)?;
            }
            Ok(())
        }
        _ => Err(glue_shape_error(
            plan,
            ty,
            "this ownership shape has no emitter in the current slice",
        )),
    }
}

fn validate_result_error_storage(
    error: &GluePlan,
    storage: BasicTypeEnum<'_>,
    parent: &GluePlan,
    parent_ty: BasicTypeEnum<'_>,
) -> Result<(), CompileError> {
    let is_i64 =
        matches!(storage, BasicTypeEnum::IntType(integer) if integer.get_bit_width() == 64);
    if !is_i64 {
        return Err(glue_shape_error(
            parent,
            parent_ty,
            "Result error storage must be the canonical erased i64 slot",
        ));
    }
    match error {
        GluePlan::Trivial | GluePlan::StringBox => Ok(()),
        _ => Err(glue_shape_error(
            parent,
            parent_ty,
            "the erased Result error shape has no payload-storage bridge in the current slice",
        )),
    }
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Clone a value through glue derived from the canonical ownership class
    /// and the concrete LLVM ABI as a pair.  The ABI half is essential for
    /// products: `(i32, string)` and `(i64, string)` have the same ownership
    /// shape but require different function signatures.
    pub(in crate::codegen) fn clone_value_with_derived_glue(
        &self,
        class: &OwnershipClass,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let plan = GluePlan::derive(class).map_err(|error| {
            CompileError::Unsupported(format!("cannot derive value glue: {error}"))
        })?;
        let ty = value.get_type();
        let pair = self.ensure_value_glue(&plan, ty)?;
        let call = self.build_call(
            pair.clone,
            &[BasicMetadataValueEnum::from(value)],
            "value_clone_glue",
        )?;
        call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("value clone glue returned void".into()))
    }

    /// Check whether this exact ownership/ABI pair is covered by the adopted
    /// return slice. Unsupported categories return false so their old,
    /// separately-gated path remains intact. A covered category with a shape
    /// mismatch is a hard error rather than a silent fallback.
    pub(in crate::codegen) fn value_glue_can_adopt_return(
        &self,
        class: &OwnershipClass,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<bool, CompileError> {
        if !self.value_glue_enabled() {
            return Ok(false);
        }
        let Ok(plan) = GluePlan::derive(class) else {
            return Ok(false);
        };
        if !plan.is_adoptable_return() {
            return Ok(false);
        }
        validate_plan_type(&plan, ty)?;
        Ok(true)
    }

    /// Clone an adopted return, yielding `None` when the category intentionally
    /// stays on the legacy ownership path.
    pub(in crate::codegen) fn clone_return_with_derived_glue(
        &self,
        class: &OwnershipClass,
        value: BasicValueEnum<'ctx>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        if !self.value_glue_can_adopt_return(class, value.get_type())? {
            return Ok(None);
        }
        self.clone_value_with_derived_glue(class, value).map(Some)
    }

    /// Register the heap leaves of a freshly-cloned return in the caller's
    /// active scope. This mirrors clone derivation from the same plan, so the
    /// caller cannot accidentally free a different set of fields.
    pub(in crate::codegen) fn register_returned_value_with_derived_glue(
        &self,
        class: &OwnershipClass,
        value: BasicValueEnum<'ctx>,
    ) -> Result<bool, CompileError> {
        if !self.value_glue_can_adopt_return(class, value.get_type())? {
            return Ok(false);
        }
        let plan = GluePlan::derive(class).map_err(|error| {
            CompileError::Unsupported(format!("cannot derive returned-value tracking: {error}"))
        })?;
        self.register_returned_plan(&plan, value, value.get_type())?;
        Ok(true)
    }

    fn register_returned_plan(
        &self,
        plan: &GluePlan,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<(), CompileError> {
        validate_plan_type(plan, ty)?;
        match plan {
            GluePlan::Trivial => Ok(()),
            GluePlan::StringBox => {
                let BasicValueEnum::StructValue(value) = value else {
                    return Err(glue_shape_error(
                        plan,
                        ty,
                        "returned StringBox tracking requires a struct",
                    ));
                };
                let data = self
                    .build_extract_value(value.into(), 0, "returned_glue_string_data")?
                    .into_pointer_value();
                self.register_heap_alloc(data);
                Ok(())
            }
            GluePlan::Option(payload) => {
                let BasicTypeEnum::StructType(option) = ty else {
                    return Err(glue_shape_error(
                        plan,
                        ty,
                        "returned Option tracking requires a struct ABI",
                    ));
                };
                let BasicValueEnum::StructValue(option_value) = value else {
                    return Err(glue_shape_error(
                        plan,
                        ty,
                        "returned Option tracking requires a struct value",
                    ));
                };
                // Variant clone glue canonicalizes an inactive payload to zero,
                // so tracking can remain branch-free: None recursively
                // registers only null pointers, while Some registers its fresh
                // payload leaves. This keeps every cleanup operand dominated.
                let payload_ty =
                    option.get_field_types()[builtin_variant::OPTION_PAYLOAD_FIELD as usize];
                let payload_value = self.build_extract_value(
                    option_value.into(),
                    builtin_variant::OPTION_PAYLOAD_FIELD,
                    "returned_glue_option_payload",
                )?;
                self.register_returned_plan(payload, payload_value, payload_ty)
            }
            GluePlan::Result { ok, error } => {
                let BasicTypeEnum::StructType(result) = ty else {
                    return Err(glue_shape_error(
                        plan,
                        ty,
                        "returned Result tracking requires a struct ABI",
                    ));
                };
                let BasicValueEnum::StructValue(result_value) = value else {
                    return Err(glue_shape_error(
                        plan,
                        ty,
                        "returned Result tracking requires a struct value",
                    ));
                };
                let fields = result.get_field_types();
                let ok_value = self.build_extract_value(
                    result_value.into(),
                    builtin_variant::RESULT_OK_FIELD,
                    "returned_glue_result_ok",
                )?;
                self.register_returned_plan(
                    ok,
                    ok_value,
                    fields[builtin_variant::RESULT_OK_FIELD as usize],
                )?;
                let error_value = self.build_extract_value(
                    result_value.into(),
                    builtin_variant::RESULT_ERROR_FIELD,
                    "returned_glue_result_error",
                )?;
                self.register_result_error_storage(error, error_value)
            }
            GluePlan::Tuple(fields) | GluePlan::Record(fields) => {
                let BasicTypeEnum::StructType(product) = ty else {
                    return Err(glue_shape_error(
                        plan,
                        ty,
                        "returned product tracking requires a struct",
                    ));
                };
                let BasicValueEnum::StructValue(product_value) = value else {
                    return Err(glue_shape_error(
                        plan,
                        ty,
                        "returned product tracking requires a struct value",
                    ));
                };
                for (index, (field, field_ty)) in
                    fields.iter().zip(product.get_field_types()).enumerate()
                {
                    let field_value = self.build_extract_value(
                        product_value.into(),
                        index as u32,
                        "returned_glue_product_field",
                    )?;
                    self.register_returned_plan(field, field_value, field_ty)?;
                }
                Ok(())
            }
            _ => Err(glue_shape_error(
                plan,
                ty,
                "returned-value tracking is not emitted for this shape",
            )),
        }
    }

    fn register_result_error_storage(
        &self,
        error: &GluePlan,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), CompileError> {
        match error {
            GluePlan::Trivial => Ok(()),
            GluePlan::StringBox => {
                let BasicValueEnum::IntValue(handle) = value else {
                    return Err(CompileError::Unsupported(
                        "returned Result<string-error> requires an i64 payload handle".into(),
                    ));
                };
                let pointer_type = self.context.ptr_type(inkwell::AddressSpace::default());
                let box_pointer =
                    self.build_int_to_ptr(handle, pointer_type, "returned_result_error_box")?;
                let is_null = self
                    .builder
                    .build_is_null(box_pointer, "returned_result_error_is_null")
                    .map_err(|error| {
                        CompileError::LlvmError(format!("returned Result error null test: {error}"))
                    })?;
                // Loading the inactive zero handle would dereference null.
                // Select a zero-initialized, entry-dominating StringBox slot
                // instead; its data leaf is null and therefore safe to track.
                let string = self.string_box_type();
                let zero_box = self.build_entry_alloca(string, "returned_result_error_zero")?;
                self.build_store(zero_box, string.const_zero())?;
                let readable = self
                    .builder
                    .build_select(
                        is_null,
                        zero_box,
                        box_pointer,
                        "returned_result_error_readable",
                    )
                    .map_err(|error| {
                        CompileError::LlvmError(format!(
                            "returned Result error pointer select: {error}"
                        ))
                    })?
                    .into_pointer_value();
                let boxed = self
                    .build_load(
                        BasicTypeEnum::StructType(string),
                        readable,
                        "returned_result_error_string",
                    )?
                    .into_struct_value();
                let data = self
                    .build_extract_value(boxed.into(), 0, "returned_result_error_string_data")?
                    .into_pointer_value();
                self.register_heap_alloc(data);
                self.register_heap_alloc(box_pointer);
                Ok(())
            }
            _ => Err(CompileError::Unsupported(format!(
                "returned Result error storage `{}` is not adopted in the current slice",
                error.symbol_suffix()
            ))),
        }
    }

    fn ensure_value_glue(
        &self,
        plan: &GluePlan,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<GluePair<'ctx>, CompileError> {
        validate_plan_type(plan, ty)?;
        match plan {
            GluePlan::StringBox => self.ensure_string_glue_pair(plan, ty),
            GluePlan::Option(_) | GluePlan::Result { .. } if plan.is_emitted() => {
                self.ensure_variant_glue_pair(plan, ty)
            }
            GluePlan::Tuple(_) | GluePlan::Record(_) if plan.is_emitted() => {
                self.ensure_product_glue_pair(plan, ty)
            }
            _ => Err(CompileError::Unsupported(format!(
                "value glue plan `{}` is derived but not emitted in the current slice",
                plan.symbol_suffix()
            ))),
        }
    }

    fn ensure_string_glue_pair(
        &self,
        plan: &GluePlan,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<GluePair<'ctx>, CompileError> {
        validate_plan_type(plan, ty)?;
        let suffix = plan.symbol_suffix();
        let clone_name = format!("mimi_value_clone_glue__{suffix}");
        let drop_name = format!("mimi_value_drop_glue__{suffix}");
        let string = self.string_box_type();
        let clone_type = string.fn_type(&[BasicMetadataTypeEnum::StructType(string)], false);
        let drop_type = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::StructType(string)], false);

        let clone = match self.module.get_function(&clone_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == clone_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{clone_name}`"
                )))
            }
            None => self.emit_string_clone_glue(&clone_name)?,
        };
        let drop = match self.module.get_function(&drop_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == drop_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{drop_name}`"
                )))
            }
            None => self.emit_string_drop_glue(&drop_name)?,
        };
        Ok(GluePair { clone, drop })
    }

    fn ensure_variant_glue_pair(
        &self,
        plan: &GluePlan,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<GluePair<'ctx>, CompileError> {
        validate_plan_type(plan, ty)?;
        let BasicTypeEnum::StructType(variant) = ty else {
            return Err(glue_shape_error(
                plan,
                ty,
                "variant glue requires a struct ABI",
            ));
        };
        let suffix = format!(
            "{}__llvm_{}",
            plan.symbol_suffix(),
            llvm_type_symbol_suffix(ty)
        );
        let clone_name = format!("mimi_value_clone_glue__{suffix}");
        let drop_name = format!("mimi_value_drop_glue__{suffix}");
        let clone_type = variant.fn_type(&[BasicMetadataTypeEnum::StructType(variant)], false);
        let drop_type = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::StructType(variant)], false);

        let clone = match self.module.get_function(&clone_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == clone_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{clone_name}`"
                )))
            }
            None => self.emit_variant_clone_glue(plan, variant, &clone_name)?,
        };
        let drop = match self.module.get_function(&drop_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == drop_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{drop_name}`"
                )))
            }
            None => self.emit_variant_drop_glue(plan, variant, &drop_name)?,
        };
        Ok(GluePair { clone, drop })
    }

    fn clone_inline_glue_value(
        &self,
        plan: &GluePlan,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if matches!(plan, GluePlan::Trivial) {
            return Ok(value);
        }
        let pair = self.ensure_value_glue(plan, ty)?;
        let call = self.build_call(pair.clone, &[BasicMetadataValueEnum::from(value)], name)?;
        call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("child clone glue returned void".into()))
    }

    fn drop_inline_glue_value(
        &self,
        plan: &GluePlan,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        name: &str,
    ) -> Result<(), CompileError> {
        if matches!(plan, GluePlan::Trivial) {
            return Ok(());
        }
        let pair = self.ensure_value_glue(plan, ty)?;
        self.build_call(pair.drop, &[BasicMetadataValueEnum::from(value)], name)?;
        Ok(())
    }

    fn clone_result_error_storage(
        &self,
        plan: &GluePlan,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match plan {
            GluePlan::Trivial => Ok(value),
            GluePlan::StringBox => {
                let pair = self.ensure_packed_result_string_glue_pair()?;
                let call = self.build_call(
                    pair.clone,
                    &[BasicMetadataValueEnum::from(value)],
                    "clone_result_error_child",
                )?;
                call_try_basic_value(&call).ok_or_else(|| {
                    CompileError::LlvmError("packed Result error clone returned void".into())
                })
            }
            _ => Err(CompileError::Unsupported(format!(
                "Result error storage `{}` cannot be cloned in the current slice",
                plan.symbol_suffix()
            ))),
        }
    }

    fn drop_result_error_storage(
        &self,
        plan: &GluePlan,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), CompileError> {
        match plan {
            GluePlan::Trivial => Ok(()),
            GluePlan::StringBox => {
                let pair = self.ensure_packed_result_string_glue_pair()?;
                self.build_call(
                    pair.drop,
                    &[BasicMetadataValueEnum::from(value)],
                    "drop_result_error_child",
                )?;
                Ok(())
            }
            _ => Err(CompileError::Unsupported(format!(
                "Result error storage `{}` cannot be dropped in the current slice",
                plan.symbol_suffix()
            ))),
        }
    }

    fn emit_variant_clone_glue(
        &self,
        plan: &GluePlan,
        variant: inkwell::types::StructType<'ctx>,
        name: &str,
    ) -> Result<FunctionValue<'ctx>, CompileError> {
        let function = self.module.add_function(
            name,
            variant.fn_type(&[BasicMetadataTypeEnum::StructType(variant)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            let active_block = self.context.append_basic_block(function, "variant_active");
            let inactive_block = self
                .context
                .append_basic_block(function, "variant_inactive");
            let merge_block = self
                .context
                .append_basic_block(function, "variant_clone_merge");
            self.builder.position_at_end(entry);
            let source = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("variant clone glue parameter missing".into())
                })?
                .into_struct_value();
            let tag = self
                .build_extract_value(
                    source.into(),
                    builtin_variant::DISCRIMINANT_FIELD,
                    "variant_clone_tag",
                )?
                .into_int_value();
            let active = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    tag,
                    tag.get_type().const_int(builtin_variant::ACTIVE_TAG, false),
                    "variant_clone_is_active",
                )
                .map_err(|error| {
                    CompileError::LlvmError(format!("variant clone tag compare: {error}"))
                })?;
            self.build_cond_br(active, active_block, inactive_block)?;

            let fields = variant.get_field_types();
            self.builder.position_at_end(active_block);
            let active_value = match plan {
                GluePlan::Option(payload) => {
                    let field = self.build_extract_value(
                        source.into(),
                        builtin_variant::OPTION_PAYLOAD_FIELD,
                        "clone_option_payload",
                    )?;
                    let cloned = self.clone_inline_glue_value(
                        payload,
                        field,
                        fields[builtin_variant::OPTION_PAYLOAD_FIELD as usize],
                        "clone_option_child",
                    )?;
                    self.builder
                        .build_insert_value(
                            source,
                            cloned,
                            builtin_variant::OPTION_PAYLOAD_FIELD,
                            "clone_option_insert",
                        )
                        .map_err(|error| {
                            CompileError::LlvmError(format!("Option clone payload insert: {error}"))
                        })?
                        .into_struct_value()
                }
                GluePlan::Result { ok, .. } => {
                    let field = self.build_extract_value(
                        source.into(),
                        builtin_variant::RESULT_OK_FIELD,
                        "clone_result_ok",
                    )?;
                    let cloned = self.clone_inline_glue_value(
                        ok,
                        field,
                        fields[builtin_variant::RESULT_OK_FIELD as usize],
                        "clone_result_ok_child",
                    )?;
                    let with_ok = self
                        .builder
                        .build_insert_value(
                            source,
                            cloned,
                            builtin_variant::RESULT_OK_FIELD,
                            "clone_result_ok_insert",
                        )
                        .map_err(|error| {
                            CompileError::LlvmError(format!("Result clone Ok insert: {error}"))
                        })?
                        .into_struct_value();
                    self.builder
                        .build_insert_value(
                            with_ok,
                            self.zero_value_for(
                                fields[builtin_variant::RESULT_ERROR_FIELD as usize],
                            ),
                            builtin_variant::RESULT_ERROR_FIELD,
                            "clone_result_clear_error",
                        )
                        .map_err(|error| {
                            CompileError::LlvmError(format!(
                                "Result clone inactive error clear: {error}"
                            ))
                        })?
                        .into_struct_value()
                }
                _ => {
                    return Err(CompileError::Unsupported(format!(
                        "value glue `{}` is not a built-in variant",
                        plan.symbol_suffix()
                    )))
                }
            };
            let active_end = self
                .builder
                .get_insert_block()
                .ok_or_else(|| CompileError::LlvmError("variant clone lost active block".into()))?;
            self.build_br(merge_block)?;

            self.builder.position_at_end(inactive_block);
            let inactive_value = match plan {
                GluePlan::Option(_) => self
                    .builder
                    .build_insert_value(
                        source,
                        self.zero_value_for(fields[builtin_variant::OPTION_PAYLOAD_FIELD as usize]),
                        builtin_variant::OPTION_PAYLOAD_FIELD,
                        "clone_option_clear_payload",
                    )
                    .map_err(|error| {
                        CompileError::LlvmError(format!(
                            "Option clone inactive payload clear: {error}"
                        ))
                    })?
                    .into_struct_value(),
                GluePlan::Result { error, .. } => {
                    let field = self.build_extract_value(
                        source.into(),
                        builtin_variant::RESULT_ERROR_FIELD,
                        "clone_result_error",
                    )?;
                    let cloned = self.clone_result_error_storage(error, field)?;
                    let cleared = self
                        .builder
                        .build_insert_value(
                            source,
                            self.zero_value_for(fields[builtin_variant::RESULT_OK_FIELD as usize]),
                            builtin_variant::RESULT_OK_FIELD,
                            "clone_result_clear_ok",
                        )
                        .map_err(|error| {
                            CompileError::LlvmError(format!(
                                "Result clone inactive Ok clear: {error}"
                            ))
                        })?
                        .into_struct_value();
                    self.builder
                        .build_insert_value(
                            cleared,
                            cloned,
                            builtin_variant::RESULT_ERROR_FIELD,
                            "clone_result_error_insert",
                        )
                        .map_err(|error| {
                            CompileError::LlvmError(format!("Result clone Err insert: {error}"))
                        })?
                        .into_struct_value()
                }
                _ => unreachable!("variant plan checked above"),
            };
            let inactive_end = self.builder.get_insert_block().ok_or_else(|| {
                CompileError::LlvmError("variant clone lost inactive block".into())
            })?;
            self.build_br(merge_block)?;

            self.builder.position_at_end(merge_block);
            let phi = self
                .builder
                .build_phi(variant, "variant_clone_result")
                .map_err(|error| CompileError::LlvmError(format!("variant clone phi: {error}")))?;
            phi.add_incoming(&[(&active_value, active_end), (&inactive_value, inactive_end)]);
            self.build_return(Some(&phi.as_basic_value()))
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }

    fn emit_variant_drop_glue(
        &self,
        plan: &GluePlan,
        variant: inkwell::types::StructType<'ctx>,
        name: &str,
    ) -> Result<FunctionValue<'ctx>, CompileError> {
        let function = self.module.add_function(
            name,
            self.context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::StructType(variant)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            let active_block = self.context.append_basic_block(function, "variant_active");
            let inactive_block = self
                .context
                .append_basic_block(function, "variant_inactive");
            let exit_block = self
                .context
                .append_basic_block(function, "variant_drop_exit");
            self.builder.position_at_end(entry);
            let source = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("variant drop glue parameter missing".into())
                })?
                .into_struct_value();
            let tag = self
                .build_extract_value(
                    source.into(),
                    builtin_variant::DISCRIMINANT_FIELD,
                    "variant_drop_tag",
                )?
                .into_int_value();
            let active = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    tag,
                    tag.get_type().const_int(builtin_variant::ACTIVE_TAG, false),
                    "variant_drop_is_active",
                )
                .map_err(|error| {
                    CompileError::LlvmError(format!("variant drop tag compare: {error}"))
                })?;
            self.build_cond_br(active, active_block, inactive_block)?;

            let fields = variant.get_field_types();
            self.builder.position_at_end(active_block);
            match plan {
                GluePlan::Option(payload) => {
                    let value = self.build_extract_value(
                        source.into(),
                        builtin_variant::OPTION_PAYLOAD_FIELD,
                        "drop_option_payload",
                    )?;
                    self.drop_inline_glue_value(
                        payload,
                        value,
                        fields[builtin_variant::OPTION_PAYLOAD_FIELD as usize],
                        "drop_option_child",
                    )?;
                }
                GluePlan::Result { ok, .. } => {
                    let value = self.build_extract_value(
                        source.into(),
                        builtin_variant::RESULT_OK_FIELD,
                        "drop_result_ok",
                    )?;
                    self.drop_inline_glue_value(
                        ok,
                        value,
                        fields[builtin_variant::RESULT_OK_FIELD as usize],
                        "drop_result_ok_child",
                    )?;
                }
                _ => unreachable!("variant plan checked above"),
            }
            self.build_br(exit_block)?;

            self.builder.position_at_end(inactive_block);
            if let GluePlan::Result { error, .. } = plan {
                let value = self.build_extract_value(
                    source.into(),
                    builtin_variant::RESULT_ERROR_FIELD,
                    "drop_result_error",
                )?;
                self.drop_result_error_storage(error, value)?;
            }
            self.build_br(exit_block)?;

            self.builder.position_at_end(exit_block);
            self.build_return(None)
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }

    fn ensure_packed_result_string_glue_pair(&self) -> Result<GluePair<'ctx>, CompileError> {
        let clone_name = "mimi_value_clone_glue__result_error_packed_string";
        let drop_name = "mimi_value_drop_glue__result_error_packed_string";
        let i64_type = self.context.i64_type();
        let clone_type = i64_type.fn_type(&[BasicMetadataTypeEnum::IntType(i64_type)], false);
        let drop_type = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::IntType(i64_type)], false);
        let clone = match self.module.get_function(clone_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == clone_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{clone_name}`"
                )))
            }
            None => self.emit_packed_result_string_clone_glue(clone_name)?,
        };
        let drop = match self.module.get_function(drop_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == drop_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{drop_name}`"
                )))
            }
            None => self.emit_packed_result_string_drop_glue(drop_name)?,
        };
        Ok(GluePair { clone, drop })
    }

    fn emit_packed_result_string_clone_glue(
        &self,
        name: &str,
    ) -> Result<FunctionValue<'ctx>, CompileError> {
        let i64_type = self.context.i64_type();
        let function = self.module.add_function(
            name,
            i64_type.fn_type(&[BasicMetadataTypeEnum::IntType(i64_type)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            let null_block = self
                .context
                .append_basic_block(function, "packed_string_null");
            let copy_block = self
                .context
                .append_basic_block(function, "packed_string_copy");
            let merge_block = self
                .context
                .append_basic_block(function, "packed_string_merge");
            self.builder.position_at_end(entry);
            let handle = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("packed string clone parameter missing".into())
                })?
                .into_int_value();
            let is_null = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    handle,
                    i64_type.const_zero(),
                    "packed_string_is_null",
                )
                .map_err(|error| {
                    CompileError::LlvmError(format!("packed string null compare: {error}"))
                })?;
            self.build_cond_br(is_null, null_block, copy_block)?;

            self.builder.position_at_end(null_block);
            self.build_br(merge_block)?;

            self.builder.position_at_end(copy_block);
            let pointer_type = self.context.ptr_type(inkwell::AddressSpace::default());
            let source_pointer =
                self.build_int_to_ptr(handle, pointer_type, "packed_string_ptr")?;
            let string = self.string_box_type();
            let source = self.build_load(
                BasicTypeEnum::StructType(string),
                source_pointer,
                "packed_string_value",
            )?;
            let string_pair = self.ensure_string_glue_pair(&GluePlan::StringBox, string.into())?;
            let call = self.build_call(
                string_pair.clone,
                &[BasicMetadataValueEnum::from(source)],
                "packed_string_clone_child",
            )?;
            let cloned = call_try_basic_value(&call).ok_or_else(|| {
                CompileError::LlvmError("packed string child clone returned void".into())
            })?;
            let box_size = string
                .size_of()
                .ok_or_else(|| CompileError::LlvmError("StringBox size is unknown".into()))?;
            let copy_pointer = self.malloc_or_abort(box_size, "packed_result_string")?;
            self.build_store(copy_pointer, cloned)?;
            let copied_handle =
                self.build_ptr_to_int(copy_pointer, i64_type, "packed_result_string_handle")?;
            let copy_end = self.builder.get_insert_block().ok_or_else(|| {
                CompileError::LlvmError("packed string clone lost copy block".into())
            })?;
            self.build_br(merge_block)?;

            self.builder.position_at_end(merge_block);
            let phi = self
                .builder
                .build_phi(i64_type, "packed_string_clone_result")
                .map_err(|error| {
                    CompileError::LlvmError(format!("packed string clone phi: {error}"))
                })?;
            phi.add_incoming(&[
                (&handle as &dyn inkwell::values::BasicValue, null_block),
                (&copied_handle as &dyn inkwell::values::BasicValue, copy_end),
            ]);
            self.build_return(Some(&phi.as_basic_value()))
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }

    fn emit_packed_result_string_drop_glue(
        &self,
        name: &str,
    ) -> Result<FunctionValue<'ctx>, CompileError> {
        let i64_type = self.context.i64_type();
        let function = self.module.add_function(
            name,
            self.context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::IntType(i64_type)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            let drop_block = self
                .context
                .append_basic_block(function, "packed_string_drop");
            let exit_block = self
                .context
                .append_basic_block(function, "packed_string_drop_exit");
            self.builder.position_at_end(entry);
            let handle = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("packed string drop parameter missing".into())
                })?
                .into_int_value();
            let is_null = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    handle,
                    i64_type.const_zero(),
                    "packed_string_drop_is_null",
                )
                .map_err(|error| {
                    CompileError::LlvmError(format!("packed string drop null compare: {error}"))
                })?;
            self.build_cond_br(is_null, exit_block, drop_block)?;

            self.builder.position_at_end(drop_block);
            let pointer_type = self.context.ptr_type(inkwell::AddressSpace::default());
            let box_pointer =
                self.build_int_to_ptr(handle, pointer_type, "packed_string_drop_ptr")?;
            let string = self.string_box_type();
            let value = self.build_load(
                BasicTypeEnum::StructType(string),
                box_pointer,
                "packed_string_drop_value",
            )?;
            let string_pair = self.ensure_string_glue_pair(&GluePlan::StringBox, string.into())?;
            self.build_call(
                string_pair.drop,
                &[BasicMetadataValueEnum::from(value)],
                "packed_string_drop_child",
            )?;
            let free = self.get_runtime_fn("free")?;
            self.build_call(
                free,
                &[BasicMetadataValueEnum::PointerValue(box_pointer)],
                "packed_string_drop_box",
            )?;
            self.build_br(exit_block)?;

            self.builder.position_at_end(exit_block);
            self.build_return(None)
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }

    fn ensure_product_glue_pair(
        &self,
        plan: &GluePlan,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<GluePair<'ctx>, CompileError> {
        validate_plan_type(plan, ty)?;
        let BasicTypeEnum::StructType(product) = ty else {
            return Err(glue_shape_error(
                plan,
                ty,
                "product glue requires a struct ABI",
            ));
        };
        let suffix = format!(
            "{}__llvm_{}",
            plan.symbol_suffix(),
            llvm_type_symbol_suffix(ty)
        );
        let clone_name = format!("mimi_value_clone_glue__{suffix}");
        let drop_name = format!("mimi_value_drop_glue__{suffix}");
        let clone_type = product.fn_type(&[BasicMetadataTypeEnum::StructType(product)], false);
        let drop_type = self
            .context
            .void_type()
            .fn_type(&[BasicMetadataTypeEnum::StructType(product)], false);

        let clone = match self.module.get_function(&clone_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == clone_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{clone_name}`"
                )))
            }
            None => self.emit_product_clone_glue(plan, product, &clone_name)?,
        };
        let drop = match self.module.get_function(&drop_name) {
            Some(function)
                if function.get_linkage() == Linkage::Internal
                    && function.get_type() == drop_type =>
            {
                function
            }
            Some(_) => {
                return Err(CompileError::Unsupported(format!(
                    "reserved value-glue symbol collision: `{drop_name}`"
                )))
            }
            None => self.emit_product_drop_glue(plan, product, &drop_name)?,
        };
        Ok(GluePair { clone, drop })
    }

    fn product_plan_fields<'plan>(
        plan: &'plan GluePlan,
    ) -> Result<&'plan [GluePlan], CompileError> {
        match plan {
            GluePlan::Tuple(fields) | GluePlan::Record(fields) => Ok(fields),
            _ => Err(CompileError::Unsupported(format!(
                "value glue `{}` is not a product",
                plan.symbol_suffix()
            ))),
        }
    }

    fn emit_product_clone_glue(
        &self,
        plan: &GluePlan,
        product: inkwell::types::StructType<'ctx>,
        name: &str,
    ) -> Result<FunctionValue<'ctx>, CompileError> {
        let function = self.module.add_function(
            name,
            product.fn_type(&[BasicMetadataTypeEnum::StructType(product)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            self.builder.position_at_end(entry);
            let source = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("product clone glue parameter missing".into())
                })?
                .into_struct_value();
            let fields = Self::product_plan_fields(plan)?;
            let llvm_fields = product.get_field_types();
            let mut cloned = source;
            for (index, (field, field_ty)) in fields.iter().zip(llvm_fields).enumerate() {
                if matches!(field, GluePlan::Trivial) {
                    continue;
                }
                let pair = self.ensure_value_glue(field, field_ty)?;
                let value =
                    self.build_extract_value(source.into(), index as u32, "clone_product_field")?;
                let call = self.build_call(
                    pair.clone,
                    &[BasicMetadataValueEnum::from(value)],
                    "clone_product_child",
                )?;
                let child = call_try_basic_value(&call).ok_or_else(|| {
                    CompileError::LlvmError("product child clone returned void".into())
                })?;
                cloned = self
                    .builder
                    .build_insert_value(cloned, child, index as u32, "clone_product_insert")
                    .map_err(|error| {
                        CompileError::LlvmError(format!("product clone insert: {error}"))
                    })?
                    .into_struct_value();
            }
            self.build_return(Some(&cloned))
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }

    fn emit_product_drop_glue(
        &self,
        plan: &GluePlan,
        product: inkwell::types::StructType<'ctx>,
        name: &str,
    ) -> Result<FunctionValue<'ctx>, CompileError> {
        let function = self.module.add_function(
            name,
            self.context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::StructType(product)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            self.builder.position_at_end(entry);
            let source = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("product drop glue parameter missing".into())
                })?
                .into_struct_value();
            let fields = Self::product_plan_fields(plan)?;
            let llvm_fields = product.get_field_types();
            for (index, (field, field_ty)) in fields.iter().zip(llvm_fields).enumerate() {
                if matches!(field, GluePlan::Trivial) {
                    continue;
                }
                let pair = self.ensure_value_glue(field, field_ty)?;
                let value =
                    self.build_extract_value(source.into(), index as u32, "drop_product_field")?;
                self.build_call(
                    pair.drop,
                    &[BasicMetadataValueEnum::from(value)],
                    "drop_product_child",
                )?;
            }
            self.build_return(None)
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }

    fn string_box_type(&self) -> inkwell::types::StructType<'ctx> {
        self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        )
    }

    fn emit_string_clone_glue(&self, name: &str) -> Result<FunctionValue<'ctx>, CompileError> {
        let string = self.string_box_type();
        let function = self.module.add_function(
            name,
            string.fn_type(&[BasicMetadataTypeEnum::StructType(string)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            self.builder.position_at_end(entry);
            let source = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("string clone glue parameter missing".into())
                })?
                .into_struct_value();
            let data = self
                .build_extract_value(source.into(), 0, "clone_string_data")?
                .into_pointer_value();
            let is_null = self
                .builder
                .build_is_null(data, "clone_string_is_null")
                .map_err(|error| CompileError::LlvmError(format!("string null test: {error}")))?;
            let null_block = self.context.append_basic_block(function, "clone_null");
            let copy_block = self.context.append_basic_block(function, "clone_copy");
            let continue_block = self.context.append_basic_block(function, "clone_continue");
            self.build_cond_br(is_null, null_block, copy_block)?;

            // Null is a valid empty/absent runtime representation and remains
            // null; this avoids relying on memcpy(null, null, 0).
            self.builder.position_at_end(null_block);
            self.build_br(continue_block)?;

            self.builder.position_at_end(copy_block);
            let BasicValueEnum::StructValue(cloned) = self.heap_copy_string_value(source.into())?
            else {
                return Err(CompileError::LlvmError(
                    "string clone glue produced a non-StringBox value".into(),
                ));
            };
            let copy_end = self.builder.get_insert_block().ok_or_else(|| {
                CompileError::LlvmError("string clone glue lost copy block".into())
            })?;
            self.build_br(continue_block)?;

            self.builder.position_at_end(continue_block);
            let result = self
                .builder
                .build_phi(string, "clone_string_result")
                .map_err(|error| CompileError::LlvmError(format!("string clone phi: {error}")))?;
            result.add_incoming(&[
                (&source as &dyn inkwell::values::BasicValue, null_block),
                (&cloned as &dyn inkwell::values::BasicValue, copy_end),
            ]);
            self.build_return(Some(&result.as_basic_value()))
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }

    fn emit_string_drop_glue(&self, name: &str) -> Result<FunctionValue<'ctx>, CompileError> {
        let string = self.string_box_type();
        let function = self.module.add_function(
            name,
            self.context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::StructType(string)], false),
            Some(Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let result = (|| {
            let entry = self.context.append_basic_block(function, "entry");
            self.builder.position_at_end(entry);
            let source = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CompileError::LlvmError("string drop glue parameter missing".into())
                })?
                .into_struct_value();
            let data = self
                .build_extract_value(source.into(), 0, "drop_string_data")?
                .into_pointer_value();
            let free = self.get_runtime_fn("free")?;
            self.build_call(
                free,
                &[BasicMetadataValueEnum::PointerValue(data)],
                "drop_string",
            )?;
            self.build_return(None)
        })();
        if let Some(block) = saved_block {
            self.builder.position_at_end(block);
        }
        result?;
        Ok(function)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::abi::ownership::OpaqueHandleKind;

    #[test]
    fn plans_nested_products_without_source_or_llvm_heuristics() {
        let class = OwnershipClass::Record(vec![
            OwnershipClass::StringBox,
            OwnershipClass::Option(Box::new(OwnershipClass::List(Box::new(
                OwnershipClass::Scalar,
            )))),
        ]);
        let plan = GluePlan::derive(&class).unwrap();
        assert_eq!(
            plan,
            GluePlan::Record(vec![
                GluePlan::StringBox,
                GluePlan::Option(Box::new(GluePlan::List(Box::new(GluePlan::Trivial))))
            ])
        );
        assert_eq!(
            plan.symbol_suffix(),
            "record2_6_string_18_option_list_scalar"
        );
    }

    #[test]
    fn linear_generic_and_unknown_values_fail_closed() {
        assert_eq!(
            GluePlan::derive(&OwnershipClass::Linear {
                kind: LinearOwnershipKind::Capability,
                payload: None,
            }),
            Err(GlueDeriveError::Linear(LinearOwnershipKind::Capability))
        );
        assert_eq!(
            GluePlan::derive(&OwnershipClass::Generic),
            Err(GlueDeriveError::Generic)
        );
        assert_eq!(
            GluePlan::derive(&OwnershipClass::Unknown),
            Err(GlueDeriveError::Unknown)
        );
    }

    #[test]
    fn opaque_runtime_handles_derive_noop_glue() {
        assert_eq!(
            GluePlan::derive(&OwnershipClass::OpaqueHandle(
                OpaqueHandleKind::ManagedCollection
            )),
            Ok(GluePlan::OpaqueNoop)
        );
    }

    #[test]
    fn string_glue_pair_is_internal_valid_and_idempotent() {
        let context = inkwell::context::Context::create();
        let generator = CodeGenerator::new(&context, "value_glue_test");
        let plan = GluePlan::derive(&OwnershipClass::StringBox).unwrap();
        let string = generator.string_box_type();
        let first = generator.ensure_value_glue(&plan, string.into()).unwrap();
        let second = generator.ensure_value_glue(&plan, string.into()).unwrap();
        assert_eq!(first.clone, second.clone);
        assert_eq!(first.drop, second.drop);
        assert_eq!(first.clone.get_linkage(), Linkage::Internal);
        assert_eq!(first.drop.get_linkage(), Linkage::Internal);
        generator.module.verify().unwrap();
        assert!(generator.emit_ir().contains("clone_string_is_null"));
    }

    #[test]
    fn product_glue_is_recursive_abi_keyed_and_idempotent() {
        let context = inkwell::context::Context::create();
        let generator = CodeGenerator::new(&context, "value_product_glue_test");
        let string = generator.string_box_type();
        let product = context.struct_type(
            &[
                BasicTypeEnum::StructType(string),
                BasicTypeEnum::IntType(context.i64_type()),
            ],
            false,
        );
        let class = OwnershipClass::Tuple(vec![OwnershipClass::StringBox, OwnershipClass::Scalar]);
        let plan = GluePlan::derive(&class).unwrap();
        let first = generator.ensure_value_glue(&plan, product.into()).unwrap();
        let second = generator.ensure_value_glue(&plan, product.into()).unwrap();
        assert_eq!(first.clone, second.clone);
        assert_eq!(first.drop, second.drop);
        assert_eq!(first.clone.get_linkage(), Linkage::Internal);
        assert_eq!(first.drop.get_linkage(), Linkage::Internal);

        // Ownership shape alone is insufficient as an ABI key: the narrow
        // integer width must produce a distinct product symbol.
        let narrow_product = context.struct_type(
            &[
                BasicTypeEnum::StructType(string),
                BasicTypeEnum::IntType(context.i32_type()),
            ],
            false,
        );
        let narrow = generator
            .ensure_value_glue(&plan, narrow_product.into())
            .unwrap();
        assert_ne!(first.clone, narrow.clone);
        assert_ne!(first.drop, narrow.drop);
        generator.module.verify().unwrap();
        let ir = generator.emit_ir();
        assert!(ir.contains("mimi_value_clone_glue__tuple"));
        assert!(ir.contains("mimi_value_drop_glue__tuple"));
    }

    #[test]
    fn option_and_result_glue_are_discriminator_aware_and_abi_keyed() {
        let context = inkwell::context::Context::create();
        let generator = CodeGenerator::new(&context, "value_variant_glue_test");
        let string = generator.string_box_type();

        let option = context.struct_type(
            &[
                BasicTypeEnum::IntType(context.bool_type()),
                BasicTypeEnum::StructType(string),
            ],
            false,
        );
        let option_plan =
            GluePlan::derive(&OwnershipClass::Option(Box::new(OwnershipClass::StringBox))).unwrap();
        let first = generator
            .ensure_value_glue(&option_plan, option.into())
            .unwrap();
        let second = generator
            .ensure_value_glue(&option_plan, option.into())
            .unwrap();
        assert_eq!(first.clone, second.clone);
        assert_eq!(first.drop, second.drop);

        // Result E is stored through the canonical erased i64 handle. The
        // semantic StringBox plan must therefore emit the packed-storage bridge
        // as well as the outer discriminator-aware glue.
        let result = context.struct_type(
            &[
                BasicTypeEnum::IntType(context.bool_type()),
                BasicTypeEnum::StructType(string),
                BasicTypeEnum::IntType(context.i64_type()),
            ],
            false,
        );
        let result_plan = GluePlan::derive(&OwnershipClass::Result {
            ok: Box::new(OwnershipClass::StringBox),
            error: Box::new(OwnershipClass::StringBox),
        })
        .unwrap();
        generator
            .ensure_value_glue(&result_plan, result.into())
            .unwrap();

        generator.module.verify().unwrap();
        let ir = generator.emit_ir();
        assert!(ir.contains("variant_clone_is_active"));
        assert!(ir.contains("variant_drop_is_active"));
        assert!(ir.contains("mimi_value_clone_glue__result_error_packed_string"));
        assert!(ir.contains("mimi_value_drop_glue__result_error_packed_string"));
        assert!(ir.contains("clone_result_clear_error"));
        assert!(ir.contains("clone_result_clear_ok"));
    }

    #[test]
    fn erased_result_product_error_stays_fail_closed() {
        let context = inkwell::context::Context::create();
        let generator = CodeGenerator::new(&context, "value_variant_fail_closed_test");
        let result = context.struct_type(
            &[
                BasicTypeEnum::IntType(context.bool_type()),
                BasicTypeEnum::IntType(context.i64_type()),
                BasicTypeEnum::IntType(context.i64_type()),
            ],
            false,
        );
        let plan = GluePlan::derive(&OwnershipClass::Result {
            ok: Box::new(OwnershipClass::Scalar),
            error: Box::new(OwnershipClass::Record(vec![OwnershipClass::StringBox])),
        })
        .unwrap();
        let error = match generator.ensure_value_glue(&plan, result.into()) {
            Err(error) => error,
            Ok(_) => panic!("erased product errors need an explicit storage descriptor"),
        };
        assert!(error.to_string().contains("no payload-storage bridge"));
    }

    #[test]
    fn union_glue_is_fail_closed() {
        assert_eq!(
            GluePlan::derive(&OwnershipClass::Union(vec![OwnershipClass::StringBox])),
            Err(GlueDeriveError::Unsupported("union"))
        );
    }

    #[test]
    fn reserved_symbol_collision_fails_closed() {
        let context = inkwell::context::Context::create();
        let generator = CodeGenerator::new(&context, "value_glue_collision_test");
        generator.module.add_function(
            "mimi_value_clone_glue__string",
            context.i64_type().fn_type(&[], false),
            None,
        );
        let plan = GluePlan::derive(&OwnershipClass::StringBox).unwrap();
        let string = generator.string_box_type();
        let error = match generator.ensure_value_glue(&plan, string.into()) {
            Err(error) => error,
            Ok(_) => panic!("reserved glue symbol must not be reused"),
        };
        assert!(error
            .to_string()
            .contains("reserved value-glue symbol collision"));
    }
}
