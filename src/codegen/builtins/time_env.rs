use super::super::call_try_basic_value;
use super::super::CallSiteValueExt;
use super::CodeGenerator;
use crate::error::MimiResult;
use inkwell::types::BasicTypeEnum;
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
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.builder
            .build_call(
                exit_fn,
                &[BasicMetadataValueEnum::IntValue(code)],
                "exit_call",
            )
            .map_err(|e| format!("exit error: {}", e))?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_now(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if !args.is_empty() {
            return Err("now/timestamp expects 0 arguments".into());
        }
        let fn_val = self
            .module
            .get_function("mimi_now")
            .ok_or_else(|| "codegen: mimi_now not declared".to_string())?;
        let call = self
            .builder
            .build_call(fn_val, &[], "now_call")
            .map_err(|e| format!("now error: {}", e))?;
        self.expect_basic_value(&call, "now")
    }

    pub(super) fn compile_now_ms(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if !args.is_empty() {
            return Err("now_ms/timestamp_ms expects 0 arguments".into());
        }
        let fn_val = self
            .module
            .get_function("mimi_now_ms")
            .ok_or_else(|| "codegen: mimi_now_ms not declared".to_string())?;
        let call = self
            .builder
            .build_call(fn_val, &[], "now_ms_call")
            .map_err(|e| format!("now_ms error: {}", e))?;
        self.expect_basic_value(&call, "now_ms")
    }

    pub(super) fn compile_sleep(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("sleep expects 1 argument (milliseconds)".into());
        }
        let fn_val = self
            .module
            .get_function("mimi_sleep")
            .ok_or_else(|| "codegen: mimi_sleep not declared".to_string())?;
        self.builder
            .build_call(fn_val, args, "sleep_call")
            .map_err(|e| format!("sleep error: {}", e))?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_getenv(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("getenv expects 1 argument (name)".into());
        }
        let getenv_fn = self
            .module
            .get_function("mimi_getenv")
            .ok_or_else(|| "codegen: mimi_getenv not declared".to_string())?;
        let arg_ptr = self.extract_raw_str_ptr(&args[0])?;
        let call = self
            .builder
            .build_call(
                getenv_fn,
                &[BasicMetadataValueEnum::PointerValue(arg_ptr)],
                "getenv_call",
            )
            .map_err(|e| format!("getenv error: {}", e))?;
        let ptr = match call_try_basic_value(&call) {
            Some(BasicValueEnum::PointerValue(pv)) => pv,
            _ => return Err("getenv should return a pointer".into()),
        };

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        // Result<string,string> layout: {i1 disc, string ok, i64 err}
        let result_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::StructType(string_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        let str_alloca = self.build_alloca(string_ty, "getenv_str")?;
        let result_alloca = self.build_alloca(result_ty, "getenv_result")?;

        let str_ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| format!("gep error: {}", e))?;
        let str_len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| format!("gep error: {}", e))?;
        self.build_store(str_ptr_gep, i8_ptr.const_null())?;
        self.build_store(str_len_gep, i64_ty.const_int(0, false))?;

        let disc_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 0, "res_disc")
            .map_err(|e| format!("gep error: {}", e))?;
        let ok_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 1, "res_ok")
            .map_err(|e| format!("gep error: {}", e))?;
        let err_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 2, "res_err")
            .map_err(|e| format!("gep error: {}", e))?;

        let is_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                ptr,
                i8_ptr.const_null(),
                "env_is_null",
            )
            .map_err(|e| format!("compare error: {}", e))?;
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for getenv".to_string())?;
        let not_null_bb = self.context.append_basic_block(function, "getenv_not_null");
        let err_bb = self.context.append_basic_block(function, "getenv_err");
        let merge_bb = self.context.append_basic_block(function, "getenv_merge");
        self.build_cond_br(is_null, err_bb, not_null_bb)?;

        // Ok branch: disc=1, ok=string, err=0
        self.builder.position_at_end(not_null_bb);
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(ptr)],
                "getenv_strlen",
            )
            .map_err(|e| format!("strlen error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        self.build_store(str_ptr_gep, ptr)?;
        self.build_store(str_len_gep, str_len)?;
        self.build_store(disc_gep, bool_ty.const_int(1, false))?;
        let str_val = self.build_load(string_ty, str_alloca, "str_val")?;
        self.build_store(ok_gep, str_val)?;
        self.build_store(err_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;

        // Err branch: disc=0, ok=zero, err=error message pointer
        self.builder.position_at_end(err_bb);
        let err_msg = self
            .builder
            .build_global_string_ptr("env var not set", "getenv_err_msg")
            .map_err(|e| format!("global string error: {}", e))?;
        self.build_store(disc_gep, bool_ty.const_int(0, false))?;
        self.build_store(ok_gep, string_ty.const_zero())?;
        let err_ptr_int =
            self.build_ptr_to_int(err_msg.as_pointer_value(), i64_ty, "err_ptr_int")?;
        self.build_store(err_gep, err_ptr_int)?;
        self.build_br(merge_bb)?;

        self.builder.position_at_end(merge_bb);
        self.build_load(result_ty, result_alloca, "getenv_loaded")
    }

    pub(super) fn compile_args(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if !args.is_empty() {
            return Err("args expects 0 arguments".into());
        }
        let list_fn = self
            .module
            .get_function("mimi_args_list")
            .ok_or_else(|| "codegen: mimi_args_list not declared".to_string())?;
        let call = self
            .builder
            .build_call(list_fn, &[], "args_list_call")
            .map_err(|e| format!("args error: {}", e))?;
        self.expect_basic_value(&call, "args")
    }
}
