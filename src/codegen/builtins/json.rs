use super::CodeGenerator;
use crate::error::MimiResult;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_to_json(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err("[E0711] to_json expects 1 argument".into()); }
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let sprintf_fn = self.module.get_function("sprintf")
                    .ok_or_else(|| "sprintf not declared".to_string())?;
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let alloc_size = i64_ty.const_int(64, false);
                match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "json_malloc")
                            .map_err(|e| format!("malloc error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt = self.builder.build_global_string_ptr("%ld", "json_int_fmt")
                            .map_err(|e| format!("fmt error: {}", e))?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(iv),
                        ], "json_sprintf")
                            .map_err(|e| format!("sprintf error: {}", e))?;
                        let str_len = self.builder.build_call(strlen_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                        ], "json_strlen")
                            .map_err(|e| format!("strlen error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("strlen returned void")?;
                        let string_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ], false);
                        let str_alloca = self.builder.build_alloca(string_ty, "json_str")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(ptr_gep, buf)
                            .map_err(|e| format!("store error: {}", e))?;
                        let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(len_gep, str_len)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(str_alloca.into())
                    }
                    BasicMetadataValueEnum::FloatValue(fv) => {
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "json_malloc")
                            .map_err(|e| format!("malloc error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt = self.builder.build_global_string_ptr("%f", "json_float_fmt")
                            .map_err(|e| format!("fmt error: {}", e))?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(fv),
                        ], "json_sprintf")
                            .map_err(|e| format!("sprintf error: {}", e))?;
                        let str_len = self.builder.build_call(strlen_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                        ], "json_strlen")
                            .map_err(|e| format!("strlen error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("strlen returned void")?;
                        let string_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ], false);
                        let str_alloca = self.builder.build_alloca(string_ty, "json_str")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(ptr_gep, buf)
                            .map_err(|e| format!("store error: {}", e))?;
                        let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(len_gep, str_len)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(str_alloca.into())
                    }
                    _ => {
                        // Try to extract a C string pointer (from both raw i8* and Mimi string struct)
                        let raw_ptr = self.extract_raw_str_ptr(&args[0])?;
                        let buf = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "json_malloc")
                            .map_err(|e| format!("malloc error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt = self.builder.build_global_string_ptr("\"%s\"", "json_str_fmt")
                            .map_err(|e| format!("fmt error: {}", e))?;
                        self.builder.build_call(sprintf_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(raw_ptr),
                        ], "json_sprintf")
                            .map_err(|e| format!("sprintf error: {}", e))?;
                        let str_len = self.builder.build_call(strlen_fn, &[
                            BasicMetadataValueEnum::PointerValue(buf),
                        ], "json_strlen")
                            .map_err(|e| format!("strlen error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("strlen returned void")?;
                        let string_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ], false);
                        let str_alloca = self.builder.build_alloca(string_ty, "json_str")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(ptr_gep, buf)
                            .map_err(|e| format!("store error: {}", e))?;
                        let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(len_gep, str_len)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(str_alloca.into())
                    }
                }

    }

    pub(super) fn compile_from_json(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err("[E0711] from_json expects 1 argument".into()); }
                let raw_ptr = self.extract_raw_str_ptr(&args[0])?;
                let from_json_fn = self.module.get_function("mimi_from_json")
                    .ok_or_else(|| "codegen: mimi_from_json not declared".to_string())?;
                let result = self.builder.build_call(from_json_fn, &[
                    BasicMetadataValueEnum::PointerValue(raw_ptr),
                ], "from_json_call")
                    .map_err(|e| format!("from_json error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_from_json returned void")?
                    .into_pointer_value();
                // Return the raw C string pointer directly (matches how string literals work in codegen)
                Ok(result.into())

    }

    pub(super) fn compile_json_get_string(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err("[E0711] json_get_string expects 2 arguments".into()); }
                let json_ptr = self.extract_raw_str_ptr(&args[0])?;
                let key_ptr = self.extract_raw_str_ptr(&args[1])?;
                let func = self.module.get_function("json_get_string")
                    .ok_or_else(|| "codegen: json_get_string not declared".to_string())?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "json_get_string_call")
                    .map_err(|e| format!("json_get_string error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("json_get_string returned void")?
                    .into_pointer_value();
                Ok(result.into())

    }

    pub(super) fn compile_json_get_int(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err("[E0711] json_get_int expects 2 arguments".into()); }
                let json_ptr = self.extract_raw_str_ptr(&args[0])?;
                let key_ptr = self.extract_raw_str_ptr(&args[1])?;
                let func = self.module.get_function("json_get_int")
                    .ok_or_else(|| "codegen: json_get_int not declared".to_string())?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "json_get_int_call")
                    .map_err(|e| format!("json_get_int error: {}", e))?;
                Ok(self.expect_basic_value(&result, "json_get_int")?)

    }

    pub(super) fn compile_json_get_element(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err("[E0711] json_get_element expects 2 arguments".into()); }
                let json_ptr = self.extract_raw_str_ptr(&args[0])?;
                let index = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] json_get_element: index must be i32".into()),
                };
                let func = self.module.get_function("json_get_element")
                    .ok_or_else(|| "codegen: json_get_element not declared".to_string())?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(json_ptr),
                    BasicMetadataValueEnum::IntValue(index),
                ], "json_get_element_call")
                    .map_err(|e| format!("json_get_element error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("json_get_element returned void")?
                    .into_pointer_value();
                Ok(result.into())

    }

}
