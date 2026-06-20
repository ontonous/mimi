use super::CodeGenerator;
use super::super::call_try_basic_value;
use crate::error::MimiResult;
use super::super::CallSiteValueExt;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_exit(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 {
                    return Err("exit expects 1 argument".into());
                }
                let code = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("exit code must be integer".into()),
                };
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(code),
                ], "exit_call")
                    .map_err(|e| format!("exit error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_now(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if !args.is_empty() { return Err("now/timestamp expects 0 arguments".into()); }
                let fn_val = self.module.get_function("mimi_now")
                    .ok_or_else(|| "codegen: mimi_now not declared".to_string())?;
                let call = self.builder.build_call(fn_val, &[], "now_call")
                    .map_err(|e| format!("now error: {}", e))?;
                Ok(self.expect_basic_value(&call, "now")?)

    }

    pub(super) fn compile_now_ms(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if !args.is_empty() { return Err("now_ms/timestamp_ms expects 0 arguments".into()); }
                let fn_val = self.module.get_function("mimi_now_ms")
                    .ok_or_else(|| "codegen: mimi_now_ms not declared".to_string())?;
                let call = self.builder.build_call(fn_val, &[], "now_ms_call")
                    .map_err(|e| format!("now_ms error: {}", e))?;
                Ok(self.expect_basic_value(&call, "now_ms")?)

    }

    pub(super) fn compile_sleep(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err("sleep expects 1 argument (milliseconds)".into()); }
                let fn_val = self.module.get_function("mimi_sleep")
                    .ok_or_else(|| "codegen: mimi_sleep not declared".to_string())?;
                self.builder.build_call(fn_val, &args, "sleep_call")
                    .map_err(|e| format!("sleep error: {}", e))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_getenv(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err("getenv expects 1 argument (name)".into()); }
                let getenv_fn = self.module.get_function("mimi_getenv")
                    .ok_or_else(|| "codegen: mimi_getenv not declared".to_string())?;
                let call = self.builder.build_call(getenv_fn, &args, "getenv_call")
                    .map_err(|e| format!("getenv error: {}", e))?;
                let ptr = match call_try_basic_value(&call) {
                    Some(BasicValueEnum::PointerValue(pv)) => pv,
                    _ => return Err("getenv should return a pointer".into()),
                };
                // Check if NULL (env var not set); return {null, 0} instead of calling strlen(NULL)
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "getenv_str")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| format!("gep error: {}", e))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                // Initialize with {null, 0} as default (NULL env var case)
                let zero_i64 = self.context.i64_type().const_int(0, false);
                self.builder.build_store(ptr_gep, i8_ptr.const_null())
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(len_gep, zero_i64)
                    .map_err(|e| format!("store error: {}", e))?;
                let is_null = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    ptr,
                    i8_ptr.const_null(),
                    "env_is_null",
                ).map_err(|e| format!("compare error: {}", e))?;
                let function = self.current_function()
                    .ok_or_else(|| "codegen: no current function for getenv".to_string())?;
                let not_null_bb = self.context.append_basic_block(function, "getenv_not_null");
                let merge_bb = self.context.append_basic_block(function, "getenv_merge");
                self.builder.build_conditional_branch(is_null, merge_bb, not_null_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                // Non-null path: compute strlen and store actual values
                self.builder.position_at_end(not_null_bb);
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(ptr),
                ], "getenv_strlen")
                    .map_err(|e| format!("strlen error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                self.builder.build_store(ptr_gep, ptr)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(merge_bb);
                Ok(str_alloca.into())

    }

    pub(super) fn compile_args(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if !args.is_empty() { return Err("args expects 0 arguments".into()); }
                // Return the args count (simplified: return as i64 for now)
                let count_fn = self.module.get_function("mimi_args_count")
                    .ok_or_else(|| "codegen: mimi_args_count not declared".to_string())?;
                let call = self.builder.build_call(count_fn, &[], "args_count_call")
                    .map_err(|e| format!("args error: {}", e))?;
                Ok(self.expect_basic_value(&call, "args")?)

    }

}
