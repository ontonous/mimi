use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_range(
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
                let sizeof_i64 = self.list_elem_size();
                let alloc_size = self.builder.build_int_mul(len_val, sizeof_i64, "alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let data_ptr = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "malloc_call")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
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
                                let elem_ptr = {
                    self.gep().build_in_bounds_gep(i64_ty, data_ptr_i64, &[idx], "elem_ptr")
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
                let list_alloca = self.alloc_list_result(len_val, data_ptr)?;
                Ok(list_alloca.into())

    }

}
