use crate::ast::*;
use crate::error::{CompileError, MimiResult};
use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
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
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for while".to_string())
        })?;
        let loop_bb = self.context.append_basic_block(function, "loop");
        let body_bb = self.context.append_basic_block(function, "loopbody");
        let merge_bb = self.context.append_basic_block(function, "loopcont");

        self.build_br(loop_bb)?;

        self.builder.position_at_end(loop_bb);
        let cond_val = self.compile_expr(cond, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            iv
        } else {
            let fn_name = function.get_name().to_str().unwrap_or("unknown");
            return Err(CompileError::TypeMismatch(format!(
                "while condition must be bool, got {} in function '{}'",
                cond_val.get_type(),
                fn_name
            )));
        };
        self.build_cond_br(cond_bool, body_bb, merge_bb)?;

        self.builder.position_at_end(body_bb);
        self.emit_loop_body_block(body, vars, loop_bb, merge_bb, |_, _| Ok(()), |_, _| Ok(()))?;

        self.builder.position_at_end(merge_bb);
        Ok(())
    }

    /// Shared implementation for Stmt::WhileLet — pattern-matched loop
    pub(in crate::codegen) fn compile_while_let_stmt(
        &mut self,
        pat: &Pattern,
        init: &Expr,
        body: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for while-let".to_string())
        })?;
        let loop_bb = self.context.append_basic_block(function, "while_let_loop");
        let merge_bb = self.context.append_basic_block(function, "while_let_end");
        let type_bb = self.context.append_basic_block(function, "while_let_type");

        self.build_br(loop_bb)?;

        // Loop header: evaluate init and check pattern match
        self.builder.position_at_end(loop_bb);
        let init_val = self.compile_expr(init, vars)?;

        // Check if pattern matches
        let matches = self.compile_pattern_check(pat, &init_val, vars)?;
        self.build_cond_br(matches, type_bb, merge_bb)?;

        // Type-checking block: rebind pattern variables and run body
        self.builder.position_at_end(type_bb);
        self.emit_loop_body_block(
            body,
            vars,
            loop_bb,
            merge_bb,
            |slf, vs| slf.compile_pattern_bind(pat, init_val, vs),
            |_, _| Ok(()),
        )?;

        self.builder.position_at_end(merge_bb);
        Ok(())
    }

    /// Shared implementation for Stmt::Loop — infinite loop with break
    pub(in crate::codegen) fn compile_loop_stmt(
        &mut self,
        body: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for loop".to_string())
        })?;
        let loop_bb = self.context.append_basic_block(function, "loop");
        let body_bb = self.context.append_basic_block(function, "loopbody");
        let merge_bb = self.context.append_basic_block(function, "loopcont");

        self.build_br(loop_bb)?;

        // Loop condition block (always true)
        self.builder.position_at_end(loop_bb);
        let true_val = self.context.bool_type().const_int(1, false);
        self.build_cond_br(true_val, body_bb, merge_bb)?;

        self.builder.position_at_end(body_bb);
        self.emit_loop_body_block(body, vars, loop_bb, merge_bb, |_, _| Ok(()), |_, _| Ok(()))?;

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
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());

        if let Expr::Binary(BinOp::Range, start_expr, end_expr) = iterable {
            let start_val = self.compile_expr(start_expr, vars)?;
            let end_val = self.compile_expr(end_expr, vars)?;
            let start_iv = Self::expect_int_value(start_val, "range start must be i64")?;
            let end_iv = Self::expect_int_value(end_val, "range end must be i64")?;

            let (idx_alloca, loop_bb, body_bb, merge_bb) = self.build_for_index_header(start_iv)?;

            self.builder.position_at_end(loop_bb);
            self.build_for_index_condition(idx_alloca, end_iv, body_bb, merge_bb)?;

            self.builder.position_at_end(body_bb);
            self.emit_loop_body_block(
                body,
                vars,
                loop_bb,
                merge_bb,
                |slf, vs| {
                    let idx_val = slf.build_load(i64_ty, idx_alloca, "idx")?;
                    let idx_iv = Self::expect_int_value(idx_val, "index must be i64")?;
                    let elem_alloca = slf.build_alloca(i64_ty, var)?;
                    slf.build_store(elem_alloca, idx_iv)?;
                    vs.insert(var.to_string(), (elem_alloca, i64_ty));
                    Ok(())
                },
                |slf, vs| {
                    vs.remove(var);
                    slf.build_for_index_increment(idx_alloca)
                },
            )?;

            self.builder.position_at_end(merge_bb);
            return Ok(());
        }

        let iterable_val = self.compile_expr(iterable, vars)?;
        let list_ptr = self.coerce_iterable_to_list_ptr(iterable_val)?;

        let (idx_alloca, len_iv, loop_bb, body_bb, merge_bb) =
            self.build_for_list_header(list_ptr)?;

        self.builder.position_at_end(loop_bb);
        self.build_for_list_condition(idx_alloca, len_iv, body_bb, merge_bb)?;

        // G-41 fix: determine element type from iterable expression
        let elem_is_string = self.is_string_list_iterable(iterable, vars);

        self.builder.position_at_end(body_bb);
        self.emit_loop_body_block(
            body,
            vars,
            loop_bb,
            merge_bb,
            |slf, vs| {
                let idx_val = slf.build_load(i64_ty, idx_alloca, "idx")?;
                let idx_iv = Self::expect_int_value(idx_val, "index must be i64")?;
                slf.bind_for_list_element(var, list_ptr, idx_iv, elem_is_string, iterable, vs)?;
                Ok(())
            },
            |slf, vs| {
                vs.remove(var);
                slf.build_for_index_increment(idx_alloca)
            },
        )?;

        self.builder.position_at_end(merge_bb);
        Ok(())
    }

    /// G-41 helper: determine if a for-loop iterable produces string elements.
    /// Checks var_types, var_type_names, and known builtin function names.
    fn is_string_list_iterable(
        &self,
        iterable: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> bool {
        // Direct function call: known List<string> producers
        if let Expr::Call(callee, _) = iterable {
            if let Expr::Ident(name) = callee.as_ref() {
                match name.as_str() {
                    "listdir" | "walk_dir" | "str_split" | "words" | "lines" | "split" | "keys" => {
                        return true;
                    }
                    _ => {}
                }
            }
        }
        // Variable reference: check var_types and var_type_names
        if let Expr::Ident(name) = iterable {
            // Check Type object
            if let Some(ty) = self.var_types.get(name) {
                match ty {
                    Type::Name(n, args) if n == "List" && !args.is_empty() => {
                        return matches!(&args[0], Type::Name(inner, _) if inner == "string");
                    }
                    _ => {}
                }
            }
            // Check string-based type name (populated by let-binding codegen)
            if let Some(tn) = self.var_type_names.get(name) {
                if tn == "List<string>" {
                    return true;
                }
            }
        }
        // General fallback: use the expression's inferred type.
        if let Some(Type::Name(n, args)) = self.expr_type_of(iterable, vars) {
            if n == "List" && !args.is_empty() {
                return matches!(&args[0], Type::Name(inner, _) if inner == "string");
            }
        }
        false
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
                let val = self.normalize_string_value(val, value)?;
                if let Some(&(alloca, ty)) = vars.get(name) {
                    let is_string_val = self
                        .var_type_names
                        .get(name)
                        .map(|t| t == "string")
                        .unwrap_or(false);
                    let is_temp = matches!(
                        value,
                        Expr::Binary(BinOp::Add, _, _) | Expr::Literal(Lit::FString(_))
                    );
                    if is_string_val && is_temp {
                        self.pop_last_heap_ptr();
                        if let BasicTypeEnum::StructType(st) = ty {
                            if st.get_field_types().len() == 2 {
                                self.register_heap_slot_root(alloca, st, 0);
                            }
                        }
                    }
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
                return Err(CompileError::LlvmError(format!(
                    "unsupported assignment target: {:?}",
                    target
                )));
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
                if let Some(&(alloca, _ty)) = vars.get(name.as_str()) {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let heap_ptr = self
                        .build_load(ptr_ty, alloca, &format!("{}_heap_ptr", name))?
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
                    _ => {
                        return Err(CompileError::LlvmError(format!(
                            "type '{}' is not a struct",
                            obj_type
                        )))
                    }
                };
                let alloca = self.build_alloca(sty, "tmp")?;
                self.build_store(alloca, sv)?;
                alloca
            }
            _ => {
                return Err(CompileError::LlvmError(
                    "field assign requires a struct".to_string(),
                ))
            }
        };
        self.compile_store_field(field_ptr, &obj_type, field_name, val)
    }

    /// Store a value into a struct field given a struct pointer and field name.
    /// Shared helper used by compile_field_assign and shared var field assignment.
    fn compile_store_field(
        &mut self,
        struct_ptr: PointerValue<'ctx>,
        obj_type: &str,
        field_name: &str,
        val: BasicValueEnum<'ctx>,
    ) -> MimiResult<()> {
        let sty = match self.type_llvm.get(obj_type) {
            Some(BasicTypeEnum::StructType(s)) => *s,
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "type '{}' is not a struct",
                    obj_type
                )))
            }
        };
        if let Some(td) = self.type_defs.get(obj_type) {
            if let TypeDefKind::Record(fields) = &td.kind {
                if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                    let gep = self
                        .gep()
                        .build_struct_gep(sty, struct_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    self.build_store(gep, val)?;
                    return Ok(());
                }
            }
        }
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self
                .gep()
                .build_struct_gep(sty, struct_ptr, idx, field_name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(gep, val)?;
            return Ok(());
        }
        Err(CompileError::LlvmError(format!(
            "field '{}' not found on type '{}'",
            field_name, obj_type
        )))
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
            _ => {
                return Err(CompileError::LlvmError(
                    "index assign requires a list pointer".to_string(),
                ))
            }
        };
        let list_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i64_type()),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            ],
            false,
        );
        // Bounds check before access
        self.check_list_bounds(list_ptr, idx_iv, "index assign")?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_ptr, 1, "list.data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_ptr = self
            .build_load(
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                data_gep,
                "data",
            )?
            .into_pointer_value();
        let data_ptr_i64 = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(data_ptr),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                "data_i64",
            )?
            .into_pointer_value();
        let elem_ptr = {
            self.gep()
                .build_in_bounds_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(elem_ptr, val)?;
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
                if let Some(&(alloca, _ty)) = vars.get(name.as_str()) {
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let heap_ptr = self
                        .build_load(ptr_ty, alloca, &format!("{}_heap_ptr", name))?
                        .into_pointer_value();
                    self.build_store(heap_ptr, val)?;
                    return Ok(());
                }
            }
        }
        let ptr_val = self.compile_expr(inner, vars)?;
        let ptr = match ptr_val {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::IntValue(iv) => {
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                self.builder
                    .build_int_to_ptr(iv, i8_ptr_ty, "ptr_cast")
                    .map_err(|e| CompileError::LlvmError(format!("int_to_ptr error: {}", e)))?
            }
            _ => {
                return Err(CompileError::LlvmError(
                    "deref assign requires a pointer".to_string(),
                ))
            }
        };
        self.build_store(ptr, val)?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Loop helper methods
    // -------------------------------------------------------------------------

    /// Emit a loop body block: set break/continue targets, compile the body,
    /// and add the back-edge when the block is not already terminated.
    /// `before_body` is called after the break/continue targets are set up but
    /// before the body is compiled (e.g. to bind the loop variable). `after_body`
    /// is called after the body is compiled and before the unconditional branch
    /// back to the loop header (e.g. to increment an index variable).
    fn emit_loop_body_block<Before, After>(
        &mut self,
        body: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
        loop_header: BasicBlock<'ctx>,
        merge_bb: BasicBlock<'ctx>,
        before_body: Before,
        after_body: After,
    ) -> MimiResult<()>
    where
        Before: FnOnce(&mut Self, &mut HashMap<String, VarEntry<'ctx>>) -> MimiResult<()>,
        After: FnOnce(&mut Self, &mut HashMap<String, VarEntry<'ctx>>) -> MimiResult<()>,
    {
        let old_break = self.loop_break.take();
        let old_continue = self.loop_continue.take();
        self.loop_break = Some(merge_bb);
        self.loop_continue = Some(loop_header);
        before_body(self, vars)?;
        self.compile_block(body, vars)?;
        if !self.block_has_terminator() {
            after_body(self, vars)?;
            self.build_br(loop_header)?;
        }
        self.loop_break = old_break;
        self.loop_continue = old_continue;
        Ok(())
    }

    /// Coerce an iterable expression value into a list pointer.
    fn coerce_iterable_to_list_ptr(
        &self,
        iterable_val: BasicValueEnum<'ctx>,
    ) -> MimiResult<PointerValue<'ctx>> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        match iterable_val {
            BasicValueEnum::PointerValue(pv) => Ok(pv),
            BasicValueEnum::StructValue(sv) => {
                let list_ty = self.list_struct_basic_type();
                let alloca = self.build_alloca(list_ty, "list_alloca")?;
                self.build_store(alloca, sv)?;
                Ok(alloca)
            }
            BasicValueEnum::IntValue(iv) => {
                let int_ptr = self
                    .builder
                    .build_int_to_ptr(iv, i8_ptr_ty, "list_as_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("int_to_ptr error: {}", e)))?;
                Ok(int_ptr)
            }
            _ => Err(CompileError::LlvmError(
                "for loop requires a list or range".to_string(),
            )),
        }
    }

    /// Build the LLVM type used to represent a Mimi list: `{ i64, i8** }`.
    fn list_struct_basic_type(&self) -> BasicTypeEnum<'ctx> {
        BasicTypeEnum::StructType(self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i64_type()),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            ],
            false,
        ))
    }

    /// Build the loop header for a numeric range for-loop.
    /// Returns `(idx_alloca, loop_bb, body_bb, merge_bb)`.
    fn build_for_index_header(
        &self,
        start_iv: IntValue<'ctx>,
    ) -> MimiResult<(
        PointerValue<'ctx>,
        BasicBlock<'ctx>,
        BasicBlock<'ctx>,
        BasicBlock<'ctx>,
    )> {
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for for".to_string())
        })?;
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        let idx_alloca = self.build_alloca(i64_ty, "idx")?;
        self.build_store(idx_alloca, start_iv)?;
        let loop_bb = self.context.append_basic_block(function, "forloop");
        let body_bb = self.context.append_basic_block(function, "forbody");
        let merge_bb = self.context.append_basic_block(function, "forcont");
        self.build_br(loop_bb)?;
        Ok((idx_alloca, loop_bb, body_bb, merge_bb))
    }

    /// Build the loop condition for a numeric range for-loop.
    fn build_for_index_condition(
        &self,
        idx_alloca: PointerValue<'ctx>,
        end_iv: IntValue<'ctx>,
        body_bb: BasicBlock<'ctx>,
        merge_bb: BasicBlock<'ctx>,
    ) -> MimiResult<()> {
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        let idx_val = self.build_load(i64_ty, idx_alloca, "idx")?;
        let idx_iv = Self::expect_int_value(idx_val, "index must be i64")?;
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx_iv, end_iv, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cmp, body_bb, merge_bb)
    }

    /// Build the loop header for a list for-loop.
    /// Returns `(idx_alloca, len_iv, loop_bb, body_bb, merge_bb)`.
    fn build_for_list_header(
        &self,
        list_ptr: PointerValue<'ctx>,
    ) -> MimiResult<(
        PointerValue<'ctx>,
        IntValue<'ctx>,
        BasicBlock<'ctx>,
        BasicBlock<'ctx>,
        BasicBlock<'ctx>,
    )> {
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for for".to_string())
        })?;
        let list_struct_ty = self.list_struct_basic_type();
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        let list_len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "list.len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let list_len = self.build_load(i64_ty, list_len_gep, "len")?;
        let len_iv = match list_len {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err(CompileError::LlvmError("length must be i64".to_string())),
        };
        let idx_alloca = self.build_alloca(i64_ty, "idx")?;
        self.build_store(idx_alloca, self.context.i64_type().const_int(0, false))?;
        let loop_bb = self.context.append_basic_block(function, "forloop");
        let body_bb = self.context.append_basic_block(function, "forbody");
        let merge_bb = self.context.append_basic_block(function, "forcont");
        self.build_br(loop_bb)?;
        Ok((idx_alloca, len_iv, loop_bb, body_bb, merge_bb))
    }

    /// Build the loop condition for a list for-loop.
    fn build_for_list_condition(
        &self,
        idx_alloca: PointerValue<'ctx>,
        len_iv: IntValue<'ctx>,
        body_bb: BasicBlock<'ctx>,
        merge_bb: BasicBlock<'ctx>,
    ) -> MimiResult<()> {
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        let idx_val = self.build_load(i64_ty, idx_alloca, "idx")?;
        let idx_iv = Self::expect_int_value(idx_val, "index must be i64")?;
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx_iv, len_iv, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cmp, body_bb, merge_bb)
    }

    /// Increment the loop index by one and store it back to `idx_alloca`.
    fn build_for_index_increment(&self, idx_alloca: PointerValue<'ctx>) -> MimiResult<()> {
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
        let idx_val = self.build_load(i64_ty, idx_alloca, "idx")?;
        let idx_iv = Self::expect_int_value(idx_val, "index must be i64")?;
        let one = self.context.i64_type().const_int(1, false);
        let next_idx = self
            .builder
            .build_int_add(idx_iv, one, "next_idx")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(idx_alloca, next_idx)
    }

    /// Extract the current list element (string or non-string) and bind it to
    /// the loop variable. For struct-typed list elements (User, Config, etc.),
    /// converts from the stored i64 pointer back to the struct via inttoptr+load.
    fn bind_for_list_element(
        &mut self,
        var: &str,
        list_ptr: PointerValue<'ctx>,
        idx_iv: IntValue<'ctx>,
        elem_is_string: bool,
        iterable: &Expr,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_struct_ty = self.list_struct_basic_type();
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "list.data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_ptr = self.build_load(BasicTypeEnum::PointerType(i8_ptr_ty), data_gep, "data")?;
        let data_pv = match data_ptr {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err(CompileError::LlvmError("data must be pointer".to_string())),
        };

        if elem_is_string {
            // String list: data contains *mut c_char pointers
            // Load element as pointer, then wrap into Mimi string struct
            let elem_ptr = self
                .gep()
                .build_in_bounds_gep(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    data_pv,
                    &[idx_iv],
                    "elem",
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let raw_str_ptr = match self.build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                elem_ptr,
                "raw_str_ptr",
            )? {
                BasicValueEnum::PointerValue(pv) => pv,
                _ => {
                    return Err(CompileError::LlvmError(
                        "raw_str_ptr must be pointer".to_string(),
                    ))
                }
            };
            // Wrap C string into Mimi string struct {ptr, len}
            let mimi_str = self.wrap_c_string(raw_str_ptr)?;
            let str_ty = mimi_str.get_type();
            let elem_alloca = self.build_alloca(str_ty, var)?;
            self.build_store(elem_alloca, mimi_str)?;
            vars.insert(var.to_string(), (elem_alloca, str_ty));
        } else {
            // Non-string list: elements are i64 values
            let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
            let elem_ptr = self
                .gep()
                .build_in_bounds_gep(i64_ty, data_pv, &[idx_iv], "elem")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let elem = self.build_load(i64_ty, elem_ptr, "elem_val")?;
            // Try to convert i64 element to proper struct type (e.g. User struct)
            let converted_elem =
                self.try_convert_loop_elem(elem.into_int_value(), iterable, vars)?;
            if let Some(converted) = converted_elem {
                let elem_ty = converted.get_type();
                let elem_alloca = self.build_alloca(elem_ty, var)?;
                self.build_store(elem_alloca, converted)?;
                vars.insert(var.to_string(), (elem_alloca, elem_ty));
                // Track type name for field access
                if let Some(tn) = self
                    .infer_object_type(iterable, vars)
                    .strip_prefix("List<")
                    .and_then(|s| s.strip_suffix('>'))
                    .map(|s| s.to_string())
                {
                    if !matches!(
                        tn.as_str(),
                        "i32" | "i64" | "f32" | "f64" | "bool" | "string"
                    ) {
                        self.var_type_names.insert(var.to_string(), tn);
                    }
                }
            } else {
                let elem_alloca = self.build_alloca(i64_ty, var)?;
                self.build_store(elem_alloca, elem)?;
                vars.insert(var.to_string(), (elem_alloca, i64_ty));
            }
        }
        Ok(())
    }

    /// Try to convert a for-loop element from i64 to its proper struct type.
    /// For `for x in list`, if `list` is `List<User>`, each element is stored
    /// as a ptrtoint i64 in the data array. This method reconstructs the struct
    /// via inttoptr + load.
    fn try_convert_loop_elem(
        &self,
        elem_int: inkwell::values::IntValue<'ctx>,
        iterable: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<Option<BasicValueEnum<'ctx>>> {
        // Resolve the list element type from either var_types or var_type_names.
        let elem_ty = self.resolve_loop_elem_type(iterable, vars);
        let Some(elem_ty) = elem_ty else {
            return Ok(None);
        };
        // Skip conversion for known scalar element types
        if let Type::Name(inner, _) = &elem_ty {
            if matches!(
                inner.as_str(),
                "i32" | "i64" | "f32" | "f64" | "bool" | "string"
            ) {
                return Ok(None);
            }
        }
        // Resolve generic param (e.g., T→Item) via type_map if elem is a single
        // uppercase letter (generic placeholder from trait method self type).
        let concrete_ty = match &elem_ty {
            Type::Name(inner, _)
                if inner.len() == 1 && inner.chars().next().is_some_and(|c| c.is_uppercase()) =>
            {
                let resolved = self.type_map.get(inner.as_str()).cloned();
                resolved.unwrap_or_else(|| elem_ty.clone())
            }
            _ => elem_ty.clone(),
        };
        if let Some(BasicTypeEnum::StructType(sty)) = self.llvm_type_for(&concrete_ty) {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let elem_ptr = self.build_int_to_ptr(elem_int, ptr_ty, "loop_elem_ptr")?;
            let struct_val =
                self.build_load(BasicTypeEnum::StructType(sty), elem_ptr, "loop_elem_struct")?;
            return Ok(Some(struct_val));
        }
        Ok(None)
    }

    /// Resolve the element type of a list expression, handling both generic
    /// trait methods (self: &List<T>) via var_types and concrete types via
    /// infer_object_type.
    fn resolve_loop_elem_type(
        &self,
        iterable: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Option<Type> {
        // Check var_types first (handles Type::Ref for generic trait method self)
        if let Expr::Ident(name) = iterable {
            if let Some(ty) = self.var_types.get(name) {
                let inner = match ty {
                    Type::Name(n, args) if n == "List" && args.len() == 1 => Some(&args[0]),
                    Type::Ref(_, ref_inner) | Type::RefMut(_, ref_inner) => {
                        if let Type::Name(n, args) = ref_inner.as_ref() {
                            if n == "List" && args.len() == 1 {
                                Some(&args[0])
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(elem) = inner {
                    // Try to resolve generic param (e.g., T → Item) via type_map
                    if let Type::Name(elem_name, _) = elem {
                        if let Some(resolved) = self.type_map.get(elem_name) {
                            return Some(resolved.clone());
                        }
                    }
                    return Some(elem.clone());
                }
            }
        }
        // Fallback: parse infer_object_type result (e.g. "List<User>" -> User)
        let obj_type = self.infer_object_type(iterable, vars);
        let inner = obj_type.strip_prefix("List<").map(|s| {
            let mut depth = 0u32;
            for (i, ch) in s.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' if depth == 0 => return s[..i].trim().to_string(),
                    '>' => depth -= 1,
                    _ => {}
                }
            }
            s.trim().to_string()
        })?;
        Some(Type::Name(inner, vec![]))
    }

    /// Extract an `IntValue` from a `BasicValueEnum`, failing with
    /// `TypeMismatch` if the value is not an integer.
    fn expect_int_value(val: BasicValueEnum<'ctx>, msg: &str) -> MimiResult<IntValue<'ctx>> {
        match val {
            BasicValueEnum::IntValue(iv) => Ok(iv),
            _ => Err(CompileError::TypeMismatch(msg.to_string())),
        }
    }
}
