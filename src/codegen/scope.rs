#![allow(dead_code, deprecated)]

use crate::ast::*;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn enter_parasteps(&mut self) {
        self.in_parasteps = true;
        self.parasteps_thread_ids.clear();
    }

    /// Leave parallel parasteps mode: join all spawned threads
    pub(super) fn leave_parasteps(&mut self) -> MimiResult<()> {
        if !self.in_parasteps {
            return Ok(());
        }
        // Join any threads not yet awaited
        let _i8_type = self.context.i8_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let join_fn = self.module.get_function("pthread_join");
        if let Some(join_fn) = join_fn {
            for &(thread_id, _) in &self.parasteps_thread_ids {
                self.builder.build_call(join_fn, &[
                    BasicMetadataValueEnum::IntValue(thread_id),
                    BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                ], "parasteps_join")
                    .map_err(|e| CompileError::LlvmError(format!("parasteps join error: {}", e)))?;
            }
        }
        self.parasteps_thread_ids.clear();
        self.in_parasteps = false;
        Ok(())
    }

    /// Push a new compensation scope
    pub(super) fn push_comp_scope(&mut self) {
        self.comp_scope_stack.push(self.compensation_blocks.len());
    }

    /// Pop the current compensation scope (discard blocks registered in it — normal exit)
    pub(super) fn pop_comp_scope(&mut self) {
        if let Some(start) = self.comp_scope_stack.pop() {
            self.compensation_blocks.truncate(start);
        }
    }

    /// Register a compensation block for LIFO execution on error exit
    pub(super) fn register_comp(&mut self, stmts: &Block) {
        self.compensation_blocks.push(stmts.clone());
    }

    /// Compile all registered compensation blocks in LIFO order
    pub(super) fn compile_compensations(
        &mut self,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let blocks: Vec<Block> = self.compensation_blocks.iter().rev().cloned().collect();
        for stmts in &blocks {
            self.compile_block(stmts, vars)?;
        }
        Ok(())
    }

    /// Compile a contract condition as a runtime assert (for --verify-contracts)
    pub(super) fn compile_contract_assert(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
        msg: &str,
    ) -> MimiResult<()> {
        let cond_val = self.compile_expr(expr, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            iv
        } else {
            return Err(CompileError::ContractCondition(format!("{:?}", cond_val.get_type())));
        };

        let function = self.current_function()
            .ok_or_else(|| CompileError::LlvmError("codegen: no current function for contract assert".to_string()))?;
        let pass_bb = self.context.append_basic_block(function, "contract_pass");
        let fail_bb = self.context.append_basic_block(function, "contract_fail");

        self.builder.build_conditional_branch(cond_bool, pass_bb, fail_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(fail_bb);
        let contract_text = format!("{:?}", expr);
        let full_msg = format!("{} | contract: {}", msg, contract_text);
        let msg_ptr = self.builder.build_global_string_ptr(&full_msg, "contract_msg")
            .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
        let abort_fn = self.module.get_function("mimi_runtime_abort")
            .unwrap_or_else(|| {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let ty = self.context.void_type().fn_type(&[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ], false);
                self.module.add_function("mimi_runtime_abort", ty, Some(inkwell::module::Linkage::External))
            });
        self.builder.build_call(abort_fn, &[
            BasicMetadataValueEnum::PointerValue(msg_ptr.as_pointer_value()),
        ], "abort_call")
            .map_err(|e| CompileError::LlvmError(format!("abort call error: {}", e)))?;
        self.builder.build_unconditional_branch(pass_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(pass_bb);
        Ok(())
    }

    /// Push a new capability scope
    pub(super) fn push_cap_scope(&mut self) {
        self.cap_vars.push(HashMap::new());
    }

    /// Pop the current capability scope
    pub(super) fn pop_cap_scope(&mut self) {
        self.cap_vars.pop();
    }

    /// Register a capability variable in the current scope
    pub(super) fn register_cap(&mut self, name: &str, ptr: inkwell::values::PointerValue<'ctx>) {
        if let Some(scope) = self.cap_vars.last_mut() {
            scope.insert(name.to_string(), (ptr, false));
        }
    }

    /// Mark a capability as consumed
    pub(super) fn consume_cap(&mut self, name: &str) -> MimiResult<()> {
        for scope in self.cap_vars.iter_mut().rev() {
            if let Some((_, consumed)) = scope.get_mut(name) {
                if *consumed {
                    return Err(CompileError::CapConsumed(name.to_string()));
                }
                *consumed = true;
                return Ok(());
            }
        }
        Ok(()) // Not a capability variable
    }

    /// Check if a variable is a consumed capability
    pub(super) fn is_cap_consumed(&self, name: &str) -> bool {
        for scope in self.cap_vars.iter().rev() {
            if let Some((_, consumed)) = scope.get(name) {
                return *consumed;
            }
        }
        false
    }

    /// Check if a variable is a capability variable
    pub(super) fn is_cap_var(&self, name: &str) -> bool {
        for scope in self.cap_vars.iter().rev() {
            if scope.contains_key(name) {
                return true;
            }
        }
        false
    }

    /// Push a new shared variable scope
    pub(super) fn push_shared_scope(&mut self) {
        self.shared_release_vars.push(Vec::new());
        self.weak_release_vars.push(Vec::new());
    }

    /// Pop the current shared variable scope and emit release calls for all
    /// shared and weak variables declared in it.
    pub(super) fn pop_shared_scope(&mut self) -> MimiResult<()> {
        if let Some(scope) = self.shared_release_vars.pop() {
            if let Some(release_fn) = self.module.get_function("mimi_rc_release") {
                for heap_ptr in &scope {
                    self.builder.build_call(release_fn, &[
                        inkwell::values::BasicMetadataValueEnum::PointerValue(*heap_ptr),
                    ], "shared_release")
                        .map_err(|e| CompileError::LlvmError(format!("shared release error: {}", e)))?;
                }
            }
        }
        if let Some(scope) = self.weak_release_vars.pop() {
            if let Some(release_fn) = self.module.get_function("mimi_rc_weak_release") {
                for heap_ptr in &scope {
                    self.builder.build_call(release_fn, &[
                        inkwell::values::BasicMetadataValueEnum::PointerValue(*heap_ptr),
                    ], "weak_release")
                        .map_err(|e| CompileError::LlvmError(format!("weak release error: {}", e)))?;
                }
            }
        }
        Ok(())
    }

    /// Release all remaining shared and weak variables at function exit
    pub(super) fn release_all_shared(&mut self) -> MimiResult<()> {
        let all_release: Vec<inkwell::values::PointerValue<'ctx>> = self.shared_release_vars
            .iter()
            .flat_map(|scope| scope.iter())
            .copied()
            .collect();
        if let Some(release_fn) = self.module.get_function("mimi_rc_release") {
            for heap_ptr in all_release {
                self.builder.build_call(release_fn, &[
                    inkwell::values::BasicMetadataValueEnum::PointerValue(heap_ptr),
                ], "shared_release")
                    .map_err(|e| CompileError::LlvmError(format!("shared release error: {}", e)))?;
            }
        }
        let all_weak: Vec<inkwell::values::PointerValue<'ctx>> = self.weak_release_vars
            .iter()
            .flat_map(|scope| scope.iter())
            .copied()
            .collect();
        if let Some(release_fn) = self.module.get_function("mimi_rc_weak_release") {
            for heap_ptr in all_weak {
                self.builder.build_call(release_fn, &[
                    inkwell::values::BasicMetadataValueEnum::PointerValue(heap_ptr),
                ], "weak_release")
                    .map_err(|e| CompileError::LlvmError(format!("weak release error: {}", e)))?;
            }
        }
        self.shared_release_vars.clear();
        self.shared_release_vars.push(Vec::new());
        self.weak_release_vars.clear();
        self.weak_release_vars.push(Vec::new());
        Ok(())
    }

    /// Register a shared variable's heap pointer for release on scope exit
    pub(super) fn register_shared_var(&mut self, heap_ptr: inkwell::values::PointerValue<'ctx>) {
        if let Some(scope) = self.shared_release_vars.last_mut() {
            scope.push(heap_ptr);
        }
    }

    /// Register a weak variable's heap pointer for weak_release on scope exit
    pub(super) fn register_weak_var(&mut self, heap_ptr: inkwell::values::PointerValue<'ctx>) {
        if let Some(scope) = self.weak_release_vars.last_mut() {
            scope.push(heap_ptr);
        }
    }

    /// Check for unconsumed capabilities at scope exit
    pub(super) fn check_unconsumed_caps(&self) -> MimiResult<()> {
        if let Some(scope) = self.cap_vars.last() {
            for (name, (_, consumed)) in scope {
                if !consumed {
                    return Err(CompileError::CapNotConsumed(name.to_string()));
                }
            }
        }
        Ok(())
    }
}
