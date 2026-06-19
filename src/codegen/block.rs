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
    pub(super) fn compile_block(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        self.push_comp_scope();
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
                    val = self.adjust_int_val(val, self.current_fn_ret_type())?;
                    self.builder.build_return(Some(&val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    self.builder.build_return(None).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), ty, .. } => {
                    let mut val = self.compile_expr(init, vars)?;
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
                    if let Expr::Record { ty: Some(tn), .. } = init {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    }
                    vars.insert(name, (alloca, llvm_ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err(CompileError::TypeMismatch("if condition must be boolean".to_string()));
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
                Stmt::Break(_) | Stmt::Continue => {}
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
                Stmt::SharedLet { name, init, .. } => {
                    let val = self.compile_expr(init, vars)?;
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, name)
                        .map_err(|e| CompileError::LlvmError(format!("shared alloca error: {}", e)))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| CompileError::LlvmError(format!("shared store error: {}", e)))?;
                    vars.insert(name.clone(), (alloca, llvm_ty));
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
                    self.compile_block(block, vars)?;
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
                    self.compile_block(block, vars)?;
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
                    self.compile_block(body, vars)?;
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
                    // Alloc: execute body sequentially (simplified)
                    self.compile_block(body, vars)?;
                }
                Stmt::Desc(_) | Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Math(_) => {
                    // Skip contract-related statements in codegen
                }
                _ => {}
            }
        }
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
        let val = call.try_as_basic_value().left()
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
                    let name = match pat {
                        Pattern::Variable(n) => n.clone(),
                        _ => continue,
                    };
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &name)
                        .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                    self.builder.build_store(alloca, val)
                        .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    vars.insert(name, (alloca, llvm_ty));
                }
                Stmt::Assign { target: Expr::Ident(name), value } => {
                    let val = self.compile_expr(value, vars)?;
                    if let Some(&(alloca, _)) = vars.get(name) {
                        self.builder.build_store(alloca, val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        last_val = val;
                    }
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err(CompileError::TypeMismatch("if condition must be boolean".to_string()));
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
                _ => {}
            }
        }
        Ok(last_val)
    }

}
