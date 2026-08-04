use crate::ast::*;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;
use crate::error::{CompileError, MimiResult};

/// Which clause of a function contract a runtime assertion checks.
/// Rendered into the violation message so both backends use the same phrasing
/// (bytecode VM: `requires condition failed for '<fn>'` / E0808).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContractPhase {
    Requires,
    Ensures,
}

impl ContractPhase {
    fn label(self) -> &'static str {
        match self {
            ContractPhase::Requires => "requires",
            ContractPhase::Ensures => "ensures",
        }
    }
}

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

    /// 0.31.24: Push a new defer scope level
    pub(super) fn push_defer_scope(&mut self) {
        self.defer_scope_stack.push(self.defer_blocks.len());
    }

    /// 0.31.24: Pop the current defer scope level and compile all defer blocks in LIFO order.
    /// Unlike compensation scopes, defer blocks always run (on normal exit and error exit).
    pub(super) fn pop_defer_scope(
        &mut self,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        if let Some(start) = self.defer_scope_stack.pop() {
            // Compile defer blocks in reverse order (LIFO)
            let blocks: Vec<Block> = self.defer_blocks[start..].iter().rev().cloned().collect();
            self.defer_blocks.truncate(start);
            for stmts in &blocks {
                self.compile_block(stmts, vars)?;
            }
        }
        Ok(())
    }

    /// 0.31.24: Register a defer block for LIFO execution on scope exit
    pub(super) fn register_defer(&mut self, stmts: &Block) {
        self.defer_blocks.push(stmts.clone());
    }

    /// H3 (audit-codegen): drop pending defers WITHOUT executing them — used by
    /// the panic→Fault absorption return path, mirroring the bytecode VM which
    /// truncates the frame on absorption (defer never runs there either).
    pub(super) fn discard_defer_scope(&mut self) {
        if let Some(start) = self.defer_scope_stack.pop() {
            self.defer_blocks.truncate(start);
        }
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
        phase: ContractPhase,
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
        // Message is embedded at compile time; render it the way the bytecode
        // VM reports violations (E0808), with span + source line instead of an
        // internal AST dump.
        let full_msg = self.build_contract_violation_message(expr, phase);
        let msg_ptr = self
            .builder
            .build_global_string_ptr(&full_msg, "contract_msg")
            .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
        let abort_fn = self.get_or_declare_abort_fn();
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

    /// Build the embedded violation message for a contract assertion.
    ///
    /// Single dense line, phrasing aligned with the bytecode VM's E0808
    /// report, machine-first (no gutter/caret/arrow decoration — coordinates
    /// carry the positional information):
    /// ```text
    /// [E0808] requires condition failed for 'div': b != 0 @ src.mimi:2:15-21 | hint: rebuild without --verify-contracts to disable contract checking.
    /// ```
    /// The expression is rendered back to source-like text (`expr_render`),
    /// never Debug-dumped. Location is best-effort: synthetic/desugared
    /// contracts without spans degrade to the message line only.
    fn build_contract_violation_message(&self, expr: &Expr, phase: ContractPhase) -> String {
        // Owner name comes from the function currently being compiled.
        // Regular functions use their bare name; actor methods carry a mangled
        // LLVM name (`Actor__method__method`), which is pretty-printed to the
        // `Actor::method` form users write. Generic instantiations keep their
        // mangled name — that is still more actionable than an AST dump.
        let owner = self
            .current_function()
            .map(|f| {
                let name = f.get_name().to_string_lossy().into_owned();
                match name.strip_suffix("__method") {
                    Some(stripped) => stripped.replace("__", "::"),
                    None => name,
                }
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        let contract_text = crate::expr_render::render_expr(expr);
        let mut msg = format!(
            "[E0808] {} condition failed for '{}': {}",
            phase.label(),
            owner,
            contract_text
        );

        if let Some(meta) = expr.meta() {
            let span = meta.span;
            if span.start_line > 0 {
                let label = self.contract_location_label(span.source_id);
                let columns = if span.start_col > 0 {
                    if span.end_line == span.start_line && span.end_col > span.start_col {
                        format!(":{}-{}", span.start_col, span.end_col)
                    } else {
                        format!(":{}", span.start_col)
                    }
                } else {
                    String::new()
                };
                msg.push_str(&format!(" @ {}:{}{}", label, span.start_line, columns));
            }
        }

        msg.push_str(" | hint: rebuild without --verify-contracts to disable contract checking.");
        msg
    }

    /// Resolve a `SourceId` to a display label for contract messages:
    /// disk path when available, then canonical URI, then the registry key.
    /// In-memory sources (test harnesses) degrade to their key — coordinates
    /// remain exact either way.
    fn contract_location_label(&self, source_id: crate::span::SourceId) -> String {
        self.comptime_file
            .as_ref()
            .and_then(|f| f.sources.record(source_id))
            .map(|record| {
                record
                    .disk_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .or_else(|| record.canonical_uri.clone())
                    .unwrap_or_else(|| record.key.to_string())
            })
            .unwrap_or_else(|| "<unknown source>".to_string())
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
        // Each entry represents one owned strong/weak count, not one unique
        // address. The same pointer may legitimately appear more than once
        // after clone/upgrade; every successful retain must have a matching
        // release. Deduplicating by PointerValue leaks those extra counts.
        if let Some(scope) = self.shared_release_vars.pop() {
            if let Some(release_fn) = self.module.get_function("mimi_rc_release") {
                for heap_ptr in scope {
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
                for heap_ptr in scope {
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

    /// Emit releases for every ownership count active on the current return
    /// path without mutating the compiler's scope stacks. Branch codegen is
    /// path-insensitive: clearing outer scopes for a returning `if` arm would
    /// make the fallthrough arm leak those same values.
    pub(super) fn emit_all_shared_releases(&mut self) -> MimiResult<()> {
        let strong: Vec<_> = self
            .shared_release_vars
            .iter()
            .flat_map(|scope| scope.iter().copied())
            .collect();
        if let Some(release_fn) = self.module.get_function("mimi_rc_release") {
            for heap_ptr in strong {
                self.build_call(
                    release_fn,
                    &[BasicMetadataValueEnum::PointerValue(heap_ptr)],
                    "shared_return_release",
                )?;
            }
        }

        let weak: Vec<_> = self
            .weak_release_vars
            .iter()
            .flat_map(|scope| scope.iter().copied())
            .collect();
        if let Some(release_fn) = self.module.get_function("mimi_rc_weak_release") {
            for heap_ptr in weak {
                self.build_call(
                    release_fn,
                    &[BasicMetadataValueEnum::PointerValue(heap_ptr)],
                    "weak_return_release",
                )?;
            }
        }
        Ok(())
    }

    /// Balance compiler bookkeeping for a block whose runtime path has already
    /// emitted full return cleanup.
    pub(super) fn discard_shared_scope(&mut self) {
        self.shared_release_vars.pop();
        self.weak_release_vars.pop();
    }

    /// Release all remaining shared and weak variables at function exit
    pub(super) fn release_all_shared(&mut self) -> MimiResult<()> {
        // Entries are ownership counts, so preserve duplicates across scopes.
        let all_release: Vec<inkwell::values::PointerValue<'ctx>> = self
            .shared_release_vars
            .iter()
            .flat_map(|scope| scope.iter())
            .copied()
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
        let all_weak: Vec<inkwell::values::PointerValue<'ctx>> = self
            .weak_release_vars
            .iter()
            .flat_map(|scope| scope.iter())
            .copied()
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
