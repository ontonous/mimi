#![allow(dead_code, deprecated)]

use crate::ast::*;
use crate::codegen::types;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_async_func(&mut self, func: &FuncDef) -> Result<(), String> {
        // 1. Compile the actual body as a hidden function
        let body_name = format!("{}__async_body", func.name);
        let body_func = FuncDef {
            name: body_name,
            commitment: func.commitment,
            pub_: false,
            params: func.params.clone(),
            ret: func.ret.clone(),
            body: func.body.clone(),
            where_clause: None,
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
        };
        self.compile_func(&body_func)?;

        // 2. Compile the public spawner: func name(args) -> i64 { spawn name__async_body(args) }
        // Build call args: name__async_body(arg1, arg2, ...)
        let call_args: Vec<Expr> = func.params.iter().map(|p| {
            Expr::Ident(p.name.clone())
        }).collect();
        let spawn_body = Expr::Spawn(Box::new(
            Expr::Call(
                Box::new(Expr::Ident(format!("{}__async_body", func.name))),
                call_args,
            )
        ));
        let spawner_func = FuncDef {
            name: func.name.clone(),
            commitment: func.commitment,
            pub_: func.pub_,
            params: func.params.clone(),
            ret: Some(Type::Name("i64".into(), vec![])),
            body: vec![Stmt::Expr(spawn_body)],
            where_clause: None,
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
        };
        self.compile_func(&spawner_func)?;
        Ok(())
    }

    pub(super) fn compile_func(&mut self, func: &FuncDef) -> Result<(), String> {
        // Delegate async funcs to compile_async_func
        if func.is_async {
            return self.compile_async_func(func);
        }
        let ret_type = match &func.ret {
            Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let mut param_types = Vec::new();
        for param in &func.params {
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &param.ty) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let function = self.module.add_function(&func.name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // Push scopes for function body
        self.push_cap_scope();
        self.push_comp_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &param.ty) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).ok_or_else(|| "param index matches function signature".to_string())?)
                    .map_err(|e| format!("store error: {}", e))?;
                vars.insert(param.name.clone(), (alloca, ty));
                
                // Track type name for method dispatch
                if let Type::Name(tn, _) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), tn.clone());
                }
                if let Type::DynTrait(_) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&param.ty));
                }
                if let Type::ImplTrait(_) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&param.ty));
                }
                
                // Track capability parameters
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }

        // Compile requires contracts as runtime asserts when verify_contracts is enabled
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    self.compile_contract_assert(expr, &vars, &format!("requires violation in '{}'", func.name))?;
                }
            }
        }

        let mut last_val: BasicValueEnum = self.context.i64_type().const_int(0, false).into();
        for stmt in &func.body {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(&mut vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    last_val = self.compile_expr(expr, &vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    self.pop_comp_scope();
                    let val = self.compile_expr(expr, &vars)?;
                    self.builder.build_return(Some(&val)).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.pop_comp_scope();
                    self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), ty, .. } => {
                    let val = self.compile_expr(init, &vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &name)
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("store error: {}", e))?;
                    // Track type name from explicit annotation or record expression
                    if let Some(Type::Name(tn, _)) = ty {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    } else if let Expr::Record { ty: Some(tn), .. } = init {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    }
                    vars.insert(name.clone(), (alloca, llvm_ty));
                    
                    // Track capability variables
                    if let Some(Type::Cap(_)) = &ty {
                        self.register_cap(&name, alloca);
                    }
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, &vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| format!("store error: {}", e))?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("[E0712] if condition must be boolean".into());
                    };

                    let function = self.current_function().ok_or_else(|| "codegen: no current function for if".to_string())?;
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Then block
                    let then_val = {
                        self.builder.position_at_end(then_bb);
                        let mut then_vars = vars.clone();
                        let v = self.compile_block_last_val(then_, &mut then_vars)?;
                        let current = self.builder.get_insert_block().ok_or_else(|| "codegen: no insert block for then block".to_string())?;
                        if current.get_terminator().is_none() {
                            self.builder.build_unconditional_branch(merge_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                        }
                        v
                    };
                    let then_bb_end = self.builder.get_insert_block().ok_or_else(|| "codegen: no insert block after then".to_string())?;

                    // Else block
                    let else_val = {
                        self.builder.position_at_end(else_bb);
                        if let Some(else_block) = else_ {
                            let mut else_vars = vars.clone();
                            let v = self.compile_block_last_val(else_block, &mut else_vars)?;
                            let current = self.builder.get_insert_block().ok_or_else(|| "codegen: no insert block for else block".to_string())?;
                            if current.get_terminator().is_none() {
                                self.builder.build_unconditional_branch(merge_bb)
                                    .map_err(|e| format!("branch error: {}", e))?;
                            }
                            v
                        } else {
                            self.context.i64_type().const_int(0, false).into()
                        }
                    };
                    let else_bb_end = self.builder.get_insert_block().ok_or_else(|| "codegen: no insert block after else".to_string())?;
                    // No-else case: else_bb has no terminator yet — supply one
                    if else_bb_end.get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }

                    // Continue at merge, produce phi if both branches have values
                    self.builder.position_at_end(merge_bb);
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self.builder.build_phi(then_val.get_type(), "if_result")
                            .map_err(|e| format!("phi error: {}", e))?;
                        phi.add_incoming(&[
                            (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
                            (&else_val as &dyn inkwell::values::BasicValue, else_bb_end),
                        ]);
                        last_val = phi.as_basic_value();
                    }
                }
                Stmt::While { cond, body } => {
                    let function = self.current_function().ok_or_else(|| "codegen: no current function for while".to_string())?;
                    let loop_bb = self.context.append_basic_block(function, "loop");
                    let body_bb = self.context.append_basic_block(function, "loopbody");
                    let merge_bb = self.context.append_basic_block(function, "loopcont");

                    // Jump to loop condition check
                    self.builder.build_unconditional_branch(loop_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Loop condition
                    self.builder.position_at_end(loop_bb);
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err("[E0712] while condition must be boolean".into());
                    };
                    self.builder.build_conditional_branch(cond_bool, body_bb, merge_bb)
                        .map_err(|e| format!("branch error: {}", e))?;

                    // Loop body
                    self.builder.position_at_end(body_bb);
                    let old_break = self.loop_break.take();
                    let old_continue = self.loop_continue.take();
                    self.loop_break = Some(merge_bb);
                    self.loop_continue = Some(loop_bb);
                    self.compile_block(body, &mut vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                    }
                    self.loop_break = old_break;
                    self.loop_continue = old_continue;

                    // Continue after loop
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::For { var, iterable, body } => {
                    let function = self.current_function().ok_or_else(|| "codegen: no current function for for".to_string())?;
                    let iterable_val = self.compile_expr(iterable, &vars)?;

                    if let Expr::Binary(BinOp::Range, start_expr, end_expr) = iterable {
                        let start_val = self.compile_expr(start_expr, &vars)?;
                        let end_val = self.compile_expr(end_expr, &vars)?;
                        let start_iv = if let BasicValueEnum::IntValue(iv) = start_val { iv } else { return Err("[E0712] range start must be i64".into()); };
                        let end_iv = if let BasicValueEnum::IntValue(iv) = end_val { iv } else { return Err("[E0712] range end must be i64".into()); };

                        let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(idx_alloca, start_iv)
                            .map_err(|e| format!("store error: {}", e))?;

                        let loop_bb = self.context.append_basic_block(function, "forloop");
                        let body_bb = self.context.append_basic_block(function, "forbody");
                        let merge_bb = self.context.append_basic_block(function, "forcont");

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(loop_bb);
                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, end_iv, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(body_bb);
                        let old_break = self.loop_break.take();
                        let old_continue = self.loop_continue.take();
                        self.loop_break = Some(merge_bb);
                        self.loop_continue = Some(loop_bb);

                        let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(elem_alloca, idx_val)
                            .map_err(|e| format!("store error: {}", e))?;
                        vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));

                        self.compile_block(body, &mut vars)?;

                        vars.remove(var);
                        self.loop_break = old_break;
                        self.loop_continue = old_continue;

                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let one = self.context.i64_type().const_int(1, false);
                        let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next_idx)
                            .map_err(|e| format!("store error: {}", e))?;

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(merge_bb);
                    } else {
                        // Handle list iteration: accept both PointerValue (inline list)
                        // and IntValue (list parameter passed as opaque i64 pointer)
                        let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let list_ptr = match iterable_val {
                            BasicValueEnum::PointerValue(pv) => pv,
                            BasicValueEnum::IntValue(iv) => {
                                // Cast i64 (opaque pointer) to struct pointer
                                let int_ptr = self.builder.build_int_to_ptr(iv, i8_ptr_ty, "list_as_ptr")
                                    .map_err(|e| format!("int_to_ptr error: {}", e))?;
                                int_ptr
                            }
                            _ => return Err("for loop requires a list or range".into()),
                        };

                        let list_struct_ty = inkwell::types::BasicTypeEnum::StructType(
                            self.context.struct_type(&[
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                            ], false)
                        );
                        let list_len_gep = self.builder.build_struct_gep(
                            list_struct_ty,
                            list_ptr,
                            0,
                            "list.len"
                        ).map_err(|e| format!("gep error: {}", e))?;
                        let list_len = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            list_len_gep,
                            "len"
                        ).map_err(|e| format!("load error: {}", e))?;

                        let idx_alloca = self.builder.build_alloca(self.context.i64_type(), "idx")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(idx_alloca, self.context.i64_type().const_int(0, false))
                            .map_err(|e| format!("store error: {}", e))?;

                        let loop_bb = self.context.append_basic_block(function, "forloop");
                        let body_bb = self.context.append_basic_block(function, "forbody");
                        let merge_bb = self.context.append_basic_block(function, "forcont");

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(loop_bb);
                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let len_iv = if let BasicValueEnum::IntValue(iv) = list_len { iv } else { return Err("length must be i64".into()); };
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, len_iv, "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(body_bb);
                        let old_break = self.loop_break.take();
                        let old_continue = self.loop_continue.take();
                        self.loop_break = Some(merge_bb);
                        self.loop_continue = Some(loop_bb);

                        let data_gep = self.builder.build_struct_gep(
                            list_struct_ty,
                            list_ptr,
                            1,
                            "list.data"
                        ).map_err(|e| format!("gep error: {}", e))?;
                        let data_ptr = self.builder.build_load(
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                            data_gep,
                            "data"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let data_pv = if let BasicValueEnum::PointerValue(pv) = data_ptr { pv } else { return Err("data must be pointer".into()); };

                        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                        let elem_ptr = unsafe {
                            self.builder.build_gep(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                data_pv,
                                &[idx_iv],
                                "elem"
                            )
                        }.map_err(|e| format!("gep error: {}", e))?;
                        let elem = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            elem_ptr,
                            "elem_val"
                        ).map_err(|e| format!("load error: {}", e))?;

                        let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(elem_alloca, elem)
                            .map_err(|e| format!("store error: {}", e))?;
                        vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));

                        self.compile_block(body, &mut vars)?;

                        vars.remove(var);
                        self.loop_break = old_break;
                        self.loop_continue = old_continue;

                        let idx_val = self.builder.build_load(
                            BasicTypeEnum::IntType(self.context.i64_type()),
                            idx_alloca,
                            "idx"
                        ).map_err(|e| format!("load error: {}", e))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err("[E0712] index must be i64".into()); };
                        let one = self.context.i64_type().const_int(1, false);
                        let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next_idx)
                            .map_err(|e| format!("store error: {}", e))?;

                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;

                        self.builder.position_at_end(merge_bb);
                    }
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| format!("break error: {}", e))?;
                        // Create unreachable block for subsequent statements
                        let function = self.current_function().ok_or_else(|| "codegen: no current function for break".to_string())?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err("break outside of loop".into());
                    }
                }
                Stmt::Continue => {
                    if let Some(target) = self.loop_continue {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| format!("continue error: {}", e))?;
                        let function = self.current_function().ok_or_else(|| "codegen: no current function for continue".to_string())?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err("continue outside of loop".into());
                    }
                }
                Stmt::MmsBlock { .. } => {
                    // Skip MMS blocks in codegen (they're for documentation/contracts)
                }
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, &mut vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and mark capability as consumed
                    let _val = self.compile_expr(expr, &vars)?;
                    // If the expression is a variable, mark it as consumed and call mimi_cap_consume
                    if let Expr::Ident(name) = expr {
                        self.consume_cap(name)?;
                        // Generate runtime cap consume call
                        if self.is_cap_var(name) {
                            if let Some(consume_fn) = self.module.get_function("mimi_cap_consume") {
                                if let Some(&(alloca, _)) = vars.get(name) {
                                    let cap_val = self.builder.build_load(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        alloca, &format!("cap_val_{}", name))
                                        .map_err(|e| format!("load error: {}", e))?;
                                    let name_global = self.builder.build_global_string_ptr(
                                        &format!("{}\0", name), &format!("cap_name_drop_{}", name))
                                        .map_err(|e| format!("string global error: {}", e))?;
                                    let name_ptr = name_global.as_pointer_value();
                                    self.builder.build_call(consume_fn, &[
                                        BasicMetadataValueEnum::IntValue(cap_val.into_int_value()),
                                        BasicMetadataValueEnum::PointerValue(name_ptr),
                                    ], &format!("cap_consume_{}", name))
                                        .map_err(|e| format!("cap_consume error: {}", e))?;
                                }
                            }
                        }
                    }
                }
                Stmt::SharedLet { name, init, .. } => {
                    let val = self.compile_expr(init, &vars)?;
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, name)
                        .map_err(|e| format!("shared alloca error: {}", e))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| format!("shared store error: {}", e))?;
                    vars.insert(name.clone(), (alloca, llvm_ty));
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(block, &mut vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after arena: {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::Alloc { kind: AllocKind::Arena, body } => {
                    let function = self.current_function().ok_or("arena outside function")?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| format!("branch to alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_body_bb);
                    let saved = self.build_stacksave()?;
                    let vars_before: std::collections::HashSet<String> = vars.keys().cloned().collect();
                    self.compile_block(body, &mut vars)?;
                    for k in vars.keys().cloned().collect::<Vec<_>>() {
                        if !vars_before.contains(&k) {
                            vars.remove(&k);
                        }
                    }
                    self.build_stackrestore(saved)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_cont_bb)
                            .map_err(|e| format!("branch after alloc(Arena): {}", e))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified - no custom allocator in codegen)
                    self.compile_block(body, &mut vars)?;
                }
                Stmt::Desc(_) | Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Math(_) => {
                    // Skip contract-related statements in codegen
                }
                _ => {}
            }
        }

        // Check for unconsumed capabilities before returning
        self.check_unconsumed_caps()?;
        
        // Pop scopes (discard compensations on normal exit)
        self.pop_comp_scope();
        self.pop_cap_scope();

        self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
        Ok(())
    }

    /// Compile a generic function with concrete type arguments (monomorphization)
    pub(super) fn compile_generic_func(&mut self, func: &FuncDef, type_map: &HashMap<String, crate::ast::Type>) -> Result<(), String> {
        // Save and set the type_map
        let prev_type_map = self.type_map.clone();
        self.type_map = type_map.clone();

        let mangled = Self::mangle_name(&func.name, type_map);

        // Skip if already compiled
        if self.module.get_function(&mangled).is_some() {
            self.type_map = prev_type_map;
            return Ok(());
        }

        // Substitute generic params in ret type and param types
        let ret_type = match &func.ret {
            Some(ty) => {
                let resolved = self.resolve_type(ty);
                types::mimi_type_to_llvm(self.context, &resolved)
                    .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
            }
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let mut param_types = Vec::new();
        for param in &func.params {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &resolved) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let function = self.module.add_function(&mangled, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &resolved) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).ok_or_else(|| "param index matches".to_string())?)
                    .map_err(|e| format!("store error: {}", e))?;
                vars.insert(param.name.clone(), (alloca, ty));
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }

        let last_val: BasicValueEnum = self.context.i64_type().const_int(0, false).into();
        self.compile_block(&func.body, &mut vars)?;

        self.check_unconsumed_caps()?;
        self.pop_cap_scope();

        if !self.block_has_terminator() {
            self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
        }
        self.type_map = prev_type_map;
        Ok(())
    }

}
