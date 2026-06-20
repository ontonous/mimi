#![allow(dead_code, deprecated)]

use crate::ast::*;
use crate::codegen::types;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_async_func(&mut self, func: &FuncDef) -> MimiResult<()> {
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
            extern_abi: None,
            pos: (0, 0),
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
            extern_abi: None,
            pos: (0, 0),
        };
            self.compile_func(&spawner_func)?;
        Ok(())
    }

    /// For a function returning `impl Trait`, extract the concrete return type
    /// from the function body (e.g., a record literal's type annotation).
    fn concrete_return_type_for_impl_trait(body: &[Stmt]) -> Option<String> {
        let last = body.last()?;
        match last {
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) => match expr {
                Expr::Record { ty, .. } => ty.clone(),
                Expr::Call(callee, _) => {
                    if let Expr::Ident(fname) = callee.as_ref() {
                        // We can't look up func_defs here (static context),
                        // so return None; caller must handle this case
                        None
                    } else {
                        None
                    }
                }
                _ => None,
            },
            Stmt::If { cond: _, then_, else_ } => {
                let then_ty = Self::concrete_return_type_for_impl_trait(then_);
                if then_ty.is_some() {
                    then_ty
                } else {
                    else_.as_ref()
                        .and_then(|el| Self::concrete_return_type_for_impl_trait(el))
                }
            }
            Stmt::Block(block) => Self::concrete_return_type_for_impl_trait(block),
            _ => None,
        }
    }

    pub(super) fn compile_func(&mut self, func: &FuncDef) -> MimiResult<()> {
        // Delegate async funcs to compile_async_func
        if func.is_async {
            return self.compile_async_func(func);
        }

        // For impl Trait return types, determine the concrete type from the body
        // so the function's LLVM signature uses the right type.
        let effective_ret_override = if let Some(Type::ImplTrait(_)) = &func.ret {
            Self::concrete_return_type_for_impl_trait(&func.body)
                .and_then(|tn| self.type_llvm.get(&tn).cloned())
        } else {
            None
        };

        let ret_type = effective_ret_override.or_else(|| {
            match &func.ret {
                Some(ty) => types::mimi_type_to_llvm(self.context, ty),
                None => None,
            }
        }).unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

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

        let linkage = if func.extern_abi.is_some() {
            Some(inkwell::module::Linkage::External)
        } else {
            None
        };
        let function = self.module.add_function(&func.name, fn_type, linkage);
        // Set calling convention for extern "C" / extern "stdcall" etc.
        if let Some(ref abi) = func.extern_abi {
            let cc = crate::ffi::abi_to_llvm_call_conv(abi);
            function.set_call_conventions(cc);
        }
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // Push scopes for function body
        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if let Some(ty) = types::mimi_type_to_llvm(self.context, &param.ty) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).ok_or_else(|| CompileError::LlvmError(format!("param index {} out of range for function '{}' with {} params", i, func.name, function.count_params())))?)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
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

        // Collect ensures contracts for runtime checking at return points
        self.ensures_stmts = if self.verify_contracts {
            func.body.iter().filter_map(|s| {
                if let Stmt::Ensures(expr, _) = s { Some(Box::new(expr.clone())) } else { None }
            }).collect()
        } else {
            Vec::new()
        };

        // Compile requires contracts as runtime asserts when verify_contracts is enabled
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    self.compile_contract_assert(expr, &vars, &format!("requires violation in '{}'", func.name))?;
                }
            }
        }

        let default_val = match ret_type {
            BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
            BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
            _ => self.context.i64_type().const_int(0, false).into(),
        };
        let mut last_val: BasicValueEnum = default_val;
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
                    last_val = self.adjust_int_val(last_val, ret_type)?;
                }
                Stmt::Return(Some(expr)) => {
                    self.pop_comp_scope();
                    self.free_heap_allocs()?;
                    self.pop_shared_scope()?;
                    self.pop_cap_scope();
                    let val = self.compile_expr(expr, &vars)?;
                    let val = self.adjust_int_val(val, ret_type)?;
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        self.compile_contract_assert(ensures_expr, &vars, &format!("ensures violation in '{}'", func.name))?;
                    }
                    self.builder.build_return(Some(&val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.pop_comp_scope();
                    self.free_heap_allocs()?;
                    self.pop_shared_scope()?;
                    self.pop_cap_scope();
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        self.compile_contract_assert(ensures_expr, &vars, &format!("ensures violation in '{}'", func.name))?;
                    }
                    self.builder.build_return(None).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), ty, .. } => {
                    // dyn Trait let-binding: build fat pointer from concrete value (requires Variable pattern)
                    if let Some(Type::DynTrait(trait_names)) = &ty {
                        let name = match pat {
                            Pattern::Variable(n) => n.clone(),
                            _ => return Err(CompileError::LlvmError(
                                "dyn Trait binding requires a simple variable pattern".to_string()
                            )),
                        };
                        let concrete_val = self.compile_expr(init, &vars)?;
                        let concrete_type = match init {
                            Expr::Record { ty: Some(tn), .. } => tn.clone(),
                            Expr::Ident(var_name) => self.var_type_names.get(var_name).cloned().unwrap_or_default(),
                            _ => {
                                return Err(CompileError::LlvmError(
                                    format!("cannot infer concrete type for dyn Trait binding '{}'", name)
                                ));
                            }
                        };
                        if concrete_type.is_empty() {
                            return Err(CompileError::LlvmError(
                                format!("cannot infer concrete type for dyn Trait binding '{}'", name)
                            ));
                        }
                        let trait_name = &trait_names[0];
                        let concrete_ty = self.type_llvm.get(&concrete_type)
                            .cloned()
                            .unwrap_or_else(|| concrete_val.get_type());
                        let data_alloca = self.builder.build_alloca(concrete_ty, &format!("{}_data", name))
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        self.builder.build_store(data_alloca, concrete_val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let data_ptr = self.builder.build_pointer_cast(
                            data_alloca, i8_ptr, &format!("{}_data_i8", name)
                        ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                        let vtable_key = format!("{}__{}", concrete_type, trait_name);
                        let vtable_gv = self.vtable_globals.get(&vtable_key)
                            .ok_or_else(|| CompileError::LlvmError(
                                format!("no vtable for {}.{}", concrete_type, trait_name)
                            ))?;
                        let vtable_ptr = self.builder.build_pointer_cast(
                            vtable_gv.as_pointer_value(), i8_ptr,
                            &format!("{}_vtable_i8", name)
                        ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                        let fat_ty = BasicTypeEnum::StructType(
                            self.context.struct_type(&[
                                BasicTypeEnum::PointerType(i8_ptr),
                                BasicTypeEnum::PointerType(i8_ptr),
                            ], false)
                        );
                        let fat_alloca = self.builder.build_alloca(fat_ty, &name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        let data_gep = self.builder.build_struct_gep(fat_ty, fat_alloca, 0, &format!("{}_data_gep", name))
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(data_gep, data_ptr)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let vtable_gep = self.builder.build_struct_gep(fat_ty, fat_alloca, 1, &format!("{}_vtable_gep", name))
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(vtable_gep, vtable_ptr)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let dyn_type_str = crate::core::fmt_type(&ty.as_ref().unwrap());
                        self.var_type_names.insert(name.clone(), dyn_type_str);
                        vars.insert(name.clone(), (fat_alloca, fat_ty));
                        if let Some(Type::Cap(_)) = &ty {
                            self.register_cap(&name, fat_alloca);
                        }
                        continue;
                    }
                    // Shared ref copy: let v = shared_var
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Ident(src_name) = init {
                            if self.shared_var_names.contains(src_name.as_str()) {
                                self.compile_shared_ref_copy(name, src_name, &mut vars)?;
                                continue;
                            }
                        }
                    }
                    // Non-dyn Trait: compile init and bind via recursive pattern matching
                    let mut val = self.compile_expr(init, &vars)?;
                    if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        val = self.adjust_int_val(val, target)?;
                    }
                    // Track type info for simple Variable patterns
                    if let Pattern::Variable(name) = pat {
                        if let Some(Type::Name(tn, _)) = &ty {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if let Expr::Record { ty: Some(tn), .. } = init {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if let Expr::Call(callee, _) = init {
                            if let Expr::Field(obj, method_name) = callee.as_ref() {
                                if method_name == "spawn" {
                                    let obj_type = self.infer_object_type(obj, &vars);
                                    if !obj_type.is_empty() {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(method_name.as_str(), "map" | "and_then" | "map_err" | "ok_or") {
                                    let obj_type = self.infer_object_type(obj, &vars);
                                    if obj_type == "Result" || obj_type == "Option" {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                }
                            } else if let Expr::Ident(func_name) = callee.as_ref() {
                                match func_name.as_str() {
                                    "Ok" | "Err" => {
                                        self.var_type_names.insert(name.clone(), "Result".to_string());
                                    }
                                    "Some" | "None" => {
                                        self.var_type_names.insert(name.clone(), "Option".to_string());
                                    }
                                    _ => {
                                        if let Some(fdef) = self.func_defs.get(func_name) {
                                            if let Some(ret_ty) = &fdef.ret {
                                                match ret_ty {
                                                    Type::ImplTrait(traits) => {
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            format!("impl {}", traits.join(" + ")),
                                                        );
                                                    }
                                                    Type::Name(tn, _) => {
                                                        self.var_type_names.insert(name.clone(), tn.clone());
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Track capability variables
                        if let Some(Type::Cap(_)) = &ty {
                            if let Some(&(alloca, _)) = vars.get(name) {
                                self.register_cap(name, alloca);
                            }
                        }
                    }
                    self.compile_pattern_bind(pat, val, &mut vars)?;
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, &mut vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        let fn_name = function.get_name().to_str().unwrap_or("unknown");
                        return Err(CompileError::TypeMismatch(
                            format!("if condition must be bool, got {} in function '{}'", cond_val.get_type(), fn_name)
                        ));
                    };

                    let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for if".to_string()))?;
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                    // Then block
                    let then_val = {
                        self.builder.position_at_end(then_bb);
                        let mut then_vars = vars.clone();
                        let v = self.compile_block_last_val(then_, &mut then_vars)?;
                        let current = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block for then block".to_string()))?;
                        if current.get_terminator().is_none() {
                            self.builder.build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                        v
                    };
                    let then_bb_end = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block after then".to_string()))?;

                    // Else block
                    let else_val = {
                        self.builder.position_at_end(else_bb);
                        if let Some(else_block) = else_ {
                            let mut else_vars = vars.clone();
                            let v = self.compile_block_last_val(else_block, &mut else_vars)?;
                            let current = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block for else block".to_string()))?;
                            if current.get_terminator().is_none() {
                                self.builder.build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                            }
                            v
                        } else {
                            self.context.i64_type().const_int(0, false).into()
                        }
                    };
                    let else_bb_end = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block after else".to_string()))?;
                    // No-else case: else_bb has no terminator yet — supply one
                    if else_bb_end.get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }

                    // Continue at merge, produce phi if both branches have values
                    self.builder.position_at_end(merge_bb);
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self.builder.build_phi(then_val.get_type(), "if_result")
                            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                        phi.add_incoming(&[
                            (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
                            (&else_val as &dyn inkwell::values::BasicValue, else_bb_end),
                        ]);
                        last_val = phi.as_basic_value();
                    }
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, &mut vars)?;
                }
                Stmt::For { var, iterable, body } => {
                    self.compile_for_stmt(var, iterable, body, &mut vars)?;
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| CompileError::LlvmError(format!("break error: {}", e)))?;
                        // Create unreachable block for subsequent statements
                        let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for break".to_string()))?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::BreakOutsideLoop);
                    }
                }
                Stmt::Continue => {
                    if let Some(target) = self.loop_continue {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| CompileError::LlvmError(format!("continue error: {}", e)))?;
                        let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for continue".to_string()))?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::ContinueOutsideLoop);
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
                                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                                    let name_global = self.builder.build_global_string_ptr(
                                        &format!("{}\0", name), &format!("cap_name_drop_{}", name))
                                        .map_err(|e| CompileError::LlvmError(format!("string global error: {}", e)))?;
                                    let name_ptr = name_global.as_pointer_value();
                                    self.builder.build_call(consume_fn, &[
                                        BasicMetadataValueEnum::IntValue(cap_val.into_int_value()),
                                        BasicMetadataValueEnum::PointerValue(name_ptr),
                                    ], &format!("cap_consume_{}", name))
                                        .map_err(|e| CompileError::LlvmError(format!("cap_consume error: {}", e)))?;
                                }
                            }
                        }
                    }
                }
                Stmt::SharedLet { kind, name, ty, init } => {
                    self.compile_shared_let_stmt(&kind, name, &ty, init, &mut vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    let function = self.current_function().ok_or_else(|| CompileError::LlvmError("arena outside function".to_string()))?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch to arena: {}", e)))?;
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
                            .map_err(|e| CompileError::LlvmError(format!("branch after arena: {}", e)))?;
                    }
                    self.builder.position_at_end(arena_cont_bb);
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::Alloc { kind: AllocKind::Arena, body } => {
                    let function = self.current_function().ok_or_else(|| CompileError::LlvmError("arena outside function".to_string()))?;
                    let arena_body_bb = self.context.append_basic_block(function, "arena_body");
                    let arena_cont_bb = self.context.append_basic_block(function, "arena_cont");
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(arena_body_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch to alloc(Arena): {}", e)))?;
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
                            .map_err(|e| CompileError::LlvmError(format!("branch after alloc(Arena): {}", e)))?;
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
                Stmt::Block(block) => {
                    self.compile_block(block, &mut vars)?;
                }
                _ => {}
            }
        }

        // Check for unconsumed capabilities before returning
        self.check_unconsumed_caps()?;
        
        // Pop scopes (discard compensations on normal exit)
        self.pop_comp_scope();
        self.free_heap_allocs()?;
        self.release_all_shared()?;
        self.pop_cap_scope();

        if !self.block_has_terminator() {
            let ensures = self.ensures_stmts.clone();
            for ensures_expr in &ensures {
                self.compile_contract_assert(ensures_expr, &vars, &format!("ensures violation in '{}'", func.name))?;
            }
        }
        let last_val = self.adjust_int_val(last_val, ret_type)?;
        self.builder.build_return(Some(&last_val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        Ok(())
    }

    /// Compile a generic function with concrete type arguments (monomorphization)
    pub(super) fn compile_generic_func(&mut self, func: &FuncDef, type_map: &HashMap<String, crate::ast::Type>) -> MimiResult<()> {
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
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).ok_or_else(|| CompileError::LlvmError("param index matches".to_string()))?)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
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
        let last_val = self.adjust_int_val(last_val, ret_type)?;
        self.builder.build_return(Some(&last_val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        }
        self.type_map = prev_type_map;
        Ok(())
    }

    /// Shared implementation for Stmt::While — used by compile_func and compile_block
    pub(super) fn compile_while_stmt(
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
    pub(super) fn compile_for_stmt(
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
            let list_len_gep = self.builder.build_struct_gep(
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

            let data_gep = self.builder.build_struct_gep(
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

            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
            let elem_ptr = unsafe {
                self.builder.build_gep(
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
    pub(super) fn compile_assign_stmt(
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
    pub(super) fn compile_field_assign(
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
                    let gep = self.builder.build_struct_gep(sty, field_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    self.builder.build_store(gep, val)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    return Ok(());
                }
            }
        }
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self.builder.build_struct_gep(sty, field_ptr, idx, field_name)
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
                    let gep = self.builder.build_struct_gep(sty, struct_ptr, idx as u32, field_name)
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    self.builder.build_store(gep, val)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    return Ok(());
                }
            }
        }
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self.builder.build_struct_gep(sty, struct_ptr, idx, field_name)
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
    pub(super) fn compile_index_assign(
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
        let data_gep = self.builder.build_struct_gep(list_ty, list_ptr, 1, "list.data")
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
        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
        let elem_ptr = unsafe {
            self.builder.build_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
        }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(elem_ptr, val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(())
    }

    /// Assign through a dereference: `*ptr = val`
    pub(super) fn compile_deref_assign(
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

    /// Recursive pattern matching binding for let statements.
    /// Walks the pattern tree and binds variables by extracting from the compiled value.
    pub(super) fn compile_pattern_bind(
        &mut self,
        pat: &Pattern,
        val: BasicValueEnum<'ctx>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        match pat {
            Pattern::Wildcard => Ok(()),
            Pattern::Variable(name) => {
                let llvm_ty = val.get_type();
                let alloca = self.builder.build_alloca(llvm_ty, name)
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                vars.insert(name.clone(), (alloca, llvm_ty));
                Ok(())
            }
            Pattern::Literal(lit) => {
                let lit_val = self.compile_literal_expr(lit, &HashMap::new())
                    .map_err(|e| CompileError::LlvmError(format!("literal pattern compile error: {}", e)))?;
                let eq = match (&val, &lit_val) {
                    (BasicValueEnum::IntValue(a), BasicValueEnum::IntValue(b)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, *a, *b, "pat_lit_eq")
                            .map_err(|e| CompileError::LlvmError(format!("icmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::LlvmError(
                        "literal pattern: type mismatch".to_string()
                    )),
                };
                let bool_ty = self.context.bool_type();
                let assert_fn = self.module.get_function("mimi_runtime_assert")
                    .unwrap_or_else(|| {
                        let i8_ty = self.context.i8_type();
                        let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
                        let fn_ty = self.context.void_type().fn_type(&[
                            BasicMetadataTypeEnum::IntType(bool_ty),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("mimi_runtime_assert", fn_ty, None)
                    });
                let msg = self.builder.build_global_string_ptr("pattern literal match failed", "pat_lit_msg")
                    .map_err(|e| CompileError::LlvmError(format!("global str error: {}", e)))?;
                self.builder.build_call(assert_fn, &[
                    BasicMetadataValueEnum::IntValue(eq),
                    BasicMetadataValueEnum::PointerValue(msg.as_pointer_value()),
                ], "pat_lit_assert")
                    .map_err(|e| CompileError::LlvmError(format!("assert call error: {}", e)))?;
                Ok(())
            }
            Pattern::Constructor(_name, sub_patterns) => {
                if sub_patterns.is_empty() {
                    return Ok(());
                }
                // Load struct value if we have a pointer
                let struct_val = match val {
                    BasicValueEnum::PointerValue(pv) => {
                        // We need the struct type - use a default layout
                        let i64_ty = self.context.i64_type();
                        let struct_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.bool_type()),
                            BasicTypeEnum::IntType(i64_ty),
                        ], false);
                        let loaded = self.builder.build_load(struct_ty, pv, "ctor_loaded")
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        match loaded {
                            BasicValueEnum::StructValue(sv) => sv,
                            _ => return Err(CompileError::LlvmError(
                                "constructor pattern: expected struct from pointer".to_string()
                            )),
                        }
                    }
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Err(CompileError::LlvmError(
                        "constructor pattern requires a struct value".to_string()
                    )),
                };
                if sub_patterns.len() == 1 {
                    let payload = self.builder.build_extract_value(struct_val, 1, "ctor_payload")
                        .map_err(|e| CompileError::LlvmError(format!("extract payload error: {}", e)))?;
                    self.compile_pattern_bind(&sub_patterns[0], payload, vars)?;
                } else {
                    for (i, sub_pat) in sub_patterns.iter().enumerate() {
                        let field_val = self.builder.build_extract_value(struct_val, (i + 1) as u32, &format!("ctor_field_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
                        self.compile_pattern_bind(sub_pat, field_val, vars)?;
                    }
                }
                Ok(())
            }
            Pattern::Tuple(sub_patterns) => {
                // Load struct value if we have a pointer
                let struct_val = match val {
                    BasicValueEnum::PointerValue(pv) => {
                        // Create a struct type from the tuple type stack
                        let struct_ty = self.tuple_type_stack.last()
                            .ok_or_else(|| CompileError::LlvmError("tuple_type_stack empty for tuple pattern".to_string()))?;
                        let loaded = self.builder.build_load(*struct_ty, pv, "tuple_pat_loaded")
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        match loaded {
                            BasicValueEnum::StructValue(sv) => sv,
                            _ => return Err(CompileError::LlvmError(
                                "tuple pattern: expected struct from pointer".to_string()
                            )),
                        }
                    }
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Err(CompileError::LlvmError(
                        "tuple pattern requires a tuple value".to_string()
                    )),
                };
                for (i, sub_pat) in sub_patterns.iter().enumerate() {
                    let field_val = self.builder.build_extract_value(struct_val, i as u32, &format!("tuple_pat_field_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
                    self.compile_pattern_bind(sub_pat, field_val, vars)?;
                }
                Ok(())
            }
            Pattern::Array(sub_patterns) => {
                let list_ptr = match val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::LlvmError(
                        "array pattern requires a list pointer".to_string()
                    )),
                };
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let data_gep = self.builder.build_struct_gep(list_ty, list_ptr, 1, "list.data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_ptr = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data"
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                    .into_pointer_value();
                let data_i64 = self.builder.build_bit_cast(data_ptr,
                    self.context.i64_type().ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    .into_pointer_value();
                for (i, sub_pat) in sub_patterns.iter().enumerate() {
                    let idx = self.context.i64_type().const_int(i as u64, false);
                    // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                    let elem_ptr = unsafe {
                        self.builder.build_gep(self.context.i64_type(), data_i64, &[idx], &format!("pat_elem_{}", i))
                    }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let elem = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, &format!("pat_elem_val_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                    self.compile_pattern_bind(sub_pat, elem, vars)?;
                }
                Ok(())
            }
            Pattern::Slice(sub_patterns, rest) => {
                self.compile_pattern_bind(&Pattern::Array(sub_patterns.clone()), val, vars)?;
                if let Some(rest_pat) = rest {
                    self.compile_pattern_bind(rest_pat, val, vars)?;
                }
                Ok(())
            }
        }
    }
}
