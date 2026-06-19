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
    pub(super) fn compile_actor(&mut self, actor: &crate::ast::ActorDef) -> MimiResult<()> {
        // Generate constructor function: ActorName(field1, field2, ...) -> Actor
        let mut param_types = Vec::new();
        for f in &actor.fields {
            let ty = types::mimi_type_to_llvm(self.context, &f.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            param_types.push(ty);
        }
        
        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();
        
        // Return type is a pointer to the actor struct
        let actor_ty = *self.type_llvm.get(&actor.name)
            .ok_or_else(|| CompileError::TypeNotFound(format!("actor type '{}' not found", actor.name)))?;
        
        let fn_type = match actor_ty {
            BasicTypeEnum::StructType(sty) => sty.fn_type(&metadata_params, false),
            _ => return Err(CompileError::ActorNotStruct(actor.name.to_string())),
        };
        
        let constructor_name = format!("{}_new", actor.name);
        let function = self.module.add_function(&constructor_name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        
        // Allocate actor struct
        let alloca = match actor_ty {
            BasicTypeEnum::StructType(sty) => self.builder.build_alloca(sty, &actor.name)
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?,
            _ => return Err(CompileError::LlvmError("actor type error".to_string())),
        };
        
        // Store field values
        for (i, param) in function.get_params().iter().enumerate() {
            if let Some(BasicTypeEnum::StructType(sty)) = self.type_llvm.get(&actor.name) {
                let gep = self.builder.build_struct_gep(*sty, alloca, i as u32, &actor.fields[i].name)
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(gep, *param)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            }
        }
        
        // Return the actor struct
        let ret_val = self.builder.build_load(actor_ty, alloca, &actor.name)
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        self.builder.build_return(Some(&ret_val))
            .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        
        // Compile all actor methods
        for method in &actor.methods {
            self.compile_actor_method(actor, method)?;
        }
        
        // Generate spawn wrapper function
        self.compile_actor_spawn(actor)?;
        
        Ok(())
    }
    
    fn compile_actor_spawn(&mut self, actor: &crate::ast::ActorDef) -> MimiResult<()> {
        // Generate {Name}_spawn() -> Actor struct
        // This mirrors the constructor but evaluates field init expressions instead of taking params
        let actor_ty = *self.type_llvm.get(&actor.name)
            .ok_or_else(|| CompileError::TypeNotFound(format!("actor type '{}' not found", actor.name)))?;
        
        let spawn_fn_type = match actor_ty {
            BasicTypeEnum::StructType(sty) => sty.fn_type(&[], false),
            _ => return Err(CompileError::ActorNotStruct(actor.name.to_string())),
        };
        
        let spawn_name = format!("{}_spawn", actor.name);
        let function = self.module.add_function(&spawn_name, spawn_fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        
        // Allocate actor struct
        let alloca = match actor_ty {
            BasicTypeEnum::StructType(sty) => self.builder.build_alloca(sty, &actor.name)
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?,
            _ => return Err(CompileError::LlvmError("actor type error".to_string())),
        };
        
        // Store field values from init expressions (or default)
        let empty_vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        if let BasicTypeEnum::StructType(sty) = actor_ty {
            for (i, field) in actor.fields.iter().enumerate() {
                let gep = self.builder.build_struct_gep(sty, alloca, i as u32, &field.name)
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let val = if let Some(init) = &field.init {
                    self.compile_expr(init, &empty_vars)?
                } else {
                    // Default value by type
                    let ty = types::mimi_type_to_llvm(self.context, &field.ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    match ty {
                        BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
                        BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
                        BasicTypeEnum::PointerType(t) => t.const_null().into(),
                        _ => self.context.i64_type().const_int(0, false).into(),
                    }
                };
                self.builder.build_store(gep, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            }
        }
        
        // Return the actor struct
        let ret_val = self.builder.build_load(actor_ty, alloca, &actor.name)
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        self.builder.build_return(Some(&ret_val))
            .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        
        Ok(())
    }
    
    pub(super) fn compile_actor_method(&mut self, actor: &crate::ast::ActorDef, method: &FuncDef) -> MimiResult<()> {
        let actor_ty = *self.type_llvm.get(&actor.name)
            .ok_or_else(|| CompileError::TypeNotFound(format!("actor type '{}' not found", actor.name)))?;
        
        // Method name: ActorName__methodName
        let mangled = format!("{}__{}__method", actor.name, method.name);
        
        // Build function type: self (ptr to actor struct) + params -> ret
        let actor_ptr_ty = match actor_ty {
            BasicTypeEnum::StructType(sty) => BasicTypeEnum::PointerType(sty.ptr_type(inkwell::AddressSpace::default())),
            _ => return Err(CompileError::ActorNotStruct(actor.name.to_string())),
        };
        
        let mut param_metadata = vec![types::basic_to_metadata(self.context, actor_ptr_ty)];
        let mut param_llvm = vec![actor_ptr_ty];
        for p in &method.params {
            let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            param_llvm.push(ty);
            param_metadata.push(types::basic_to_metadata(self.context, ty));
        }
        
        let ret_llvm = match &method.ret {
            Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };
        
        let fn_type = match ret_llvm {
            BasicTypeEnum::IntType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&param_metadata, false),
            _ => self.context.i64_type().fn_type(&param_metadata, false),
        };
        
        let function = self.module.add_function(&mangled, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        
        self.push_cap_scope();
        self.push_comp_scope();
        
        let mut vars: HashMap<String, VarEntry> = HashMap::new();
        
        // Bind self: allocate space for actor struct and store pointer
        let self_alloca = self.builder.build_alloca(actor_ptr_ty, "self")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder.build_store(self_alloca, function.get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("codegen: missing self param in actor method".to_string()))?)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        vars.insert("self".to_string(), (self_alloca, actor_ptr_ty));
        self.var_type_names.insert("self".to_string(), actor.name.clone());
        
        // Bind method params
        let param_offset = 1; // param 0 is self
        for (i, param) in method.params.iter().enumerate() {
            let ty = types::mimi_type_to_llvm(self.context, &param.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let alloca = self.builder.build_alloca(ty, &param.name)
                .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
            self.builder.build_store(alloca, function.get_nth_param((i + param_offset) as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("codegen: missing param {} in actor method", i + param_offset)))?)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            vars.insert(param.name.clone(), (alloca, ty));
        }
        
        let ret_type = self.current_fn_ret_type();
        let default_val = match ret_type {
            BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
            BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
            _ => self.context.i64_type().const_int(0, false).into(),
        };
        let mut last_val: BasicValueEnum = default_val;
        for stmt in &method.body {
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
                    self.pop_shared_scope()?;
                    self.pop_cap_scope();
                    let mut val = self.compile_expr(expr, &vars)?;
                    val = self.adjust_int_val(val, self.current_fn_ret_type())?;
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        self.compile_contract_assert(ensures_expr, &vars, &format!("ensures violation"))?;
                    }
                    self.builder.build_return(Some(&val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.pop_comp_scope();
                    self.pop_shared_scope()?;
                    self.pop_cap_scope();
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        self.compile_contract_assert(ensures_expr, &vars, &format!("ensures violation"))?;
                    }
                    self.builder.build_return(None).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), ty, .. } => {
                    let mut val = self.compile_expr(init, &vars)?;
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let llvm_ty = if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        val = self.adjust_int_val(val, target)?;
                        target
                    } else {
                        val.get_type()
                    };
                    let alloca = self.builder.build_alloca(llvm_ty, &name)
                        .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    // Track type name from record expressions or spawn calls
                    if let Expr::Record { ty: Some(tn), .. } = init {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    } else if let Expr::Call(callee, _) = init {
                        if let Expr::Field(obj, method_name) = callee.as_ref() {
                            if method_name == "spawn" {
                                let obj_type = self.infer_object_type(obj, &vars);
                                if !obj_type.is_empty() {
                                    self.var_type_names.insert(name.clone(), obj_type);
                                }
                            }
                        }
                    }
                    vars.insert(name, (alloca, llvm_ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, &vars)?;
                    if let Some(&(alloca, ty)) = vars.get(name) {
                        self.assign_to_var(name, val, alloca, ty)?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err(CompileError::TypeMismatch("if condition must be boolean".to_string()));
                    };
                    let function = self.current_function()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no current function for if in actor method".to_string()))?;
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");
                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, &mut vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }
                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, &mut vars)?;
                    }
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::For { var, iterable, body } => {
                    let function = self.current_function()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no current function for for loop in actor method".to_string()))?;
                    if let Expr::Binary(BinOp::Range, start_expr, end_expr) = iterable {
                        let start_val = self.compile_expr(start_expr, &vars)?;
                        let end_val = self.compile_expr(end_expr, &vars)?;
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
                        let idx_val = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), idx_alloca, "idx")
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err(CompileError::TypeMismatch("idx must be i64".to_string())); };
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx_iv, end_iv, "cmp")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                        self.builder.build_conditional_branch(cmp, body_bb, merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        self.builder.position_at_end(body_bb);
                        let elem_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(self.context.i64_type()), var)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        let old_break = self.loop_break.replace(merge_bb);
                        let old_continue = self.loop_continue.replace(loop_bb);
                        self.builder.build_store(elem_alloca, idx_val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                        self.compile_block(body, &mut vars)?;
                        vars.remove(var);
                        self.loop_break = old_break;
                        self.loop_continue = old_continue;
                        let idx_val = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), idx_alloca, "idx")
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        let idx_iv = if let BasicValueEnum::IntValue(iv) = idx_val { iv } else { return Err(CompileError::TypeMismatch("idx must be i64".to_string())); };
                        let one = self.context.i64_type().const_int(1, false);
                        let next_idx = self.builder.build_int_add(idx_iv, one, "next_idx")
                            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                        self.builder.build_store(idx_alloca, next_idx)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        self.builder.position_at_end(merge_bb);
                    } else {
                        return Err(CompileError::UnsupportedStmt("for loop requires range in codegen".to_string()));
                    }
                }
                Stmt::While { cond, body } => {
                    let function = self.current_function()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no current function for while loop in actor method".to_string()))?;
                    let loop_bb = self.context.append_basic_block(function, "loop");
                    let body_bb = self.context.append_basic_block(function, "loopbody");
                    let merge_bb = self.context.append_basic_block(function, "loopcont");
                    self.builder.build_unconditional_branch(loop_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    self.builder.position_at_end(loop_bb);
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val { iv } else { return Err(CompileError::TypeMismatch("while condition must be boolean".to_string())); };
                    self.builder.build_conditional_branch(cond_bool, body_bb, merge_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    self.builder.position_at_end(body_bb);
                    let old_break = self.loop_break.replace(merge_bb);
                    let old_continue = self.loop_continue.replace(loop_bb);
                    self.compile_block(body, &mut vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }
                    self.loop_break = old_break;
                    self.loop_continue = old_continue;
                    self.builder.position_at_end(merge_bb);
                }
                Stmt::MmsBlock { .. } => {}
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, &mut vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    self.compile_expr(expr, &vars)?;
                }
                Stmt::OnFailure(block) => {
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
                Stmt::Unsafe(block) | Stmt::Alloc { body: block, .. } => {
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::SharedLet { name, init, .. } => {
                    self.compile_shared_let_stmt(name, init, &mut vars)?;
                }
                Stmt::Desc(_) | Stmt::Requires(_, _) | Stmt::Ensures(_, _) | Stmt::Math(_) => {}
                _ => {}
            }
        }
        
        self.check_unconsumed_caps()?;
        self.pop_comp_scope();
        self.release_all_shared()?;
        self.pop_cap_scope();
        
        if !self.block_has_terminator() {
            let ensures = self.ensures_stmts.clone();
            for ensures_expr in &ensures {
                self.compile_contract_assert(ensures_expr, &vars, &format!("ensures violation"))?;
            }
            let last_val = self.adjust_int_val(last_val, ret_type)?;
            self.builder.build_return(Some(&last_val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        }
        Ok(())
    }
}
