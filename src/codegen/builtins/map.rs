use super::CodeGenerator;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_map_new(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
                let func = self.module.get_function("mimi_map_new")
                    .ok_or("mimi_map_new not declared")?;
                let result = self.builder.build_call(func, &[], "map_new_call")
                    .map_err(|e| format!("map_new error: {}", e))?;
                result.try_as_basic_value().left()
                    .ok_or("mimi_map_new returned void".to_string())

    }

    pub(super) fn compile_map_size(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
                if args.len() != 1 { return Err("map_size expects 1 argument".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_size: first arg must be i64 map handle".into()),
                };
                let func = self.module.get_function("mimi_map_size")
                    .ok_or("mimi_map_size not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                ], "map_size_call")
                    .map_err(|e| format!("map_size error: {}", e))?;
                result.try_as_basic_value().left()
                    .ok_or("mimi_map_size returned void".to_string())

    }

    pub(super) fn compile_has_key(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
                if args.len() != 2 { return Err("has_key expects 2 arguments (map, key)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("has_key: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("has_key: second arg must be string pointer".into()),
                };
                let func = self.module.get_function("mimi_map_has_key")
                    .ok_or("mimi_map_has_key not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "has_key_call")
                    .map_err(|e| format!("has_key error: {}", e))?;
                let int_val = result.try_as_basic_value().left()
                    .ok_or("mimi_map_has_key returned void".to_string())?
                    .into_int_value();
                let const_val = int_val.get_zero_extended_constant().unwrap_or(0);
                Ok(self.context.bool_type().const_int(const_val, false).into())

    }

    pub(super) fn compile_map_get(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
                if args.len() != 2 { return Err("map_get expects 2 arguments (map, key)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_get: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_get: second arg must be string pointer".into()),
                };
                let func = self.module.get_function("mimi_map_get")
                    .ok_or("mimi_map_get not declared")?;
                let value_handle = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "map_get_call")
                    .map_err(|e| format!("map_get error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_map_get returned void".to_string())?
                    .into_int_value();
                let has_key_func = self.module.get_function("mimi_map_has_key")
                    .ok_or("mimi_map_has_key not declared")?;
                let found_int = self.builder.build_call(has_key_func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "has_key_check")
                    .map_err(|e| format!("has_key error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_map_has_key returned void".to_string())?
                    .into_int_value();
                let i64_ty = self.context.i64_type();
                let tuple_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.bool_type()),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let tuple_alloca = self.builder.build_alloca(tuple_ty, "map_get_result")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let found_gep = self.builder.build_struct_gep(tuple_ty, tuple_alloca, 0, "found_field")
                    .map_err(|e| format!("gep error: {}", e))?;
                let found_val = self.builder.build_int_z_extend(found_int,
                    self.context.bool_type(), "found_ext")
                    .map_err(|e| format!("zext error: {}", e))?;
                self.builder.build_store(found_gep, found_val)
                    .map_err(|e| format!("store error: {}", e))?;
                let value_gep = self.builder.build_struct_gep(tuple_ty, tuple_alloca, 1, "value_field")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(value_gep, value_handle)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(tuple_alloca.into())

    }

    pub(super) fn compile_map_set(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
                if args.len() != 3 { return Err("map_set expects 3 arguments (map, key, value)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_set: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_set: second arg must be string pointer".into()),
                };
                let value_handle = match args[2] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_set: third arg must be i64 value handle".into()),
                };
                let func = self.module.get_function("mimi_map_set")
                    .ok_or("mimi_map_set not declared")?;
                self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                    BasicMetadataValueEnum::IntValue(value_handle),
                ], "map_set_call")
                    .map_err(|e| format!("map_set error: {}", e))?;
                let const_val = map_handle.get_zero_extended_constant().unwrap_or(0);
                Ok(self.context.i64_type().const_int(const_val, false).into())

    }

    pub(super) fn compile_map_remove(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
                if args.len() != 2 { return Err("map_remove expects 2 arguments (map, key)".into()); }
                let map_handle = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err("map_remove: first arg must be i64 map handle".into()),
                };
                let key_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_remove: second arg must be string pointer".into()),
                };
                let func = self.module.get_function("mimi_map_remove")
                    .ok_or("mimi_map_remove not declared")?;
                self.builder.build_call(func, &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                ], "map_remove_call")
                    .map_err(|e| format!("map_remove error: {}", e))?;
                let const_val = map_handle.get_zero_extended_constant().unwrap_or(0);
                Ok(self.context.i64_type().const_int(const_val, false).into())

    }

    pub(super) fn compile_map_from_list(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
                if args.len() != 1 { return Err("map_from_list expects 1 argument".into()); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err("map_from_list: first arg must be list pointer".into()),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "map_from_list_len")
                    .map_err(|e| format!("gep error: {}", e))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "map_from_list_len_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "map_from_list_data")
                    .map_err(|e| format!("gep error: {}", e))?;
                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "map_from_list_data_val")
                    .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "map_from_list_data_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let sizeof_pair = i64_ty.const_int(16, false);
                let alloc_size = self.builder.build_int_mul(list_len, sizeof_pair, "map_from_list_alloc")
                    .map_err(|e| format!("mul error: {}", e))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let keys_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "map_from_list_keys_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let values_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "map_from_list_values_malloc")
                    .map_err(|e| format!("malloc error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let keys_ptr = self.builder.build_bit_cast(keys_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "keys_ptr_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let values_ptr = self.builder.build_bit_cast(values_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "values_ptr_i64")
                    .map_err(|e| format!("bitcast error: {}", e))?
                    .into_pointer_value();
                let function = self.current_function().unwrap();
                let loop_bb = self.context.append_basic_block(function, "map_from_list_loop");
                let body_bb = self.context.append_basic_block(function, "map_from_list_body");
                let done_bb = self.context.append_basic_block(function, "map_from_list_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "map_from_list_idx")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "map_from_list_idx_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "map_from_list_cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(body_bb);
                let idx_2 = self.builder.build_int_add(idx, idx, "map_from_list_idx_2")
                    .map_err(|e| format!("add error: {}", e))?;
                let key_ptr_elem = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[idx_2], "map_from_list_key_elem") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let key_handle = self.builder.build_load(i64_ty, key_ptr_elem, "map_from_list_key_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let key_dest = unsafe { self.builder.build_gep(i64_ty, keys_ptr, &[idx], "map_from_list_key_dest") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(key_dest, key_handle)
                    .map_err(|e| format!("store error: {}", e))?;
                let idx_2_plus_1 = self.builder.build_int_add(idx_2, i64_ty.const_int(1, false), "map_from_list_idx_2_plus_1")
                    .map_err(|e| format!("add error: {}", e))?;
                let val_ptr_elem = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[idx_2_plus_1], "map_from_list_val_elem") }
                    .map_err(|e| format!("gep error: {}", e))?;
                let val_handle = self.builder.build_load(i64_ty, val_ptr_elem, "map_from_list_val_val")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value();
                let val_dest = unsafe { self.builder.build_gep(i64_ty, values_ptr, &[idx], "map_from_list_val_dest") }
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(val_dest, val_handle)
                    .map_err(|e| format!("store error: {}", e))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "map_from_list_next")
                    .map_err(|e| format!("add error: {}", e))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| format!("store error: {}", e))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
                self.builder.position_at_end(done_bb);
                let func = self.module.get_function("mimi_map_from_list")
                    .ok_or("mimi_map_from_list not declared")?;
                let result = self.builder.build_call(func, &[
                    BasicMetadataValueEnum::PointerValue(keys_ptr),
                    BasicMetadataValueEnum::PointerValue(values_ptr),
                    BasicMetadataValueEnum::IntValue(list_len),
                ], "map_from_list_call")
                    .map_err(|e| format!("map_from_list error: {}", e))?;
                result.try_as_basic_value().left()
                    .ok_or("mimi_map_from_list returned void".to_string())

    }

}
