use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_push(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // push(list, elem) — realloc data array and append element.
        // Uses simple realloc (no header/hidden capacity) for the data buffer.
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "push expects 2 arguments".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "push")?;
        let elem = args[1];

        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_struct_ty = self.list_struct_type();

        // Load current len and data
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "push_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "push_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let old_len = self
            .builder
            .build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "old_len")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let old_data = self
            .builder
            .build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "old_data")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();

        // new_len = old_len + 1
        let new_len = self
            .builder
            .build_int_add(old_len, i64_ty.const_int(1, false), "new_len")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;

        // new_alloc_size = new_len * 8 (each element is i64-sized slot — 8 bytes)
        let sizeof_i64 = i64_ty.const_int(8, false);
        let alloc_size = self
            .builder
            .build_int_mul(new_len, sizeof_i64, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;

        // realloc the data array to accommodate the new element
        let realloc_fn = self
            .module
            .get_function("realloc")
            .ok_or_else(|| "realloc not declared".to_string())?;
        let new_data = self
            .builder
            .build_call(
                realloc_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(old_data),
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ],
                "realloc_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("realloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("realloc returned void")?
            .into_pointer_value();

        // Store new data pointer back to the list struct
        self.builder
            .build_store(data_gep, new_data)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Bitcast new_data to i8* for element GEP
        let data_i8 = self
            .builder
            .build_bit_cast(
                new_data,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "data_i8",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();

        // Store element at data[old_len]
        let idx_ptr = self
            .gep()
            .build_in_bounds_gep(
                BasicTypeEnum::IntType(i64_ty),
                data_i8,
                &[old_len],
                "elem_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let idx_ptr_i64 = self
            .builder
            .build_bit_cast(
                idx_ptr,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "idx_ptr_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();

        let elem_val = match elem {
            BasicMetadataValueEnum::IntValue(iv) => BasicValueEnum::IntValue(iv),
            BasicMetadataValueEnum::FloatValue(fv) => BasicValueEnum::FloatValue(fv),
            BasicMetadataValueEnum::PointerValue(pv) => BasicValueEnum::PointerValue(pv),
            BasicMetadataValueEnum::StructValue(sv) => {
                let sty = sv.get_type();
                let fields = sty.get_field_types();
                // Canonical string struct is { i8*, i64 }. For string lists the
                // runtime expects the element slot to hold the data pointer, not
                // a pointer to the struct. Other struct types (e.g. nested lists)
                // are stored as pointers to a temporary alloca.
                let is_string_struct = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(fields[1], BasicTypeEnum::IntType(_));
                if is_string_struct {
                    // String list elements must be owning C-string pointers.
                    // Extract the source pointer, allocate a fresh copy, and
                    // store the copy so the list remains valid after any
                    // surrounding temporary string allocations are freed.
                    let raw_ptr = self
                        .builder
                        .build_extract_value(sv, 0, "push_str_data")
                        .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?
                        .into_pointer_value();
                    let strlen_fn = self.get_runtime_fn("strlen")?;
                    let len = self
                        .builder
                        .build_call(
                            strlen_fn,
                            &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                            "push_strlen",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strlen returned void")?
                        .into_int_value();
                    let alloc_size = self
                        .builder
                        .build_int_add(len, i64_ty.const_int(1, false), "push_str_alloc_size")
                        .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                    let malloc_fn = self.get_runtime_fn("malloc")?;
                    let buf = self
                        .builder
                        .build_call(
                            malloc_fn,
                            &[BasicMetadataValueEnum::IntValue(alloc_size)],
                            "push_str_malloc",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("malloc returned void")?
                        .into_pointer_value();
                    let memcpy_fn = self.get_runtime_fn("memcpy")?;
                    self.builder
                        .build_call(
                            memcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(raw_ptr),
                                BasicMetadataValueEnum::IntValue(len),
                            ],
                            "push_str_memcpy",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("memcpy error: {}", e)))?;
                    let i8_int_ty = self.context.i8_type();
                    let null_pos = self
                        .gep()
                        .build_in_bounds_gep(
                            BasicTypeEnum::IntType(i8_int_ty),
                            buf,
                            &[len],
                            "push_str_nul",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    self.builder
                        .build_store(null_pos, i8_int_ty.const_int(0, false))
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    BasicValueEnum::PointerValue(buf)
                } else {
                    let alloca = self
                        .builder
                        .build_alloca(sty, "push_struct_tmp")
                        .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                    self.builder
                        .build_store(alloca, sv)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    BasicValueEnum::PointerValue(alloca)
                }
            }
            _ => {
                return Err(CompileError::TypeMismatch(
                    "push: unsupported element type".to_string(),
                ))
            }
        };
        self.builder
            .build_store(idx_ptr_i64, elem_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Update len
        self.builder
            .build_store(len_gep, new_len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        Ok(BasicValueEnum::PointerValue(list_ptr))
    }

    pub(in crate::codegen) fn compile_pop(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // pop(list) - remove and return last element
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "pop expects 1 argument".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "pointer")?;

        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_struct_ty = self.list_struct_type();

        // Load current len and data
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "pop_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "pop_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let old_len = self
            .builder
            .build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "old_len")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let old_data = self
            .builder
            .build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "old_data")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();

        // Check if empty (len == 0)
        let is_empty = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                old_len,
                i64_ty.const_int(0, false),
                "is_empty",
            )
            .map_err(|e| CompileError::LlvmError(format!("compare error: {}", e)))?;

        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for pop".to_string())?;
        let nonempty_bb = self.context.append_basic_block(function, "pop_nonempty");
        let empty_bb = self.context.append_basic_block(function, "pop_empty");
        let merge_bb = self.context.append_basic_block(function, "pop_merge");

        self.builder
            .build_conditional_branch(is_empty, empty_bb, nonempty_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // Empty path: return 0
        self.builder.position_at_end(empty_bb);
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // Non-empty path: get last element, decrement len
        self.builder.position_at_end(nonempty_bb);
        let last_idx = self
            .builder
            .build_int_sub(old_len, i64_ty.const_int(1, false), "last_idx")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let elem_ptr = {
            self.gep()
                .build_in_bounds_gep(
                    BasicTypeEnum::IntType(i64_ty),
                    old_data,
                    &[last_idx],
                    "elem_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?
        };
        let elem_ptr_i64 = self
            .builder
            .build_bit_cast(
                elem_ptr,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "elem_ptr_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        let elem_val = self
            .builder
            .build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr_i64, "elem_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;

        // new_len = old_len - 1
        let new_len = self
            .builder
            .build_int_sub(old_len, i64_ty.const_int(1, false), "new_len")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        self.builder
            .build_store(len_gep, new_len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // realloc to shrink (optional, but good practice)
        let new_alloc_size = self
            .builder
            .build_int_mul(new_len, self.list_elem_size(), "new_alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let realloc_fn = self
            .module
            .get_function("realloc")
            .ok_or("realloc not declared")?;
        let realloc_result = self
            .builder
            .build_call(
                realloc_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(old_data),
                    BasicMetadataValueEnum::IntValue(new_alloc_size),
                ],
                "realloc_result",
            )
            .map_err(|e| CompileError::LlvmError(format!("realloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("realloc returned void")?
            .into_pointer_value();
        self.builder
            .build_store(data_gep, realloc_result)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // Merge: phi node for the returned element
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(BasicTypeEnum::IntType(i64_ty), "pop_result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        let zero = i64_ty.const_int(0, false);
        phi.add_incoming(&[
            (&BasicValueEnum::IntValue(zero), empty_bb),
            (&elem_val, nonempty_bb),
        ]);
        Ok(phi.as_basic_value())
    }

    pub(in crate::codegen) fn compile_reverse(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "reverse expects 1 argument (list)".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "reverse")?;
        let i64_ty = self.context.i64_type();
        let list_len = self.load_list_len(list_ptr)?;
        let data_ptr = self.load_list_data_i64(list_ptr)?;
        // Allocate new array
        let sizeof_i64 = self.list_elem_size();
        let alloc_size = self
            .builder
            .build_int_mul(list_len, sizeof_i64, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let new_data = self
            .builder
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(alloc_size)],
                "malloc_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        let new_data_i64 = self
            .builder
            .build_bit_cast(
                new_data,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "new_data_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        // Copy elements in reverse order
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for reverse loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "reverse_loop");
        let body_bb = self.context.append_basic_block(function, "reverse_body");
        let done_bb = self.context.append_basic_block(function, "reverse_done");
        let idx_alloca = self
            .builder
            .build_alloca(i64_ty, "ri")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_alloca, "idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(cmp, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(body_bb);
        let idx_plus_1 = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "idx_plus_1")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let src_idx = self
            .builder
            .build_int_sub(list_len, idx_plus_1, "src_idx")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let src_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[src_idx], "src_elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let src_val = self
            .builder
            .build_load(i64_ty, src_ptr, "src_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let dst_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, new_data_i64, &[idx], "dst_elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(dst_ptr, src_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(done_bb);
        // Build result list struct
        let result_alloca = self.alloc_list_result(list_len, new_data)?;
        Ok(result_alloca.into())
    }

    pub(in crate::codegen) fn compile_sort(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "sort expects 1 argument (list)".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "sort")?;
        let i64_ty = self.context.i64_type();
        let list_len = self.load_list_len(list_ptr)?;
        let data_ptr = self.load_list_data_i64(list_ptr)?;
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for sort loop".to_string())?;
        let outer_loop_bb = self.context.append_basic_block(function, "sort_outer_loop");
        let outer_body_bb = self.context.append_basic_block(function, "sort_outer_body");
        let inner_loop_bb = self.context.append_basic_block(function, "sort_inner_loop");
        let inner_body_bb = self.context.append_basic_block(function, "sort_inner_body");
        let outer_incr_bb = self.context.append_basic_block(function, "sort_outer_incr");
        let done_bb = self.context.append_basic_block(function, "sort_done");
        let i_alloca = self
            .builder
            .build_alloca(i64_ty, "sort_i")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let j_alloca = self
            .builder
            .build_alloca(i64_ty, "sort_j")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(i_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(outer_loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(outer_loop_bb);
        let i_val = self
            .builder
            .build_load(i64_ty, i_alloca, "sort_i_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let list_len_minus_1 = self
            .builder
            .build_int_sub(list_len, i64_ty.const_int(1, false), "sort_len_minus_1")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let outer_cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                i_val,
                list_len_minus_1,
                "sort_outer_cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(outer_cmp, outer_body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(outer_body_bb);
        // j = 0
        self.builder
            .build_store(j_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(inner_loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(inner_loop_bb);
        let i_val_now = self
            .builder
            .build_load(i64_ty, i_alloca, "sort_i_now")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let j_val = self
            .builder
            .build_load(i64_ty, j_alloca, "sort_j_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // inner bound: n - i - 1
        let inner_bound = self
            .builder
            .build_int_sub(list_len, i_val_now, "sort_inner_bound")
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let inner_bound_minus_1 = self
            .builder
            .build_int_sub(
                inner_bound,
                i64_ty.const_int(1, false),
                "sort_inner_bound_minus_1",
            )
            .map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?;
        let inner_cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                j_val,
                inner_bound_minus_1,
                "sort_inner_cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(inner_cmp, inner_body_bb, outer_incr_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(inner_body_bb);
        // Load arr[j] and arr[j+1]
        // SAFETY: data_ptr is i64* from bitcast; j_val is in-bounds (validated by inner loop condition).
        let elem_j_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[j_val], "sort_elem_j")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem_j = self
            .builder
            .build_load(i64_ty, elem_j_ptr, "sort_elem_j_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let j_plus_1 = self
            .builder
            .build_int_add(j_val, i64_ty.const_int(1, false), "sort_j_plus_1")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let elem_j1_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[j_plus_1], "sort_elem_j1")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem_j1 = self
            .builder
            .build_load(i64_ty, elem_j1_ptr, "sort_elem_j1_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // if arr[j] > arr[j+1], swap
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, elem_j, elem_j1, "sort_cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let swap_bb = self.context.append_basic_block(function, "sort_swap");
        let skip_swap_bb = self.context.append_basic_block(function, "sort_skip_swap");
        self.builder
            .build_conditional_branch(cmp, swap_bb, skip_swap_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(swap_bb);
        // swap arr[j] and arr[j+1]
        self.builder
            .build_store(elem_j_ptr, elem_j1)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_store(elem_j1_ptr, elem_j)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(skip_swap_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(skip_swap_bb);
        // j++
        let next_j = self
            .builder
            .build_int_add(j_val, i64_ty.const_int(1, false), "sort_next_j")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(j_alloca, next_j)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(inner_loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // After inner loop ends (j >= n-i-1), increment i and continue outer
        self.builder.position_at_end(outer_incr_bb);
        let i_val_incr = self
            .builder
            .build_load(i64_ty, i_alloca, "sort_i_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let i_next = self
            .builder
            .build_int_add(i_val_incr, i64_ty.const_int(1, false), "sort_i_next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(i_alloca, i_next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(outer_loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Build result list (data is already sorted in-place via swaps)
        self.builder.position_at_end(done_bb);
        let data_void = self
            .builder
            .build_bit_cast(
                data_ptr,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "sort_data_void",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        let result_alloca = self.alloc_list_result(list_len, data_void.into_pointer_value())?;
        Ok(result_alloca.into())
    }

    pub(in crate::codegen) fn compile_sort_f64(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Delegate to runtime: mimi_sort_f64(data_ptr, count)
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "sort_f64 expects 1 argument (list)".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "sort_f64")?;
        let list_len = self.load_list_len(list_ptr)?;
        let data_ptr = self.load_list_data_i64(list_ptr)?;
        // Call mimi_sort_f64_inplace(data, count)
        let func = self
            .module
            .get_function("mimi_sort_f64_inplace")
            .ok_or_else(|| "mimi_sort_f64_inplace not declared".to_string())?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(list_len),
                ],
                "sort_f64_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("sort_f64 call error: {}", e)))?;
        // Return the same list (sorted in place)
        Ok(list_ptr.into())
    }

    pub(in crate::codegen) fn compile_sort_str(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // sort_str: delegate to runtime mimi_sort_str_inplace(data, count)
        // which reorders the *mut c_char slots in place using lexicographic
        // comparison via CStr. The list's data buffer for List<string> is
        // already `*mut *mut c_char`, matching the runtime signature.
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "sort_str expects 1 argument (list)".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "sort_str")?;
        let list_len = self.load_list_len(list_ptr)?;
        // For List<string>, data is *mut *mut c_char — load as i8* (raw).
        let data_ptr = self.load_list_data_raw(list_ptr)?;
        let func = self
            .module
            .get_function("mimi_sort_str_inplace")
            .ok_or_else(|| "mimi_sort_str_inplace not declared".to_string())?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(list_len),
                ],
                "sort_str_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("sort_str call error: {}", e)))?;
        // Return the same list (sorted in place)
        Ok(list_ptr.into())
    }
}
