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
        self.parasteps_future_ptrs.clear();
    }

    /// Leave parallel parasteps mode: ensure all spawned futures are completed
    pub(super) fn leave_parasteps(&mut self) -> MimiResult<()> {
        if !self.in_parasteps {
            return Ok(());
        }
        // Wait for any thread-spawned futures not yet awaited
        if !self.parasteps_future_ptrs.is_empty() {
            let await_fn = self.module.get_function("mimi_await_future");
            if let Some(await_fn) = await_fn {
                for &(future_ptr, _) in &self.parasteps_future_ptrs {
                    self.build_call(
                        await_fn,
                        &[BasicMetadataValueEnum::PointerValue(future_ptr)],
                        "parasteps_await",
                    )?;
                }
            }
            // Free all futures
            if let Some(free_fn) = self.module.get_function("mimi_future_free") {
                for &(future_ptr, _) in &self.parasteps_future_ptrs {
                    self.build_call(
                        free_fn,
                        &[BasicMetadataValueEnum::PointerValue(future_ptr)],
                        "parasteps_future_free",
                    )?;
                }
            }
        }
        self.parasteps_future_ptrs.clear();
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
            return Err(CompileError::ContractCondition(format!(
                "{:?}",
                cond_val.get_type()
            )));
        };

        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for contract assert".to_string())
        })?;
        // QUAL-5 fix: use unique BB names to avoid conflicts when multiple
        // contract asserts exist in the same function (e.g., multiple ensures clauses).
        let id = self.contract_bb_counter;
        self.contract_bb_counter += 1;
        let pass_bb = self
            .context
            .append_basic_block(function, &format!("contract_pass_{}", id));
        let fail_bb = self
            .context
            .append_basic_block(function, &format!("contract_fail_{}", id));

        self.build_cond_br(cond_bool, pass_bb, fail_bb)?;

        self.builder.position_at_end(fail_bb);
        let contract_text = format!("{:?}", expr);
        let full_msg = format!("{} | contract: {}", msg, contract_text);
        let msg_ptr = self
            .builder
            .build_global_string_ptr(&full_msg, "contract_msg")
            .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
        let abort_fn = self
            .module
            .get_function("mimi_runtime_abort")
            .unwrap_or_else(|| {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let ty = self
                    .context
                    .void_type()
                    .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
                self.module.add_function(
                    "mimi_runtime_abort",
                    ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(
                msg_ptr.as_pointer_value(),
            )],
            "abort_call",
        )?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
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
        // CG-H12 (deep audit): deduplicate pointers within a scope before
        // releasing — a value registered more than once (e.g. captured into a
        // closure and also used directly) must not be released twice.
        if let Some(scope) = self.shared_release_vars.pop() {
            if let Some(release_fn) = self.module.get_function("mimi_rc_release") {
                let mut seen = std::collections::HashSet::new();
                for heap_ptr in scope {
                    if !seen.insert(heap_ptr) {
                        continue;
                    }
                    self.build_call(
                        release_fn,
                        &[BasicMetadataValueEnum::PointerValue(heap_ptr)],
                        "shared_release",
                    )?;
                }
            }
        }
        if let Some(scope) = self.weak_release_vars.pop() {
            if let Some(release_fn) = self.module.get_function("mimi_rc_weak_release") {
                let mut seen = std::collections::HashSet::new();
                for heap_ptr in scope {
                    if !seen.insert(heap_ptr) {
                        continue;
                    }
                    self.build_call(
                        release_fn,
                        &[BasicMetadataValueEnum::PointerValue(heap_ptr)],
                        "weak_release",
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Release all remaining shared and weak variables at function exit
    pub(super) fn release_all_shared(&mut self) -> MimiResult<()> {
        // CG-H12 (deep audit): collect every registered pointer across all
        // scopes but release each *unique* pointer exactly once. Without
        // deduplication, a heap pointer that was registered in more than one
        // nested scope would be released multiple times (double free / UAF).
        let mut seen = std::collections::HashSet::new();
        let all_release: Vec<inkwell::values::PointerValue<'ctx>> = self
            .shared_release_vars
            .iter()
            .flat_map(|scope| scope.iter())
            .copied()
            .filter(|p| seen.insert(*p))
            .collect();
        if let Some(release_fn) = self.module.get_function("mimi_rc_release") {
            for heap_ptr in all_release {
                self.build_call(
                    release_fn,
                    &[BasicMetadataValueEnum::PointerValue(heap_ptr)],
                    "shared_release",
                )?;
            }
        }
        let mut seen_weak = std::collections::HashSet::new();
        let all_weak: Vec<inkwell::values::PointerValue<'ctx>> = self
            .weak_release_vars
            .iter()
            .flat_map(|scope| scope.iter())
            .copied()
            .filter(|p| seen_weak.insert(*p))
            .collect();
        if let Some(release_fn) = self.module.get_function("mimi_rc_weak_release") {
            for heap_ptr in all_weak {
                self.build_call(
                    release_fn,
                    &[BasicMetadataValueEnum::PointerValue(heap_ptr)],
                    "weak_release",
                )?;
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
