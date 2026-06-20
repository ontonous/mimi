#![allow(dead_code, deprecated)]

use crate::ast::*;
use crate::codegen::CallSiteValueExt;
use crate::codegen::call_try_basic_value;
use crate::codegen::types;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_block(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        self.push_comp_scope();
        self.push_shared_scope();
        self.push_heap_scope();
        for stmt in block {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    self.compile_expr(expr, vars)?;
                }
                Stmt::Return(Some(expr)) => {
                    let mut val = self.compile_expr(expr, vars)?;
                    let ret_type = self.current_fn_ret_type();
                    val = self.adjust_int_val(val, ret_type)?;
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        let fn_name: String = self.current_function()
                            .map(|f| f.get_name().to_string_lossy().into_owned())
                            .unwrap_or_else(|| "unknown".to_string());
                        self.compile_contract_assert(ensures_expr, vars, &format!("ensures violation in '{}'", fn_name))?;
                    }
                    self.builder.build_return(Some(&val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    let ensures = self.ensures_stmts.clone();
                    for ensures_expr in &ensures {
                        let fn_name: String = self.current_function()
                            .map(|f| f.get_name().to_string_lossy().into_owned())
                            .unwrap_or_else(|| "unknown".to_string());
                        self.compile_contract_assert(ensures_expr, vars, &format!("ensures violation in '{}'", fn_name))?;
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
                        let concrete_val = self.compile_expr(init, vars)?;
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
                        vars.insert(name, (fat_alloca, fat_ty));
                        continue;
                    }
                    // Shared ref copy: let v = shared_var
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Ident(src_name) = init {
                            if self.shared_var_names.contains(src_name.as_str()) {
                                self.compile_shared_ref_copy(name, src_name, vars)?;
                                continue;
                            }
                        }
                    }
                    // Non-dyn Trait: compile init and bind via recursive pattern matching
                    let mut val = self.compile_expr(init, vars)?;
                    if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        val = self.adjust_int_val(val, target)?;
                    }
                    // For simple Variable patterns, track type info
                    if let Pattern::Variable(name) = pat {
                        if let Some(Type::Name(tn, _)) = &ty {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if let Expr::Record { ty: Some(tn), .. } = init {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if let Expr::Call(callee, _) = init {
                            if let Expr::Field(obj, method_name) = callee.as_ref() {
                                if method_name == "spawn" {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if !obj_type.is_empty() {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(method_name.as_str(), "map" | "and_then" | "map_err" | "ok_or") {
                                    let obj_type = self.infer_object_type(obj, vars);
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
                    }
                    self.compile_pattern_bind(pat, val, vars)?;
                }
                Stmt::Let { pat, init: None, ty, .. } => {
                    // let x; or let (a, b); — needs type annotation
                    if let Pattern::Variable(name) = pat {
                        let llvm_ty = match ty {
                            Some(decl_ty) => types::mimi_type_to_llvm(self.context, decl_ty)
                                .ok_or_else(|| CompileError::LlvmError(
                                    format!("unknown type for 'let {};'", name)
                                ))?,
                            None => return Err(CompileError::LlvmError(
                                format!("'let {};' requires an explicit type annotation", name)
                            )),
                        };
                        let alloca = self.builder.build_alloca(llvm_ty, name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        if let BasicTypeEnum::IntType(ty) = llvm_ty {
                            self.builder.build_store(alloca, ty.const_int(0, false))
                                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        }
                        vars.insert(name.clone(), (alloca, llvm_ty));
                    } else {
                        return Err(CompileError::LlvmError(
                            format!("'let' with no initializer requires a simple variable pattern").to_string()
                        ));
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        let function = self.current_function()
                            .ok_or_else(|| CompileError::LlvmError("codegen: no current function for if block".to_string()))?;
                        let fn_name = function.get_name().to_str().unwrap_or("unknown");
                        return Err(CompileError::TypeMismatch(
                            format!("if condition must be bool, got {} in function '{}'", cond_val.get_type(), fn_name)
                        ));
                    };

                    let function = self.current_function()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no current function for if block".to_string()))?;
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                    self.builder.position_at_end(then_bb);
                    self.compile_block(then_, vars)?;
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }

                    self.builder.position_at_end(else_bb);
                    if let Some(else_block) = else_ {
                        self.compile_block(else_block, vars)?;
                    }
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }

                    self.builder.position_at_end(merge_bb);
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| CompileError::LlvmError(format!("break error: {}", e)))?;
                        let function = self.current_function()
                            .ok_or_else(|| CompileError::LlvmError("codegen: no current function for break".to_string()))?;
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
                        let function = self.current_function()
                            .ok_or_else(|| CompileError::LlvmError("codegen: no current function for continue".to_string()))?;
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
                    self.compile_block(block, vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and discard result (for linear capabilities)
                    self.compile_expr(expr, vars)?;
                }
                Stmt::SharedLet { kind, name, ty, init } => {
                    self.compile_shared_let_stmt(&kind, name, &ty, init, vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    self.compile_arena_block(block, vars, "arena")?;
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, vars)?;
                }
                Stmt::Alloc { kind: AllocKind::Arena, body } => {
                    self.compile_arena_block(body, vars, "alloc(Arena)")?;
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified)
                    self.compile_block(body, vars)?;
                }
                Stmt::Desc(_) | Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Math(_) => {
                    // Skip contract-related statements in codegen
                }
                Stmt::Block(block) => {
                    self.compile_block(block, vars)?;
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, vars)?;
                }
                Stmt::For { var, iterable, body } => {
                    self.compile_for_stmt(var, iterable, body, vars)?;
                }
                _ => {}
            }
        }
        self.pop_shared_scope()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        Ok(())
    }

    /// Call @llvm.stacksave() to capture the current stack pointer for arena region management
    pub(super) fn build_stacksave(&self) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let fn_type = i8_ptr.fn_type(&[], false);
        let fn_val = self.module.get_function("llvm.stacksave")
            .unwrap_or_else(|| self.module.add_function(
                "llvm.stacksave",
                fn_type,
                Some(inkwell::module::Linkage::External),
            ));
        let call = self.builder.build_call(fn_val, &[], "saved_stack")
            .map_err(|e| CompileError::LlvmError(format!("stacksave: {}", e)))?;
        let val = call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("stacksave returned void".to_string()))?;
        match val {
            BasicValueEnum::PointerValue(ptr) => Ok(ptr),
            _ => Err(CompileError::LlvmError(format!("stacksave didn't return pointer, got {:?}", val))),
        }
    }

    /// Call @llvm.stackrestore(i8*) to restore the stack pointer, freeing arena allocations
    pub(super) fn build_stackrestore(&self, saved: inkwell::values::PointerValue<'ctx>) -> MimiResult<()> {
        let i8_ptr_meta = BasicMetadataTypeEnum::PointerType(
            self.context.i8_type().ptr_type(inkwell::AddressSpace::default()),
        );
        let fn_type = self.context.void_type().fn_type(&[i8_ptr_meta], false);
        let fn_val = self.module.get_function("llvm.stackrestore")
            .unwrap_or_else(|| self.module.add_function(
                "llvm.stackrestore",
                fn_type,
                Some(inkwell::module::Linkage::External),
            ));
        self.builder.build_call(fn_val, &[BasicMetadataValueEnum::PointerValue(saved)], "")
            .map_err(|e| CompileError::LlvmError(format!("stackrestore: {}", e)))?;
        Ok(())
    }

    /// Compile a block and return the value of its last expression (for if-expressions)
    pub(super) fn compile_block_last_val(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let mut last_val = self.context.i64_type().const_int(0, false).into();
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => {
                    last_val = self.compile_expr(e, vars)?;
                }
                Stmt::Return(Some(e)) => {
                    return Ok(self.compile_expr(e, vars)?);
                }
                Stmt::Return(None) => {
                    return Ok(self.context.i64_type().const_int(0, false).into());
                }
                Stmt::Let { pat, init: Some(init), .. } => {
                    let val = self.compile_expr(init, vars)?;
                    self.compile_pattern_bind(pat, val, vars)?;
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, vars)?;
                    if let Some(&(alloca, ty)) = vars.get(name) {
                        self.assign_to_var(name, val, alloca, ty)?;
                        last_val = val;
                    }
                }
                Stmt::Assign { target: Expr::Field(obj, field_name), value } => {
                    let val = self.compile_expr(value, vars)?;
                    self.compile_field_assign(obj, field_name, val, vars)?;
                    last_val = val;
                }
                Stmt::Assign { target: Expr::Index(obj, idx), value } => {
                    let val = self.compile_expr(value, vars)?;
                    self.compile_index_assign(obj, idx, val, vars)?;
                    last_val = val;
                }
                Stmt::Assign { target: Expr::Unary(crate::ast::UnOp::Deref, inner), value } => {
                    let val = self.compile_expr(value, vars)?;
                    self.compile_deref_assign(inner, val, vars)?;
                    last_val = val;
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        let function = self.current_function()
                            .ok_or_else(|| CompileError::LlvmError("codegen: no current function for if block".to_string()))?;
                        let fn_name = function.get_name().to_str().unwrap_or("unknown");
                        return Err(CompileError::TypeMismatch(
                            format!("if condition must be bool, got {} in function '{}'", cond_val.get_type(), fn_name)
                        ));
                    };
                    let function = self.current_function()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no current function for if expression".to_string()))?;
                    let then_bb = self.context.append_basic_block(function, "blt_then");
                    let else_bb = self.context.append_basic_block(function, "blt_else");
                    let merge_bb = self.context.append_basic_block(function, "blt_merge");
                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    let then_val = {
                        self.builder.position_at_end(then_bb);
                        let mut then_vars = vars.clone();
                        let v = self.compile_block_last_val(then_, &mut then_vars)?;
                        if !self.block_has_terminator() {
                            self.builder.build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                        v
                    };
                    let then_bb_end = self.builder.get_insert_block()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no insert block after then branch".to_string()))?;
                    let else_val = {
                        self.builder.position_at_end(else_bb);
                        if let Some(eb) = else_ {
                            let mut else_vars = vars.clone();
                            let v = self.compile_block_last_val(eb, &mut else_vars)?;
                            if !self.block_has_terminator() {
                                self.builder.build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                            }
                            v
                        } else {
                            self.context.i64_type().const_int(0, false).into()
                        }
                    };
                    let else_bb_end = self.builder.get_insert_block()
                        .ok_or_else(|| CompileError::LlvmError("codegen: no insert block after else branch".to_string()))?;
                    // Ensure else_bb has a terminator (it's empty for no-else case)
                    if !self.block_has_terminator() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }
                    self.builder.position_at_end(merge_bb);
                    // Create phi if both branches produce a value of the same type
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self.builder.build_phi(then_val.get_type(), "if_lastval")
                            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                        phi.add_incoming(&[
                            (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
                            (&else_val as &dyn inkwell::values::BasicValue, else_bb_end),
                        ]);
                        last_val = phi.as_basic_value();
                    } else {
                        last_val = then_val;
                    }
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| CompileError::LlvmError(format!("break error: {}", e)))?;
                        let function = self.current_function()
                            .ok_or_else(|| CompileError::LlvmError("codegen: no current function for break".to_string()))?;
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
                        let function = self.current_function()
                            .ok_or_else(|| CompileError::LlvmError("codegen: no current function for continue".to_string()))?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::ContinueOutsideLoop);
                    }
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, vars)?;
                }
                Stmt::For { var, iterable, body } => {
                    self.compile_for_stmt(var, iterable, body, vars)?;
                }
                _ => {}
            }
        }
        Ok(last_val)
    }

}
