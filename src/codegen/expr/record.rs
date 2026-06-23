use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {

    pub(in crate::codegen) fn compile_record_expr(
        &mut self,
        ty: &Option<String>,
        fields: &[RecordFieldExpr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Create a record value
        let type_name = ty.as_deref().unwrap_or("unknown");
        let llvm_ty = *self.type_llvm.get(type_name)
            .ok_or_else(|| format!("unknown type '{}'", type_name))?;
        if let BasicTypeEnum::StructType(sty) = llvm_ty {
            let alloca = self.builder.build_alloca(sty, type_name)
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
            // Store field values
            for (i, field) in fields.iter().enumerate() {
                let val = self.compile_expr(&field.value, vars)?;
                let gep = self.gep().build_struct_gep(sty, alloca, i as u32, &field.name)
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(gep, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            }
            Ok(alloca.into())
        } else {
            Err(format!("type '{}' is not a struct", type_name).into())
        }
    }


    pub(in crate::codegen) fn compile_list_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Create a list struct: { i64 len, i64* data }
        let count = elems.len() as u64;
        let len_val = self.context.i64_type().const_int(count, false);
        // Allocate array
        let sizeof_i64 = self.context.i64_type().const_int(8, false);
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
            self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
            "data_ptr_i64")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        // Store each element (universal i64 representation)
        for (i, elem) in elems.iter().enumerate() {
            let val = self.compile_expr(elem, vars)?;
            let iv = match val {
                BasicValueEnum::IntValue(iv) => iv,
                BasicValueEnum::FloatValue(fv) => {
                    self.builder.build_bit_cast(fv, self.context.i64_type(), "f64_to_i64")
                        .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                        .into_int_value()
                }
                BasicValueEnum::PointerValue(pv) => {
                    self.builder.build_ptr_to_int(pv, self.context.i64_type(), "ptr_to_i64")
                        .map_err(|e| CompileError::LlvmError(format!("ptr_to_int error: {}", e)))?
                }
                _ => return Err("list elements must be scalar types (int, float, pointer) for now".into()),
            };
            let idx = self.context.i64_type().const_int(i as u64, false);
                        let elem_ptr = unsafe {
                self.gep().build_gep(self.context.i64_type(), data_ptr_i64, &[idx], "elem")
            }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.builder.build_store(elem_ptr, iv)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        }
        // Create list struct
        let list_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(self.context.i64_type()),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let list_alloca = self.builder.build_alloca(list_ty, "list")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let len_gep = self.gep().build_struct_gep(list_ty, list_alloca, 0, "list_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(len_gep, len_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let data_gep = self.gep().build_struct_gep(list_ty, list_alloca, 1, "list_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_void_ptr = self.builder.build_bit_cast(data_ptr,
            self.context.ptr_type(inkwell::AddressSpace::default()), "data_void")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        self.builder.build_store(data_gep, data_void_ptr)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.register_heap_gep(data_gep);
        Ok(list_alloca.into())
    }


    pub(in crate::codegen) fn compile_tuple_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let mut field_vals = Vec::new();
        for e in elems {
            field_vals.push(self.compile_expr(e, vars)?);
        }
        let field_tys: Vec<BasicTypeEnum<'ctx>> = field_vals.iter().map(|v| v.get_type()).collect();
        let struct_ty = self.context.struct_type(&field_tys, false);
        self.tuple_type_stack.push(struct_ty);
        let alloca = self.builder.build_alloca(struct_ty, "tuple")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        for (i, val) in field_vals.iter().enumerate() {
            let gep = self.gep().build_struct_gep(struct_ty, alloca, i as u32, &format!("tuple_{}", i))
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.builder.build_store(gep, *val)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        }
        Ok(alloca.into())
    }


    pub(in crate::codegen) fn compile_comprehension_expr(
        &mut self,
        expr: &Expr,
        var: &String,
        iter: &Expr,
        guard: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // List comprehension: [expr for x in iter if guard]
        // Compile iter to get list pointer
        let iter_val = self.compile_expr(iter, vars)?;
        let list_ptr = match iter_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("comprehension iter must be a list pointer".into()),
        };
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
            BasicTypeEnum::IntType(i64_ty),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false));
        // Read list length and data
        let len_gep = self.gep().build_struct_gep(list_struct_ty, list_ptr, 0, "comp_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
        let data_gep = self.gep().build_struct_gep(list_struct_ty, list_ptr, 1, "comp_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
        let data_ptr = self.builder.build_bit_cast(data_i8,
            i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?.into_pointer_value();
        // Allocate output array (same max size as input)
        let elem_size = i64_ty.const_int(8, false);
        let alloc_size = self.builder.build_int_mul(list_len, elem_size, "comp_alloc")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let out_ptr = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(alloc_size),
        ], "comp_malloc")
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?.into_pointer_value();
        let out_i64 = self.builder.build_bit_cast(out_ptr,
            i64_ty.ptr_type(inkwell::AddressSpace::default()), "out_i64")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?.into_pointer_value();
        // Loop: for i in 0..len
        let function = self.current_function().ok_or_else(|| "codegen: no current function for comprehension".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "comp_loop");
        let body_bb = self.context.append_basic_block(function, "comp_body");
        let done_bb = self.context.append_basic_block(function, "comp_done");
        let idx_alloca = self.builder.build_alloca(i64_ty, "ci")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let wi_alloca = self.builder.build_alloca(i64_ty, "cw")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder.build_store(wi_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder.build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(loop_bb);
        let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder.build_conditional_branch(cmp, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(body_bb);
        // Load element
                let elem_ptr = unsafe {
            self.gep().build_gep(i64_ty, data_ptr, &[idx], "elem")
        }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        // Bind var
        let mut comp_vars = vars.clone();
        let elem_alloca = self.builder.build_alloca(i64_ty, var)
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder.build_store(elem_alloca, elem)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        comp_vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(i64_ty)));
        // Check guard
        let include = if let Some(g) = guard {
            let g_val = self.compile_expr(g, &comp_vars)?;
            let g_bool = match g_val {
                BasicValueEnum::IntValue(iv) => self.builder.build_int_z_extend(iv, i64_ty, "g_ext")
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?,
                _ => return Err("guard must be boolean".into()),
            };
            self.builder.build_int_compare(inkwell::IntPredicate::NE, g_bool, i64_ty.const_int(0, false), "g_truthy")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        } else {
            self.context.bool_type().const_int(1, false)
        };
        let store_bb = self.context.append_basic_block(function, "comp_store");
        let next_bb = self.context.append_basic_block(function, "comp_next");
        self.builder.build_conditional_branch(include, store_bb, next_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(store_bb);
        // Evaluate expression
        let result = self.compile_expr(expr, &comp_vars)?;
        let wi = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "wi")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
                let out_elem_ptr = unsafe {
            self.gep().build_gep(i64_ty, out_i64, &[wi], "out_elem")
        }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let result_i64 = match result {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::FloatValue(fv) => self.builder.build_float_to_signed_int(fv, i64_ty, "f_to_i")
                .map_err(|e| CompileError::LlvmError(format!("fptosi error: {}", e)))?,
            BasicValueEnum::PointerValue(pv) => self.builder.build_ptr_to_int(pv, i64_ty, "p_to_i")
                .map_err(|e| CompileError::LlvmError(format!("ptrtoint error: {}", e)))?,
            _ => return Err("comprehension expression must produce i64-compatible value".into()),
        };
        self.builder.build_store(out_elem_ptr, result_i64)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let next_wi = self.builder.build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder.build_store(wi_alloca, next_wi)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder.build_unconditional_branch(next_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(next_bb);
        let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder.build_store(idx_alloca, next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder.build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(done_bb);
        // Build result list
        let result_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "result_len")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        let result_alloca = self.builder.build_alloca(list_struct_ty, "comp_result")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let rlen_gep = self.gep().build_struct_gep(list_struct_ty, result_alloca, 0, "rlen")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(rlen_gep, result_len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let rdata_gep = self.gep().build_struct_gep(list_struct_ty, result_alloca, 1, "rdata")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let out_void = self.builder.build_pointer_cast(out_i64, i8_ptr, "out_void")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        self.builder.build_store(rdata_gep, out_void)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(result_alloca.into())
    }

}
