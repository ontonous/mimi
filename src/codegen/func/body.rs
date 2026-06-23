use crate::ast::*;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    /// Shared implementation for Stmt::While — used by compile_func and compile_block
    pub(in crate::codegen) fn compile_while_stmt(
        &mut self,
        cond: &Expr,
        body: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for while".to_string()))?;
        let loop_bb = self.context.append_basic_block(function, "loop");
        let body_bb = self.context.append_basic_block(function, "loopbody");
        let merge_bb = self.context.append_basic_block(function, "loopcont");

        self.builder.build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(loop_bb);
        let cond_val = self.compile_expr(cond, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            iv
        } else {
            let fn_name = function.get_name().to_str().unwrap_or("unknown");
            return Err(CompileError::TypeMismatch(
                format!("while condition must be bool, got {} in function '{}'", cond_val.get_type(), fn_name)
            ));
        };
        self.builder.build_conditional_branch(cond_bool, body_bb, merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(body_bb);
        let old_break = self.loop_break.take();
        let old_continue = self.loop_continue.take();
        self.loop_break = Some(merge_bb);
        self.loop_continue = Some(loop_bb);
        self.compile_block(body, vars)?;
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(loop_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        }
        self.loop_break = old_break;
        self.loop_continue = old_continue;

        self.builder.position_at_end(merge_bb);
        Ok(())
    }

    /// Shared implementation for Stmt::For — used by compile_func and compile_block
    pub(in crate::codegen) fn compile_for_stmt(
        &mut self,
        var: &str,
        iterable: &Expr,
        body: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for for".to_string()))?;
        let iterable_val = self.compile_expr(iterable, vars)?;

        if let Expr::Binary(BinOp::Range, start_expr, end_expr) = iterable {
            let start_val = self.compile_expr(start_expr, vars)?;
            let end_val = self.compile_expr(end_expr, vars)?;
            let start_iv = if let BasicValueEnum::IntValue(iv) = start_val { iv } else { return Err(CompileError::TypeMismatch("range start must be i64".to_string())); };
            let end_iv = if let BasicValueEnum::IntValue(iv) = end_val { iv } else { return Err(CompileError::TypeMismatch("range end must be i64".to_string())); };

            let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
            self.builder.build_store(idx_alloca, start_iv)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

            let loop_bb = self.context.append_basic_block(function, "forloop");
            let body_bb = self.context.append_basic_block(function, "forbody");
            let merge_bb = self.context.append_basic_block(function, "forcont");

            self.builder.build_unconditional_branch(loop_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

            self.builder.position_at_end(loop_bb);
            let idx_val = self.builder.build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                idx_alloca,
                "idx"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err(CompileError::TypeMismatch("index must be i64".to_string())); };
            let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, end_iv, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
            self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

            self.builder.position_at_end(body_bb);
            let old_break = self.loop_break.take();
            let old_continue = self.loop_continue.take();
            self.loop_break = Some(merge_bb);
            self.loop_continue = Some(loop_bb);

            let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
            self.builder.build_store(elem_alloca, idx_val)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            vars.insert(var.to_string(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));

            self.compile_block(body, vars)?;

            vars.remove(var);
            self.loop_break = old_break;
            self.loop_continue = old_continue;

            let idx_val = self.builder.build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                idx_alloca,
                "idx"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err(CompileError::TypeMismatch("index must be i64".to_string())); };
            let one = self.context.i64_type().const_int(1, false);
            let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
            self.builder.build_store(idx_alloca, next_idx)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

            self.builder.build_unconditional_branch(loop_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

            self.builder.position_at_end(merge_bb);
        } else {
            let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
            let list_ptr = match iterable_val {
                BasicValueEnum::PointerValue(pv) => pv,
                BasicValueEnum::StructValue(sv) => {
                    let list_ty = BasicTypeEnum::StructType(
                        self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false)
                    );
                    let alloca = self.builder.build_alloca(list_ty, "list_alloca")
                        .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                    self.builder.build_store(alloca, sv)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    alloca
                }
                BasicValueEnum::IntValue(iv) => {
                    let int_ptr = self.builder.build_int_to_ptr(iv, i8_ptr_ty, "list_as_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("int_to_ptr error: {}", e)))?;
                    int_ptr
                }
                _ => return Err(CompileError::LlvmError("for loop requires a list or range".to_string())),
            };

            let list_struct_ty = inkwell::types::BasicTypeEnum::StructType(
                self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false)
            );
            let list_len_gep = self.gep().build_struct_gep(
                list_struct_ty,
                list_ptr,
                0,
                "list.len"
            ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let list_len = self.builder.build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                list_len_gep,
                "len"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;

            let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
            self.builder.build_store(idx_alloca, self.context.i64_type().const_int(0, false))
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

            let loop_bb = self.context.append_basic_block(function, "forloop");
            let body_bb = self.context.append_basic_block(function, "forbody");
            let merge_bb = self.context.append_basic_block(function, "forcont");

            self.builder.build_unconditional_branch(loop_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

            self.builder.position_at_end(loop_bb);
            let idx_val = self.builder.build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                idx_alloca,
                "idx"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err(CompileError::TypeMismatch("index must be i64".to_string())); };
            let len_iv = if let BasicValueEnum::IntValue(iv) = list_len { iv } else { return Err(CompileError::LlvmError("length must be i64".to_string())); };
            let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, len_iv, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
            self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

            self.builder.position_at_end(body_bb);
            let old_break = self.loop_break.take();
            let old_continue = self.loop_continue.take();
            self.loop_break = Some(merge_bb);
            self.loop_continue = Some(loop_bb);

            let data_gep = self.gep().build_struct_gep(
                list_struct_ty,
                list_ptr,
                1,
                "list.data"
            ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let data_ptr = self.builder.build_load(
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                data_gep,
                "data"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            let data_pv = if let BasicValueEnum::PointerValue(pv) = data_ptr { pv } else { return Err(CompileError::LlvmError("data must be pointer".to_string())); };

                        let elem_ptr = {
                self.gep().build_gep(
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    data_pv,
                    &[idx_iv],
                    "elem"
                )
            }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let elem = self.builder.build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                elem_ptr,
                "elem_val"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;

            let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
            self.builder.build_store(elem_alloca, elem)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            vars.insert(var.to_string(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));

            self.compile_block(body, vars)?;

            vars.remove(var);
            self.loop_break = old_break;
            self.loop_continue = old_continue;

            let idx_val = self.builder.build_load(
                BasicTypeEnum::IntType(self.context.i64_type()),
                idx_alloca,
                "idx"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err(CompileError::TypeMismatch("index must be i64".to_string())); };
            let one = self.context.i64_type().const_int(1, false);
            let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
            self.builder.build_store(idx_alloca, next_idx)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

            self.builder.build_unconditional_branch(loop_bb)
                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

            self.builder.position_at_end(merge_bb);
        }
        Ok(())
    }

    /// Shared implementation for Stmt::Assign — handles all target types
    pub(in crate::codegen) fn compile_assign_stmt(
        &mut self,
        target: &Expr,
        value: &Expr,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        match target {
            Expr::Ident(name) => {
                let val = self.compile_expr(value, vars)?;
                if let Some(&(alloca, ty)) = vars.get(name) {
                    self.assign_to_var(name, val, alloca, ty)?;
                }
            }
            Expr::Field(obj, field_name) => {
                let val = self.compile_expr(value, vars)?;
                self.compile_field_assign(obj, field_name, val, vars)?;
            }
            Expr::Index(obj, idx) => {
                let val = self.compile_expr(value, vars)?;
                self.compile_index_assign(obj, idx, val, vars)?;
            }
            Expr::Unary(crate::ast::UnOp::Deref, inner) => {
                let val = self.compile_expr(value, vars)?;
                self.compile_deref_assign(inner, val, vars)?;
            }
            _ => {
                return Err(CompileError::LlvmError(
                    format!("unsupported assignment target: {:?}", target)
                ));
            }
        }
        Ok(())
    }

    /// Assign to a field: `obj.field = val`
    pub(in crate::codegen) fn compile_field_assign(
        &mut self,
        obj: &Expr,
        field_name: &str,
        val: BasicValueEnum<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        // Check if obj is a shared variable — use heap pointer directly
        if let Expr::Ident(name) = obj {
            if self.shared_var_names.contains(name.as_str()) {
                if let Some(&(alloca, ty)) = vars.get(name.as_str()) {
                    let ptr_ty = ty.ptr_type(inkwell::AddressSpace::default());
                    let heap_ptr = self.builder.build_load(ptr_ty, alloca, &format!("{}_heap_ptr", name))
                        .map_err(|e| CompileError::LlvmError(format!("shared heap ptr load: {}", e)))?
                        .into_pointer_value();
                    let obj_type = self.infer_object_type(obj, vars);
                    return self.compile_store_field(heap_ptr, &obj_type, field_name, val);
                }
            }
        }
        let obj_val = self.compile_expr(obj, vars)?;
        let obj_type = self.infer_object_type(obj, vars);
        let field_ptr = match obj_val {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::StructValue(sv) => {
                let sty = match self.type_llvm.get(&obj_type) {
                    Some(BasicTypeEnum::StructType(s)) => *s,
                    _ => return Err(CompileError::LlvmError(
                        format!("type '{}' is not a struct", obj_type)
                    )),
                };
                let alloca = self.builder.build_alloca(sty, "tmp")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, sv)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                alloca
            }
            _ => return Err(CompileError::LlvmError(
                "field assign requires a struct".to_string()
            )),
        };
        let sty = match self.type_llvm.get(&obj_type) {
            Some(BasicTypeEnum::StructType(s)) => *s,
            _ => return Err(CompileError::LlvmError(
                format!("type '{}' is not a struct", obj_type)
            )),
        };
        if let Some(td) = self.type_defs.get(&obj_type) {
            if let TypeDefKind::Record(fields) = &td.kind {
                if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                    let gep = self.gep().build_struct_gep(sty, field_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    self.builder.build_store(gep, val)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    return Ok(());
                }
            }
        }
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self.gep().build_struct_gep(sty, field_ptr, idx, field_name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.builder.build_store(gep, val)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            return Ok(());
        }
        Err(CompileError::LlvmError(
            format!("field '{}' not found on type '{}'", field_name, obj_type)
        ))
    }

    /// Store a value into a struct field given a struct pointer and field name.
    /// Shared helper used by compile_field_assign and shared var field assignment.
    fn compile_store_field(
        &mut self,
        struct_ptr: inkwell::values::PointerValue<'ctx>,
        obj_type: &str,
        field_name: &str,
        val: BasicValueEnum<'ctx>,
    ) -> MimiResult<()> {
        let sty = match self.type_llvm.get(obj_type) {
            Some(BasicTypeEnum::StructType(s)) => *s,
            _ => return Err(CompileError::LlvmError(
                format!("type '{}' is not a struct", obj_type)
            )),
        };
        if let Some(td) = self.type_defs.get(obj_type) {
            if let TypeDefKind::Record(fields) = &td.kind {
                if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                    let gep = self.gep().build_struct_gep(sty, struct_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    self.builder.build_store(gep, val)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    return Ok(());
                }
            }
        }
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self.gep().build_struct_gep(sty, struct_ptr, idx, field_name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.builder.build_store(gep, val)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            return Ok(());
        }
        Err(CompileError::LlvmError(
            format!("field '{}' not found on type '{}'", field_name, obj_type)
        ))
    }

    /// Assign to an index: `list[i] = val`
    pub(in crate::codegen) fn compile_index_assign(
        &mut self,
        obj: &Expr,
        idx: &Expr,
        val: BasicValueEnum<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let obj_val = self.compile_expr(obj, vars)?;
        let idx_val = self.compile_expr(idx, vars)?;
        let idx_iv = match idx_val {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err(CompileError::LlvmError("index must be i64".to_string())),
        };
        let list_ptr = match obj_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::LlvmError("index assign requires a list pointer".to_string())),
        };
        let list_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(self.context.i64_type()),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let data_gep = self.gep().build_struct_gep(list_ty, list_ptr, 1, "list.data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_ptr = self.builder.build_load(
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            data_gep, "data"
        ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
            self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
            "data_i64")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
                let elem_ptr = {
            self.gep().build_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
        }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(elem_ptr, val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(())
    }

    /// Assign through a dereference: `*ptr = val`
    pub(in crate::codegen) fn compile_deref_assign(
        &mut self,
        inner: &Expr,
        val: BasicValueEnum<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        // Check if inner is a shared variable — use heap pointer directly
        if let Expr::Ident(name) = inner {
            if self.shared_var_names.contains(name.as_str()) {
                if let Some(&(alloca, ty)) = vars.get(name.as_str()) {
                    let ptr_ty = ty.ptr_type(inkwell::AddressSpace::default());
                    let heap_ptr = self.builder.build_load(ptr_ty, alloca, &format!("{}_heap_ptr", name))
                        .map_err(|e| CompileError::LlvmError(format!("shared heap ptr load: {}", e)))?
                        .into_pointer_value();
                    self.builder.build_store(heap_ptr, val)
                        .map_err(|e| CompileError::LlvmError(format!("deref shared store error: {}", e)))?;
                    return Ok(());
                }
            }
        }
        let ptr_val = self.compile_expr(inner, vars)?;
        let ptr = match ptr_val {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::IntValue(iv) => {
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                self.builder.build_int_to_ptr(iv, i8_ptr_ty, "ptr_cast")
                    .map_err(|e| CompileError::LlvmError(format!("int_to_ptr error: {}", e)))?
            }
            _ => return Err(CompileError::LlvmError("deref assign requires a pointer".to_string())),
        };
        self.builder.build_store(ptr, val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(())
    }
}
