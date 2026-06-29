use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_record_expr(
        &mut self,
        ty: &Option<String>,
        fields: &[RecordFieldExpr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let type_name = ty.as_deref().unwrap_or("unknown");
        let llvm_ty = *self
            .type_llvm
            .get(type_name)
            .ok_or_else(|| format!("unknown type '{}'", type_name))?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(format!("type '{}' is not a struct", type_name).into());
        };

        let alloca = self.build_alloca(sty, type_name)?;
        for (i, field) in fields.iter().enumerate() {
            let val = self.compile_expr(&field.value, vars)?;
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let field_ty = sty
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("field {} type", i)))?;
            let store_val = self.maybe_load_compound_field_value(val, field_ty, field, vars)?;
            self.build_store(gep, store_val)?;
        }
        Ok(alloca.into())
    }

    /// When a PointerValue is stored into a struct-typed field, check if the
    /// expression produces a compound value that needs loading.
    fn maybe_load_compound_field_value(
        &self,
        val: BasicValueEnum<'ctx>,
        field_ty: BasicTypeEnum<'ctx>,
        field: &RecordFieldExpr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(_)) = (&val, field_ty)
        else {
            return Ok(val);
        };
        let needs_load = matches!(
            &field.value,
            Expr::List(_)
                | Expr::Tuple(_)
                | Expr::Comprehension { .. }
                | Expr::SetLiteral(_)
                | Expr::Block(_)
        ) || {
            let val_type = self.infer_object_type(&field.value, vars);
            val_type.starts_with("List")
                || val_type.starts_with("Set")
                || val_type.starts_with("Option")
                || val_type.starts_with("Result")
                || self.type_defs.contains_key(&val_type)
        };
        if needs_load {
            self.build_load(field_ty, *pv, &format!("load_{}", field.name))
        } else {
            Ok(val)
        }
    }

    pub(in crate::codegen) fn compile_list_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let count = elems.len() as u64;
        let len_val = self.context.i64_type().const_int(count, false);
        let (data_ptr, data_ptr_i64) = self.allocate_list_data(count)?;
        self.store_list_elements(data_ptr_i64, elems, vars)?;
        self.build_list_struct(len_val, data_ptr)
    }

    fn allocate_list_data(
        &self,
        count: u64,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let len_val = self.context.i64_type().const_int(count, false);
        let sizeof_i64 = self.context.i64_type().const_int(8, false);
        let alloc_size = self
            .builder
            .build_int_mul(len_val, sizeof_i64, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let data_ptr = self
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(alloc_size)],
                "malloc_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        let data_ptr_i64 = self
            .build_bit_cast(
                data_ptr.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "data_ptr_i64",
            )?
            .into_pointer_value();
        Ok((data_ptr, data_ptr_i64))
    }

    fn store_list_elements(
        &mut self,
        data_ptr_i64: inkwell::values::PointerValue<'ctx>,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        for (i, elem) in elems.iter().enumerate() {
            let val = self.compile_expr(elem, vars)?;
            let iv = self.coerce_to_list_storage(val, elem, vars)?;
            let idx = self.context.i64_type().const_int(i as u64, false);
            let elem_ptr =
                self.build_in_bounds_gep(self.context.i64_type(), data_ptr_i64, &[idx], "elem")?;
            self.build_store(elem_ptr, iv)?;
        }
        Ok(())
    }

    fn coerce_to_list_storage(
        &mut self,
        val: BasicValueEnum<'ctx>,
        _elem_expr: &Expr,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        match val {
            BasicValueEnum::IntValue(iv) => Ok(iv),
            BasicValueEnum::FloatValue(fv) => Ok(self
                .build_bit_cast(fv.into(), self.context.i64_type().into(), "f64_to_i64")?
                .into_int_value()),
            BasicValueEnum::PointerValue(pv) => {
                self.build_ptr_to_int(pv, self.context.i64_type(), "ptr_to_i64")
            }
            BasicValueEnum::StructValue(sv) => {
                let struct_ty = sv.get_type();
                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(struct_ty));
                let malloc_fn = self.malloc_fn();
                let size_val = self.context.i64_type().const_int(size, false);
                let ptr = self
                    .build_call(
                        malloc_fn,
                        &[BasicMetadataValueEnum::IntValue(size_val)],
                        "malloc",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let typed_ptr = self
                    .build_bit_cast(
                        ptr.into(),
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        "struct_ptr",
                    )?
                    .into_pointer_value();
                self.build_store(typed_ptr, sv)?;
                self.build_ptr_to_int(typed_ptr, self.context.i64_type(), "ptr_to_i64")
            }
            _ => Err("list elements must be scalar or struct types for now".into()),
        }
    }

    fn malloc_fn(&self) -> inkwell::values::FunctionValue<'ctx> {
        self.module.get_function("malloc").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i64_ty = self.context.i64_type();
            self.module.add_function(
                "malloc",
                i8_ptr.fn_type(&[BasicMetadataTypeEnum::IntType(i64_ty)], false),
                Some(inkwell::module::Linkage::External),
            )
        })
    }

    fn build_list_struct(
        &self,
        len_val: inkwell::values::IntValue<'ctx>,
        data_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let list_ty = self.list_struct_type();
        let list_alloca = self.build_alloca(list_ty, "list")?;
        let len_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 0, "list_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(len_gep, len_val)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_void_ptr = self.build_bit_cast(
            data_ptr.into(),
            self.context
                .ptr_type(inkwell::AddressSpace::default())
                .into(),
            "data_void",
        )?;
        self.build_store(data_gep, data_void_ptr)?;
        self.register_heap_gep(data_gep);
        Ok(list_alloca.into())
    }

    pub(in crate::codegen) fn compile_tuple_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let string_struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let mut field_vals = Vec::new();
        let mut field_tys = Vec::new();
        for e in elems.iter() {
            let val = self.compile_expr(e, vars)?;
            if let BasicValueEnum::PointerValue(pv) = val {
                if self.expr_is_string(e) {
                    let loaded = self.wrap_tuple_string_field(pv, string_struct_ty)?;
                    field_vals.push(loaded);
                    field_tys.push(BasicTypeEnum::StructType(string_struct_ty));
                    continue;
                }
            }
            field_tys.push(val.get_type());
            field_vals.push(val);
        }
        let struct_ty = self.context.struct_type(&field_tys, false);
        self.tuple_type_stack.push(struct_ty);
        let alloca = self.build_alloca(struct_ty, "tuple")?;
        for (i, val) in field_vals.iter().enumerate() {
            let gep = self
                .gep()
                .build_struct_gep(struct_ty, alloca, i as u32, &format!("tuple_{}", i))
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(gep, *val)?;
        }
        Ok(alloca.into())
    }

    fn wrap_tuple_string_field(
        &self,
        pv: inkwell::values::PointerValue<'ctx>,
        string_struct_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let str_alloca = self.build_alloca(string_struct_ty, "tuple_str")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_struct_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ptr_gep, pv)?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_struct_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let s_len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(pv)],
                "strlen_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        self.build_store(len_gep, s_len)?;
        self.build_load(string_struct_ty, str_alloca, "tuple_str_val")
    }

    pub(in crate::codegen) fn compile_comprehension_expr(
        &mut self,
        expr: &Expr,
        var: &str,
        iter: &Expr,
        guard: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (list_ptr, list_len, data_ptr) = self.load_comprehension_input(iter, vars)?;
        let (out_i64, out_ptr) = self.allocate_comprehension_output(list_len)?;
        let (_idx_alloca, wi_alloca) = self.emit_comprehension_loop(
            expr, var, guard, list_ptr, list_len, data_ptr, out_i64, vars,
        )?;
        let result_len = self.build_load(
            BasicTypeEnum::IntType(self.context.i64_type()),
            wi_alloca,
            "result_len",
        )?;
        self.build_comprehension_result(result_len.into_int_value(), out_ptr)
    }

    fn load_comprehension_input(
        &mut self,
        iter: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let iter_val = self.compile_expr(iter, vars)?;
        let list_ptr = match iter_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("comprehension iter must be a list pointer".into()),
        };
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let list_struct_ty = self.list_struct_type();
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "comp_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let list_len = self
            .build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")?
            .into_int_value();
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "comp_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_i8 = self
            .build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")?
            .into_pointer_value();
        let data_ptr = self
            .build_bit_cast(
                data_i8.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "data_i64",
            )?
            .into_pointer_value();
        Ok((list_ptr, list_len, data_ptr))
    }

    fn allocate_comprehension_output(
        &mut self,
        list_len: inkwell::values::IntValue<'ctx>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let i64_ty = self.context.i64_type();
        let elem_size = i64_ty.const_int(8, false);
        let alloc_size = self
            .builder
            .build_int_mul(list_len, elem_size, "comp_alloc")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let out_ptr = self
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(alloc_size)],
                "comp_malloc",
            )?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        let out_i64 = self
            .build_bit_cast(
                out_ptr.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "out_i64",
            )?
            .into_pointer_value();
        Ok((out_i64, out_ptr))
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_comprehension_loop(
        &mut self,
        expr: &Expr,
        var: &str,
        guard: &Option<Box<Expr>>,
        _list_ptr: inkwell::values::PointerValue<'ctx>,
        list_len: inkwell::values::IntValue<'ctx>,
        data_ptr: inkwell::values::PointerValue<'ctx>,
        out_i64: inkwell::values::PointerValue<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let i64_ty = self.context.i64_type();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for comprehension".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "comp_loop");
        let body_bb = self.context.append_basic_block(function, "comp_body");
        let done_bb = self.context.append_basic_block(function, "comp_done");
        let idx_alloca = self.build_alloca(i64_ty, "ci")?;
        let wi_alloca = self.build_alloca(i64_ty, "cw")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        self.build_store(wi_alloca, i64_ty.const_int(0, false))?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(loop_bb);
        let idx = self
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cmp, body_bb, done_bb)?;

        self.builder.position_at_end(body_bb);
        let elem_ptr = self.build_in_bounds_gep(i64_ty, data_ptr, &[idx], "elem")?;
        let elem = self.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")?;
        let mut comp_vars = vars.clone();
        let elem_alloca = self.build_alloca(i64_ty, var)?;
        self.build_store(elem_alloca, elem)?;
        comp_vars.insert(
            var.to_string(),
            (elem_alloca, BasicTypeEnum::IntType(i64_ty)),
        );

        let include = self.eval_guard(guard, &comp_vars, i64_ty)?;
        let store_bb = self.context.append_basic_block(function, "comp_store");
        let next_bb = self.context.append_basic_block(function, "comp_next");
        self.build_cond_br(include, store_bb, next_bb)?;

        self.builder.position_at_end(store_bb);
        self.emit_comprehension_store(expr, &comp_vars, out_i64, wi_alloca, i64_ty)?;
        self.build_br(next_bb)?;

        self.builder.position_at_end(next_bb);
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(idx_alloca, next)?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(done_bb);
        Ok((idx_alloca, wi_alloca))
    }

    fn eval_guard(
        &mut self,
        guard: &Option<Box<Expr>>,
        comp_vars: &HashMap<String, VarEntry<'ctx>>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let Some(g) = guard else {
            return Ok(self.context.bool_type().const_int(1, false));
        };
        let g_val = self.compile_expr(g, comp_vars)?;
        let BasicValueEnum::IntValue(iv) = g_val else {
            return Err("guard must be boolean".into());
        };
        let g_bool = self
            .builder
            .build_int_z_extend(iv, i64_ty, "g_ext")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        self.builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                g_bool,
                i64_ty.const_int(0, false),
                "g_truthy",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))
    }

    fn emit_comprehension_store(
        &mut self,
        expr: &Expr,
        comp_vars: &HashMap<String, VarEntry<'ctx>>,
        out_i64: inkwell::values::PointerValue<'ctx>,
        wi_alloca: inkwell::values::PointerValue<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> Result<(), CompileError> {
        let result = self.compile_expr(expr, comp_vars)?;
        let wi = self
            .build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "wi")?
            .into_int_value();
        let out_elem_ptr = self.build_in_bounds_gep(i64_ty, out_i64, &[wi], "out_elem")?;
        let result_i64 = match result {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::FloatValue(fv) => self
                .builder
                .build_float_to_signed_int(fv, i64_ty, "f_to_i")
                .map_err(|e| CompileError::LlvmError(format!("fptosi error: {}", e)))?,
            BasicValueEnum::PointerValue(pv) => self.build_ptr_to_int(pv, i64_ty, "p_to_i")?,
            _ => return Err("comprehension expression must produce i64-compatible value".into()),
        };
        self.build_store(out_elem_ptr, result_i64)?;
        let next_wi = self
            .builder
            .build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(wi_alloca, next_wi)?;
        Ok(())
    }

    fn build_comprehension_result(
        &self,
        result_len: inkwell::values::IntValue<'ctx>,
        out_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_struct_ty = self.list_struct_type();
        let result_alloca = self.build_alloca(list_struct_ty, "comp_result")?;
        let rlen_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 0, "rlen")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(rlen_gep, result_len)?;
        let rdata_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 1, "rdata")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let out_void = self.build_pointer_cast(out_ptr, i8_ptr, "out_void")?;
        self.build_store(rdata_gep, out_void)?;
        Ok(result_alloca.into())
    }
}
