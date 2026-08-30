//! Derived value clone/drop glue for the native backend.
//!
//! A glue plan is produced only from [`OwnershipClass`].  Expression shape,
//! source spelling and LLVM pointer heuristics are deliberately absent here.
//! The LLVM module is the registry: deterministic internal symbol names make
//! repeated requests idempotent without another mutable cache.

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};

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

impl<'ctx> CodeGenerator<'ctx> {
    /// Clone a value through derived glue.  Slice 0.40.3.1 intentionally emits
    /// only StringBox; every broader plan remains fail-closed until its emitter
    /// and caller-side adoption are landed together.
    pub(in crate::codegen) fn clone_value_with_derived_glue(
        &self,
        class: &OwnershipClass,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let plan = GluePlan::derive(class).map_err(|error| {
            CompileError::Unsupported(format!("cannot derive value glue: {error}"))
        })?;
        let pair = self.ensure_value_glue(&plan)?;
        let BasicValueEnum::StructValue(value) = value else {
            return Err(CompileError::Unsupported(
                "StringBox clone glue requires the canonical {ptr, i64} value ABI".into(),
            ));
        };
        let call = self.build_call(
            pair.clone,
            &[BasicMetadataValueEnum::StructValue(value)],
            "value_clone_glue",
        )?;
        call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("value clone glue returned void".into()))
    }

    /// The first return-adoption slice produces a fresh, untracked StringBox.
    /// Its callee may therefore free the original heap scope instead of
    /// draining it wholesale.  Composite plans keep the old transfer path.
    pub(in crate::codegen) fn value_glue_makes_return_independent(
        &self,
        class: &OwnershipClass,
    ) -> bool {
        self.value_glue_enabled() && matches!(GluePlan::derive(class), Ok(GluePlan::StringBox))
    }

    fn ensure_value_glue(&self, plan: &GluePlan) -> Result<GluePair<'ctx>, CompileError> {
        match plan {
            GluePlan::StringBox => self.ensure_string_glue_pair(plan),
            _ => Err(CompileError::Unsupported(format!(
                "value glue plan `{}` is derived but not emitted in 0.40.3.1",
                plan.symbol_suffix()
            ))),
        }
    }

    fn ensure_string_glue_pair(&self, plan: &GluePlan) -> Result<GluePair<'ctx>, CompileError> {
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
        let first = generator.ensure_value_glue(&plan).unwrap();
        let second = generator.ensure_value_glue(&plan).unwrap();
        assert_eq!(first.clone, second.clone);
        assert_eq!(first.drop, second.drop);
        assert_eq!(first.clone.get_linkage(), Linkage::Internal);
        assert_eq!(first.drop.get_linkage(), Linkage::Internal);
        generator.module.verify().unwrap();
        assert!(generator.emit_ir().contains("clone_string_is_null"));
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
        let error = match generator.ensure_value_glue(&plan) {
            Err(error) => error,
            Ok(_) => panic!("reserved glue symbol must not be reused"),
        };
        assert!(error
            .to_string()
            .contains("reserved value-glue symbol collision"));
    }
}
