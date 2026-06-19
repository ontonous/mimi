use super::CodeGenerator;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use crate::error::{CompileError, MimiResult};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_range(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 {
                    return Err(CompileError::WrongArgCount("range expects 2 arguments".to_string()));
                }
                let start_raw = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("range start must be i64".to_string())),
                };
                let end_raw = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("range end must be i64".to_string())),
                };
                // Extend i32 arguments to i64 (Mimi defaults to i32 for small integer literals)
                let i64_ty = self.context.i64_type();
                let start = if start_raw.get_type() == self.context.i32_type() {
                    self.builder.build_int_z_extend(start_raw, i64_ty, "start_ext")
                        .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                } else { start_raw };
                let end = if end_raw.get_type() == self.context.i32_type() {
                    self.builder.build_int_z_extend(end_raw, i64_ty, "end_ext")
                        .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                } else { end_raw };
                // Create a list struct: { i64 len, i64* data }
                // For simplicity in codegen, we use a runtime-allocated array
                let len_val = self.builder.build_int_sub(end, start, "range_len")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                // Allocate array: len * sizeof(i64)
                let sizeof_i64 = self.context.i64_type().const_int(8, false);
                let alloc_size = self.builder.build_int_mul(len_val, sizeof_i64, "alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let data_ptr = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
                    "data_ptr_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                // Fill the array: for i in 0..len: data[i] = start + i
                let idx_alloca = self.builder.build_alloca(i64_ty, "idx")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let function = self.current_function().ok_or_else(|| "codegen: no current function for range loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "range_loop");
                let body_bb = self.context.append_basic_block(function, "range_body");
                let exit_bb = self.context.append_basic_block(function, "range_exit");
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Loop condition
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, len_val, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, exit_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Body: data[idx] = start + idx
                self.builder.position_at_end(body_bb);
                let elem_val = self.builder.build_int_add(start, idx, "elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr_i64, &[idx], "elem_ptr")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(elem_ptr, elem_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                // idx++
                let next_idx = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next_idx")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next_idx)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Exit: create list struct { len, data* }
                self.builder.position_at_end(exit_bb);
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let list_alloca = self.builder.build_alloca(list_ty, "list")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(list_ty, list_alloca, 0, "list_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, len_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let data_gep = self.builder.build_struct_gep(list_ty, list_alloca, 1, "list_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_void_ptr = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "data_void")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
                self.builder.build_store(data_gep, data_void_ptr)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(list_alloca.into())

    }

    pub(super) fn compile_len(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount("len expects 1 argument".to_string()));
                }
                match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => {
                        // Could be a string or list. Assume list struct { len, data* }
                        let list_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        let len_gep = self.builder.build_struct_gep(list_ty, pv, 0, "list.len")
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        let len = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), len_gep, "len")
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        Ok(len)
                    }
                    _ => Err(CompileError::TypeMismatch("len expects a list or string pointer".to_string())),
                }

    }

    pub(super) fn compile_push(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // push(list, elem) - resize data array and append element
                if args.len() != 2 {
                    return Err(CompileError::WrongArgCount("push expects 2 arguments".to_string()));
                }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("push requires a list pointer".to_string())),
                };
                let elem = args[1];

                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);

                // Load current len and data
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "push_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "push_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let old_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "old_len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let old_data = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "old_data")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();

                // new_len = old_len + 1
                let new_len = self.builder.build_int_add(old_len, i64_ty.const_int(1, false), "new_len")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;

                // new_alloc_size = new_len * 8
                let elem_size = i64_ty.const_int(8, false);
                let new_alloc_size = self.builder.build_int_mul(new_len, elem_size, "new_alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;

                // realloc(old_data, new_alloc_size)
                let realloc_fn = self.module.get_function("realloc")
                    .ok_or("realloc not declared")?;
                let realloc_result = self.builder.build_call(realloc_fn, &[
                    BasicMetadataValueEnum::PointerValue(old_data),
                    BasicMetadataValueEnum::IntValue(new_alloc_size),
                ], "realloc_result")
                    .map_err(|e| CompileError::LlvmError(format!("realloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("realloc returned void")?
                    .into_pointer_value();

                // Store new data pointer
                self.builder.build_store(data_gep, realloc_result)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

                // Store new element at data[old_len]: *(new_data + old_len*8) = elem
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let idx_ptr = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(i64_ty),
                        realloc_result,
                        &[old_len],
                        "elem_ptr",
                    ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?
                };
                // Bitcast i8* to i64* for store
                let idx_ptr_i64 = self.builder.build_bit_cast(
                    idx_ptr,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()),
                    "idx_ptr_i64",
                ).map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?.into_pointer_value();

                // Get the element value
                let elem_val = match elem {
                    BasicMetadataValueEnum::IntValue(iv) => BasicValueEnum::IntValue(iv),
                    BasicMetadataValueEnum::FloatValue(fv) => BasicValueEnum::FloatValue(fv),
                    BasicMetadataValueEnum::PointerValue(pv) => BasicValueEnum::PointerValue(pv),
                    _ => return Err(CompileError::TypeMismatch("push: unsupported element type".to_string())),
                };
                self.builder.build_store(idx_ptr_i64, elem_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

                // Store new length
                self.builder.build_store(len_gep, new_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

                // Return the list pointer (unchanged)
                Ok(BasicValueEnum::PointerValue(list_ptr))

    }

    pub(super) fn compile_pop(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // pop(list) - remove and return last element
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount("pop expects 1 argument".to_string()));
                }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("pop requires a list pointer".to_string())),
                };

                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ], false);

                // Load current len and data
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "pop_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "pop_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let old_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "old_len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let old_data = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "old_data")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();

                // Check if empty (len == 0)
                let is_empty = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ, old_len,
                    i64_ty.const_int(0, false), "is_empty")
                    .map_err(|e| CompileError::LlvmError(format!("compare error: {}", e)))?;

                let function = self.current_function().ok_or_else(|| "codegen: no current function for pop".to_string())?;
                let nonempty_bb = self.context.append_basic_block(function, "pop_nonempty");
                let empty_bb = self.context.append_basic_block(function, "pop_empty");
                let merge_bb = self.context.append_basic_block(function, "pop_merge");

                self.builder.build_conditional_branch(is_empty, empty_bb, nonempty_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                // Empty path: return 0
                self.builder.position_at_end(empty_bb);
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                // Non-empty path: get last element, decrement len
                self.builder.position_at_end(nonempty_bb);
                let last_idx = self.builder.build_int_sub(old_len, i64_ty.const_int(1, false), "last_idx")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_ptr = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(i64_ty),
                        old_data,
                        &[last_idx],
                        "elem_ptr",
                    ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?
                };
                let elem_ptr_i64 = self.builder.build_bit_cast(
                    elem_ptr,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()),
                    "elem_ptr_i64",
                ).map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?.into_pointer_value();
                let elem_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr_i64, "elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;

                // new_len = old_len - 1
                let new_len = self.builder.build_int_sub(old_len, i64_ty.const_int(1, false), "new_len")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                self.builder.build_store(len_gep, new_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

                // realloc to shrink (optional, but good practice)
                let new_alloc_size = self.builder.build_int_mul(new_len, i64_ty.const_int(8, false), "new_alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
                let realloc_fn = self.module.get_function("realloc")
                    .ok_or("realloc not declared")?;
                let realloc_result = self.builder.build_call(realloc_fn, &[
                    BasicMetadataValueEnum::PointerValue(old_data),
                    BasicMetadataValueEnum::IntValue(new_alloc_size),
                ], "realloc_result")
                    .map_err(|e| CompileError::LlvmError(format!("realloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("realloc returned void")?
                    .into_pointer_value();
                self.builder.build_store(data_gep, realloc_result)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                // Merge: phi node for the returned element
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(BasicTypeEnum::IntType(i64_ty), "pop_result")
                    .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                let zero = i64_ty.const_int(0, false);
                phi.add_incoming(&[
                    (&BasicValueEnum::IntValue(zero), empty_bb),
                    (&elem_val, nonempty_bb),
                ]);
                Ok(phi.as_basic_value())

    }

    pub(super) fn compile_contains(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("contains expects 2 arguments".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("contains: first arg must be a list".to_string())),
                };
                let elem_val = args[1];
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let i64_ty = self.context.i64_type();
                // Get list length and data
                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false));
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), data_gep, "data"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                // Loop through list elements
                let function = self.current_function().ok_or_else(|| "codegen: no current function for contains loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "contains_loop");
                let body_bb = self.context.append_basic_block(function, "contains_body");
                let found_bb = self.context.append_basic_block(function, "contains_found");
                let done_bb = self.context.append_basic_block(function, "contains_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "ci")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len.into_int_value(), "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                let eq = match (elem, elem_val) {
                    (BasicValueEnum::IntValue(a), BasicMetadataValueEnum::IntValue(b)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, a, b, "eq")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::TypeMismatch("contains: element comparison only supports i64 for now".to_string())),
                };
                self.builder.build_conditional_branch(eq, found_bb, loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Next iteration
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Found
                self.builder.position_at_end(found_bb);
                self.builder.build_unconditional_branch(done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Done: phi(true, false)
                self.builder.position_at_end(done_bb);
                let phi = self.builder.build_phi(i64_ty, "result")
                    .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                phi.add_incoming(&[
                    (&i64_ty.const_int(1, false), found_bb),
                    (&i64_ty.const_int(0, false), loop_bb),
                ]);
                Ok(phi.as_basic_value())

    }

    pub(super) fn compile_sum(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("sum expects 1 argument (list)".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("sum: first arg must be a list".to_string())),
                };
                let i64_ty = self.context.i64_type();
                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false));
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.i8_type().ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                // Loop through list elements and sum
                let function = self.current_function().ok_or_else(|| "codegen: no current function for sum loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "sum_loop");
                let body_bb = self.context.append_basic_block(function, "sum_body");
                let done_bb = self.context.append_basic_block(function, "sum_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "si")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let sum_alloca = self.builder.build_alloca(i64_ty, "sum")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_store(sum_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem = self.builder.build_load(i64_ty, elem_ptr, "elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let sum = self.builder.build_load(i64_ty, sum_alloca, "sum")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let new_sum = self.builder.build_int_add(sum, elem, "new_sum")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(sum_alloca, new_sum)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                let result = self.builder.build_load(i64_ty, sum_alloca, "result_sum")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                Ok(result)

    }

    pub(super) fn compile_reverse(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("reverse expects 1 argument (list)".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("reverse: first arg must be a list".to_string())),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false));
                let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), data_gep, "data"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                // Allocate new array
                let sizeof_i64 = i64_ty.const_int(8, false);
                let alloc_size = self.builder.build_int_mul(list_len, sizeof_i64, "alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let new_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let new_data_i64 = self.builder.build_bit_cast(new_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "new_data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                // Copy elements in reverse order
                let function = self.current_function().ok_or_else(|| "codegen: no current function for reverse loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "reverse_loop");
                let body_bb = self.context.append_basic_block(function, "reverse_body");
                let done_bb = self.context.append_basic_block(function, "reverse_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "ri")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                let idx_plus_1 = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "idx_plus_1")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                let src_idx = self.builder.build_int_sub(list_len, idx_plus_1, "src_idx")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let src_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[src_idx], "src_elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let src_val = self.builder.build_load(i64_ty, src_ptr, "src_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let dst_ptr = unsafe {
                    self.builder.build_gep(i64_ty, new_data_i64, &[idx], "dst_elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(dst_ptr, src_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                // Build result list struct
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "reversed_list")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "result_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(result_len_gep, list_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "result_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let new_data_void = self.builder.build_bit_cast(new_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "new_data_void")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
                self.builder.build_store(result_data_gep, new_data_void)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(result_alloca.into())

    }

    pub(super) fn compile_flatten(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("flatten expects 1 argument (list of lists)".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("flatten: first arg must be a list".to_string())),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty), list_ptr, 0, "outer_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let outer_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "outer_len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "outer_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), data_gep, "outer_data"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    list_struct_ty.ptr_type(inkwell::AddressSpace::default()), "data_list_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                // First pass: count total elements
                let function = self.current_function().ok_or_else(|| "codegen: no current function for flatten loop".to_string())?;
                let count_loop_bb = self.context.append_basic_block(function, "flatten_count_loop");
                let count_body_bb = self.context.append_basic_block(function, "flatten_count_body");
                let count_done_bb = self.context.append_basic_block(function, "flatten_count_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "fi")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let total_alloca = self.builder.build_alloca(i64_ty, "total")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_store(total_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(count_loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(count_loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, outer_len, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, count_body_bb, count_done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(count_body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let inner_list_ptr = unsafe {
                    self.builder.build_gep(list_struct_ty, data_ptr, &[idx], "inner_list")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let inner_len_gep = self.builder.build_struct_gep(list_struct_ty, inner_list_ptr, 0, "inner_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let inner_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), inner_len_gep, "inner_len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let total = self.builder.build_load(i64_ty, total_alloca, "total")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let new_total = self.builder.build_int_add(total, inner_len, "new_total")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(total_alloca, new_total)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(count_loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(count_done_bb);
                let total_len = self.builder.build_load(i64_ty, total_alloca, "total_len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                // Allocate new array
                let sizeof_i64 = i64_ty.const_int(8, false);
                let alloc_size = self.builder.build_int_mul(total_len, sizeof_i64, "alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let new_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let new_data_i64 = self.builder.build_bit_cast(new_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "new_data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                // Second pass: copy elements
                let copy_outer_bb = self.context.append_basic_block(function, "flatten_copy_outer");
                let copy_outer_body_bb = self.context.append_basic_block(function, "flatten_copy_outer_body");
                let copy_inner_bb = self.context.append_basic_block(function, "flatten_copy_inner");
                let copy_inner_body_bb = self.context.append_basic_block(function, "flatten_copy_inner_body");
                let copy_done_bb = self.context.append_basic_block(function, "flatten_copy_done");
                let outer_idx_alloca = self.builder.build_alloca(i64_ty, "foi")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let inner_idx_alloca = self.builder.build_alloca(i64_ty, "fii")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let dest_idx_alloca = self.builder.build_alloca(i64_ty, "fdi")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(outer_idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_store(dest_idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(copy_outer_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(copy_outer_bb);
                let outer_idx = self.builder.build_load(i64_ty, outer_idx_alloca, "outer_idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let outer_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, outer_idx, outer_len, "outer_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(outer_cmp, copy_outer_body_bb, copy_done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(copy_outer_body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let inner_list_ptr = unsafe {
                    self.builder.build_gep(list_struct_ty, data_ptr, &[outer_idx], "inner_list")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let inner_len_gep = self.builder.build_struct_gep(list_struct_ty, inner_list_ptr, 0, "inner_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let inner_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), inner_len_gep, "inner_len")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let inner_data_gep = self.builder.build_struct_gep(list_struct_ty, inner_list_ptr, 1, "inner_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let inner_data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr), inner_data_gep, "inner_data"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let inner_data_ptr = self.builder.build_bit_cast(inner_data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "inner_data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                self.builder.build_store(inner_idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(copy_inner_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(copy_inner_bb);
                let inner_idx = self.builder.build_load(i64_ty, inner_idx_alloca, "inner_idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let inner_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, inner_idx, inner_len, "inner_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(inner_cmp, copy_inner_body_bb, copy_outer_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(copy_inner_body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let src_ptr = unsafe {
                    self.builder.build_gep(i64_ty, inner_data_ptr, &[inner_idx], "inner_elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let src_val = self.builder.build_load(i64_ty, src_ptr, "inner_elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let dest_idx = self.builder.build_load(i64_ty, dest_idx_alloca, "dest_idx")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let dest_ptr = unsafe {
                    self.builder.build_gep(i64_ty, new_data_i64, &[dest_idx], "dest_elem")
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(dest_ptr, src_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let next_dest = self.builder.build_int_add(dest_idx, i64_ty.const_int(1, false), "next_dest")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(dest_idx_alloca, next_dest)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let next_inner = self.builder.build_int_add(inner_idx, i64_ty.const_int(1, false), "next_inner")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(inner_idx_alloca, next_inner)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(copy_inner_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // After inner loop: increment outer_idx and continue
                self.builder.position_at_end(copy_outer_bb);
                let next_outer = self.builder.build_int_add(outer_idx, i64_ty.const_int(1, false), "next_outer")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(outer_idx_alloca, next_outer)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.position_at_end(copy_done_bb);
                // Build result list struct
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "flattened_list")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "result_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(result_len_gep, total_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "result_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let new_data_void = self.builder.build_bit_cast(new_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "new_data_void")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
                self.builder.build_store(result_data_gep, new_data_void)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(result_alloca.into())

    }

    pub(super) fn compile_sort(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("sort expects 1 argument (list)".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("sort: first arg must be a list".to_string())),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty), list_ptr, 0, "sort_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "sort_len_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "sort_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "sort_data_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "sort_data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let function = self.current_function().ok_or_else(|| "codegen: no current function for sort loop".to_string())?;
                let outer_loop_bb = self.context.append_basic_block(function, "sort_outer_loop");
                let outer_body_bb = self.context.append_basic_block(function, "sort_outer_body");
                let inner_loop_bb = self.context.append_basic_block(function, "sort_inner_loop");
                let inner_body_bb = self.context.append_basic_block(function, "sort_inner_body");
                let done_bb = self.context.append_basic_block(function, "sort_done");
                let i_alloca = self.builder.build_alloca(i64_ty, "sort_i")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let j_alloca = self.builder.build_alloca(i64_ty, "sort_j")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(i_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(outer_loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(outer_loop_bb);
                let i_val = self.builder.build_load(i64_ty, i_alloca, "sort_i_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let list_len_minus_1 = self.builder.build_int_sub(list_len, i64_ty.const_int(1, false), "sort_len_minus_1")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                let outer_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, i_val, list_len_minus_1, "sort_outer_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(outer_cmp, outer_body_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(outer_body_bb);
                // j = 0
                self.builder.build_store(j_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(inner_loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(inner_loop_bb);
                let i_val_now = self.builder.build_load(i64_ty, i_alloca, "sort_i_now")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let j_val = self.builder.build_load(i64_ty, j_alloca, "sort_j_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                // inner bound: n - i - 1
                let inner_bound = self.builder.build_int_sub(list_len, i_val_now, "sort_inner_bound")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                let inner_bound_minus_1 = self.builder.build_int_sub(inner_bound, i64_ty.const_int(1, false), "sort_inner_bound_minus_1")
                    .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
                let inner_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, j_val, inner_bound_minus_1, "sort_inner_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(inner_cmp, inner_body_bb, outer_loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(inner_body_bb);
                // Load arr[j] and arr[j+1]
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_j_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[j_val], "sort_elem_j") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem_j = self.builder.build_load(i64_ty, elem_j_ptr, "sort_elem_j_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let j_plus_1 = self.builder.build_int_add(j_val, i64_ty.const_int(1, false), "sort_j_plus_1")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_j1_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[j_plus_1], "sort_elem_j1") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem_j1 = self.builder.build_load(i64_ty, elem_j1_ptr, "sort_elem_j1_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                // if arr[j] > arr[j+1], swap
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SGT, elem_j, elem_j1, "sort_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let swap_bb = self.context.append_basic_block(function, "sort_swap");
                let skip_swap_bb = self.context.append_basic_block(function, "sort_skip_swap");
                self.builder.build_conditional_branch(cmp, swap_bb, skip_swap_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(swap_bb);
                // swap arr[j] and arr[j+1]
                self.builder.build_store(elem_j_ptr, elem_j1)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_store(elem_j1_ptr, elem_j)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(skip_swap_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(skip_swap_bb);
                // j++
                let next_j = self.builder.build_int_add(j_val, i64_ty.const_int(1, false), "sort_next_j")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(j_alloca, next_j)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(inner_loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // After inner loop ends (j >= n-i-1), increment i and continue outer
                self.builder.position_at_end(outer_loop_bb);
                let i_next = self.builder.build_int_add(i_val, i64_ty.const_int(1, false), "sort_i_next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(i_alloca, i_next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                // Build result list (data is already sorted in-place via swaps)
                self.builder.position_at_end(done_bb);
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "sort_result")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "sort_result_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(result_len_gep, list_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "sort_result_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_void = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "sort_data_void")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
                self.builder.build_store(result_data_gep, data_void)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(result_alloca.into())

    }

    pub(super) fn compile_enumerate(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("enumerate expects 1 argument (list)".to_string())); }
                let list_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("enumerate: first arg must be a list".to_string())),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty), list_ptr, 0, "enum_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "enum_len_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "enum_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "enum_data_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "enum_data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let sizeof_pair = i64_ty.const_int(16, false);
                let alloc_size = self.builder.build_int_mul(list_len, sizeof_pair, "enum_alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let result_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "enum_malloc")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let result_data_i64 = self.builder.build_bit_cast(result_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "enum_result_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let function = self.current_function().ok_or_else(|| "codegen: no current function for enumerate loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "enum_loop");
                let body_bb = self.context.append_basic_block(function, "enum_body");
                let done_bb = self.context.append_basic_block(function, "enum_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "enum_idx")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "enum_idx_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "enum_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr, &[idx], "enum_elem") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem = self.builder.build_load(i64_ty, elem_ptr, "enum_elem_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let idx_2 = self.builder.build_int_add(idx, idx, "enum_idx_2")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let pair_index_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[idx_2], "enum_pair_index") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(pair_index_ptr, idx)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let pair_value_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[self.builder.build_int_add(idx_2, i64_ty.const_int(1, false), "enum_idx_2_plus_1").map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?], "enum_pair_value") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(pair_value_ptr, elem)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "enum_next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "enum_result")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "enum_result_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(result_len_gep, list_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "enum_result_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let result_data_void = self.builder.build_bit_cast(result_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "enum_result_void")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
                self.builder.build_store(result_data_gep, result_data_void)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(result_alloca.into())

    }

    pub(super) fn compile_zip(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("zip expects 2 arguments (list, list)".to_string())); }
                let (list_ptr_a, list_ptr_b) = match (&args[0], &args[1]) {
                    (BasicMetadataValueEnum::PointerValue(pv_a), BasicMetadataValueEnum::PointerValue(pv_b)) => (pv_a, pv_b),
                    _ => return Err(CompileError::TypeMismatch("zip: both args must be lists".to_string())),
                };
                let i64_ty = self.context.i64_type();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let list_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let len_gep_a = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty), *list_ptr_a, 0, "zip_len_a")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let len_a = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep_a, "zip_len_a_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let len_gep_b = self.builder.build_struct_gep(BasicTypeEnum::StructType(list_struct_ty), *list_ptr_b, 0, "zip_len_b")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let len_b = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep_b, "zip_len_b_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let min_len = self.builder.build_int_compare(inkwell::IntPredicate::SLT, len_a, len_b, "zip_min")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let min_len = self.builder.build_select(min_len, len_a, len_b, "zip_min_len")
                    .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
                    .into_int_value();
                let data_gep_a = self.builder.build_struct_gep(list_struct_ty, *list_ptr_a, 1, "zip_data_a")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8_a = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep_a, "zip_data_a_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr_a = self.builder.build_bit_cast(data_i8_a,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "zip_data_a_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let data_gep_b = self.builder.build_struct_gep(list_struct_ty, *list_ptr_b, 1, "zip_data_b")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8_b = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep_b, "zip_data_b_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let data_ptr_b = self.builder.build_bit_cast(data_i8_b,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "zip_data_b_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let sizeof_pair = i64_ty.const_int(16, false);
                let alloc_size = self.builder.build_int_mul(min_len, sizeof_pair, "zip_alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let result_data = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "zip_malloc")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let result_data_i64 = self.builder.build_bit_cast(result_data,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "zip_result_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                let function = self.current_function().ok_or_else(|| "codegen: no current function for zip loop".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "zip_loop");
                let body_bb = self.context.append_basic_block(function, "zip_body");
                let done_bb = self.context.append_basic_block(function, "zip_done");
                let idx_alloca = self.builder.build_alloca(i64_ty, "zip_idx")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(loop_bb);
                let idx = self.builder.build_load(i64_ty, idx_alloca, "zip_idx_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, min_len, "zip_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.builder.build_conditional_branch(cmp, body_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(body_bb);
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_a_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr_a, &[idx], "zip_elem_a") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem_a = self.builder.build_load(i64_ty, elem_a_ptr, "zip_elem_a_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_b_ptr = unsafe { self.builder.build_gep(i64_ty, data_ptr_b, &[idx], "zip_elem_b") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let elem_b = self.builder.build_load(i64_ty, elem_b_ptr, "zip_elem_b_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let idx_2 = self.builder.build_int_add(idx, idx, "zip_idx_2")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let pair_a_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[idx_2], "zip_pair_a") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(pair_a_ptr, elem_a)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let pair_b_ptr = unsafe { self.builder.build_gep(i64_ty, result_data_i64, &[self.builder.build_int_add(idx_2, i64_ty.const_int(1, false), "zip_idx_2_plus_1").map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?], "zip_pair_b") }
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(pair_b_ptr, elem_b)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "zip_next")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                self.builder.build_store(idx_alloca, next)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(loop_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                let result_list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let result_alloca = self.builder.build_alloca(result_list_ty, "zip_result")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "zip_result_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(result_len_gep, min_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "zip_result_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let result_data_void = self.builder.build_bit_cast(result_data,
                    self.context.ptr_type(inkwell::AddressSpace::default()), "zip_result_void")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
                self.builder.build_store(result_data_gep, result_data_void)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(result_alloca.into())

    }

}
