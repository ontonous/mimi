use crate::codegen::{call_try_basic_value, CodeGenerator};
use crate::error::CompileError;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{AggregateValueEnum, BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    /// Extract {fn_ptr, env_ptr} from a closure value (StructValue or PointerValue).
    pub(in crate::codegen) fn extract_closure_ptrs(
        &self,
        closure_val: BasicValueEnum<'ctx>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        match closure_val {
            BasicValueEnum::StructValue(sv) => {
                let agg = AggregateValueEnum::StructValue(sv);
                let fn_ptr = self
                    .build_extract_value(agg, 0, "fn_ptr")?
                    .into_pointer_value();
                let env_ptr = self
                    .build_extract_value(agg, 1, "env_ptr")?
                    .into_pointer_value();
                Ok((fn_ptr, env_ptr))
            }
            BasicValueEnum::PointerValue(pv) => {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let closure_struct_ty =
                    self.context
                        .struct_type(&[BasicTypeEnum::PointerType(i8_ptr); 2], false);
                let loaded = self
                    .build_load(BasicTypeEnum::StructType(closure_struct_ty), pv, "closure_loaded")?
                    .into_struct_value();
                let agg = AggregateValueEnum::StructValue(loaded);
                let fn_ptr = self
                    .build_extract_value(agg, 0, "fn_ptr")?
                    .into_pointer_value();
                let env_ptr = self
                    .build_extract_value(agg, 1, "env_ptr")?
                    .into_pointer_value();
                Ok((fn_ptr, env_ptr))
            }
            _ => Err(CompileError::Generic(
                "expected a closure struct or pointer".into(),
            )),
        }
    }

    /// Build an indirect call to a closure function.
    /// The closure ABI is: `fn(env_ptr: i8*, args...) -> i64`.
    fn emit_closure_call(
        &self,
        fn_ptr: inkwell::values::PointerValue<'ctx>,
        env_ptr: inkwell::values::PointerValue<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let env_meta = BasicMetadataTypeEnum::PointerType(i8_ptr);
        let mut all_meta = vec![env_meta];
        for arg in args {
            all_meta.push(match arg {
                BasicValueEnum::IntValue(iv) => BasicMetadataTypeEnum::IntType(iv.get_type()),
                BasicValueEnum::FloatValue(fv) => BasicMetadataTypeEnum::FloatType(fv.get_type()),
                BasicValueEnum::PointerValue(pv) => {
                    BasicMetadataTypeEnum::PointerType(pv.get_type())
                }
                BasicValueEnum::StructValue(sv) => BasicMetadataTypeEnum::StructType(sv.get_type()),
                BasicValueEnum::ArrayValue(av) => BasicMetadataTypeEnum::ArrayType(av.get_type()),
                BasicValueEnum::VectorValue(vv) => BasicMetadataTypeEnum::VectorType(vv.get_type()),
                BasicValueEnum::ScalableVectorValue(_) => BasicMetadataTypeEnum::IntType(i64_ty),
            });
        }
        let indirect_fn_type = i64_ty.fn_type(&all_meta, false);
        let fn_ptr_typed = self.build_bit_cast(
            BasicValueEnum::PointerValue(fn_ptr),
            BasicTypeEnum::PointerType(i8_ptr),
            "fn_typed",
        )?;
        let mut call_args = vec![BasicMetadataValueEnum::PointerValue(env_ptr)];
        for arg in args {
            call_args.push(match arg {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                BasicValueEnum::ScalableVectorValue(_) => {
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(0, false))
                }
            });
        }
        let call = self
            .builder
            .build_indirect_call(indirect_fn_type, fn_ptr_typed.into_pointer_value(), &call_args, "closure_call")
            .map_err(|e| CompileError::LlvmError(format!("indirect call: {}", e)))?;
        Ok(call_try_basic_value(&call)
            .unwrap_or(BasicValueEnum::IntValue(i64_ty.const_int(0, false))))
    }

    /// Call a closure value with the given arguments.
    /// `closure_val` can be a StructValue {fn_ptr, env_ptr} or a PointerValue to one.
    pub(in crate::codegen) fn compile_closure_call(
        &self,
        closure_val: BasicValueEnum<'ctx>,
        args: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (fn_ptr, env_ptr) = self.extract_closure_ptrs(closure_val)?;
        self.emit_closure_call(fn_ptr, env_ptr, args)
    }
}
