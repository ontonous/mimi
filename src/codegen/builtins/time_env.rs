use super::CodeGenerator;
use crate::error::MimiResult;
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
                let ptr = match call.try_as_basic_value().left() {
                    Some(BasicValueEnum::PointerValue(pv)) => pv,
                    _ => return Err("getenv should return a pointer".into()),
                };
                // Check if NULL (env var not set)
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let _is_null = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    ptr,
                    i8_ptr.const_null(),
                    "env_is_null",
                ).map_err(|e| format!("compare error: {}", e))?;
                // For now, return the pointer as-is (caller must handle NULL)
                // A proper implementation would wrap in Ok/Err variant
                Ok(ptr.into())

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
