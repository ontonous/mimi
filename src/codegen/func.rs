#![allow(clippy::unwrap_used)]
use crate::ast::*;
use std::collections::HashMap;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::AddressSpace;

use crate::codegen::CallSiteValueExt;
use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

// Submodules for clearly independent method groups. The originally suggested
// groups (params, actor, shared) do not map to standalone methods in this file:
//
// - Ordinary function-body parameter binding and ABI emission now belong to the
//   CheckedProgram/MIR routes; the old surface-AST body compiler was removed.
// - Actor constructor / method compilation already lives in `codegen/actors.rs`.
// - Shared / RC scope cleanup helpers already live in `codegen/scope.rs` and `codegen/mod.rs`.
//
// What was split out:
// - `func/body.rs`: statement-level body helpers (loops and assignment forms).
// - `func/pattern.rs`: recursive `compile_pattern_bind`.
mod body;
mod export;
mod pattern;

impl<'ctx> CodeGenerator<'ctx> {
    fn is_string_llvm_type(ty: BasicTypeEnum<'ctx>) -> bool {
        match ty {
            BasicTypeEnum::StructType(st) => {
                let fields = st.get_field_types();
                matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(_)]
                )
            }
            _ => false,
        }
    }

    /// Check if an expression produces a heap-allocated string whose allocation
    /// is tracked by `heap_allocs`.  Such expressions need their heap pointer
    /// popped from the tracking stack before `free_heap_allocs` runs, otherwise
    /// the string data gets freed before the caller can use it.
    ///
    /// Note: the `str_*` / `format` / `to_string` matchers assume those names
    /// are the Mimi builtins. If a user shadows one of these names with their
    /// own function, this check will mis-classify the result. The names are
    /// hardcoded rather than `starts_with("str_")` (audit CG-H12) precisely
    /// to avoid the prefix-collision case; the remaining risk is a deliberate
    /// user name collision.
    fn is_string_temp_expr(expr: &Expr, val: &BasicValueEnum<'ctx>) -> bool {
        match expr.unlocated() {
            Expr::Binary(BinOp::Add, _, _) => true,
            Expr::Literal(Lit::FString(_)) => true,
            Expr::Call(callee, _) => {
                matches!(val, BasicValueEnum::PointerValue(_))
                    || matches!(
                        callee.unlocated(),
                        Expr::Ident(name)
                            if matches!(
                                name.as_str(),
                                "str_concat"
                                    | "str_repeat"
                                    | "str_slice"
                                    | "str_trim"
                                    | "str_join"
                                    | "str_from"
                                    | "to_string"
                                    | "format"
                            )
                    )
            }
            _ => false,
        }
    }

    /// MEM-C13: if returning a closure `{fn_ptr, env_ptr}`, claim the env heap
    /// pointer so `free_heap_allocs` does not free an env the caller still owns.
    ///
    /// B9 (audit): the claim is now value-exact. The returned env pointer is
    /// recorded in `claimed_returned_envs`, and the immediately following
    /// `free_heap_allocs` emits runtime guards (`ptr != claimed_env` → free).
    /// The old positional `pop_last_heap_ptr` misfired whenever an unrelated
    /// allocation was registered after the env (it popped the wrong entry,
    /// leaking the env or double-freeing data the caller received).
    pub(in crate::codegen) fn claim_returned_closure_env(
        &self,
        val: BasicValueEnum<'ctx>,
        ret_type: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let is_closure = match ret_type {
            BasicTypeEnum::StructType(st) => {
                let fields = st.get_field_types();
                matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::PointerType(_)]
                )
            }
            _ => false,
        };
        if !is_closure {
            return Ok(val);
        }
        let sv = match val {
            BasicValueEnum::StructValue(sv) => sv,
            _ => return Ok(val),
        };
        // The env is the closure struct's second field. For non-capturing
        // closures it is null — claiming null is harmless (guards only match
        // null envs, and free(null) is a C no-op).
        if let BasicValueEnum::PointerValue(env_ptr) =
            self.build_extract_value(sv.into(), 1, "b9_env_claim")?
        {
            self.claim_closure_env(env_ptr)?;
        }
        Ok(val)
    }

    pub(in crate::codegen) fn surface_ownership_class(
        &self,
        ty: &Type,
    ) -> crate::codegen::abi::ownership::OwnershipClass {
        let concrete = self.resolve_type(ty);
        crate::codegen::abi::ownership::classify_surface(
            &concrete,
            &self.type_defs,
            &self.checked_alias_primitives,
        )
    }

    pub(in crate::codegen) fn clone_surface_structural_return_with_glue(
        &self,
        ret_ty_ast: Option<&Type>,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let Some(ret_ty_ast) = ret_ty_ast else {
            return Ok(value);
        };
        let ownership = self.surface_ownership_class(ret_ty_ast);
        if !matches!(
            ownership,
            crate::codegen::abi::ownership::OwnershipClass::Tuple(_)
                | crate::codegen::abi::ownership::OwnershipClass::Record(_)
                | crate::codegen::abi::ownership::OwnershipClass::Option(_)
                | crate::codegen::abi::ownership::OwnershipClass::Result { .. }
        ) {
            return Ok(value);
        }
        Ok(self
            .clone_return_with_derived_glue(&ownership, value)?
            .unwrap_or(value))
    }

    pub(in crate::codegen) fn claim_string_return_value(
        &self,
        val: BasicValueEnum<'ctx>,
        ret_type: BasicTypeEnum<'ctx>,
        expr: Option<&Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // F-004 (0.40.1.8): fail-closed returning a closure that captures a heap
        // value on the native backend. Such a closure escapes the enclosing scope,
        // but its captured data array is freed when that scope exits (closure
        // captures have no escape-set-null like direct returns do) → use-after-free
        // when the escaped closure is later called. Mirror the E0723 ownership gate
        // instead of silently corrupting memory (mimi run / VM backend is unaffected).
        if self.returned_closure_captures_heap(expr, vars)? {
            return Err(CompileError::UnsupportedReturn(
                "returning a closure that captures a heap collection (List/Set/Map, or a \
                 container such as Option/Result/Tuple holding one) from a native (LLVM) \
                 function is not yet supported: the captured data array is freed when the \
                 enclosing scope exits, leaving the returned closure with a dangling pointer \
                 (use-after-free). Use `mimi run` (VM backend), or restructure so the closure \
                 does not escape with captured heap data. Tracked as 0.1.10 A2 \
                 ownership-glue work (E0723)."
                    .to_string(),
            ));
        }
        // Closures are not strings; claim env ownership first when applicable.
        let mut val = self.claim_returned_closure_env(val, ret_type)?;
        // A2 (0.40.3.1): the first opt-in adoption slice clones canonical
        // StringBox returns through derived module glue. The returned copy is
        // deliberately not registered in the callee scope; ordinary cleanup
        // frees the original while caller-side lifetime tracking owns the copy.
        if self.value_glue_enabled()
            && Self::is_string_llvm_type(ret_type)
            && matches!(val, BasicValueEnum::StructValue(_))
        {
            return self.clone_value_with_derived_glue(
                &crate::codegen::abi::ownership::OwnershipClass::StringBox,
                val,
            );
        }
        // Structural returns adopted by A2 are normalized once, after the value
        // has been loaded/coerced into a by-value struct. Skip the legacy
        // LLVM-shape field probe here so it cannot clone/claim child strings
        // before the product glue clones the whole ownership value.
        if self.value_glue_enabled() {
            if let Some(ret_ty_ast) = self.current_fn_ret_ty_ast.as_ref() {
                let ownership = self.surface_ownership_class(ret_ty_ast);
                if matches!(
                    ownership,
                    crate::codegen::abi::ownership::OwnershipClass::Tuple(_)
                        | crate::codegen::abi::ownership::OwnershipClass::Record(_)
                        | crate::codegen::abi::ownership::OwnershipClass::Option(_)
                        | crate::codegen::abi::ownership::OwnershipClass::Result { .. }
                ) && self.value_glue_can_adopt_return(&ownership, ret_type)?
                {
                    return Ok(val);
                }
            }
        }
        // 0.35.23 deep-eval (mimi-make native UAF): aggregate returns that
        // CARRY string fields — `(string, string)` tuples and records —
        // leave the element heap buffers registered in this function's heap
        // scope; the scope-exit flush freed them while the returned
        // aggregate still referenced them (parse_variable's pair was later
        // fed to mimi_str_clone: src == freed-and-reused dst →
        // copy_nonoverlapping overlap abort). Claim each string-shaped
        // field's data pointer so the guarded flush skips it — ownership
        // transfers to the caller (same contract as closure envs, B9).
        // Record/tuple values may arrive as an alloca POINTER (the implicit
        // return path loads them only after this claim runs). Pre-check the
        // field shapes FIRST so no IR is emitted unless a string field can
        // actually be claimed (golden-IR stability for every other shape).
        fn agg_claim_shape(st: inkwell::types::StructType) -> bool {
            let fields = st.get_field_types();
            let is_plain_string = matches!(
                fields.as_slice(),
                [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(_)]
            );
            fields.len() >= 2
                && !is_plain_string
                && fields.iter().any(|f| match f {
                    BasicTypeEnum::StructType(inner) => {
                        let fs = inner.get_field_types();
                        matches!(
                            fs.as_slice(),
                            [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(_)]
                        )
                    }
                    _ => false,
                })
        }
        let agg_claim_val: Option<(
            inkwell::types::StructType<'ctx>,
            inkwell::values::StructValue<'ctx>,
            Option<inkwell::values::PointerValue<'ctx>>,
        )> = match (ret_type, val) {
            (BasicTypeEnum::StructType(st), BasicValueEnum::StructValue(sv))
                if agg_claim_shape(st) =>
            {
                Some((st, sv, None))
            }
            (BasicTypeEnum::StructType(st), BasicValueEnum::PointerValue(pv))
                if agg_claim_shape(st) =>
            {
                match self.build_load(BasicTypeEnum::StructType(st), pv, "agg_claim_ld") {
                    Ok(BasicValueEnum::StructValue(sv)) => Some((st, sv, Some(pv))),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some((st, sv, pv)) = agg_claim_val {
            let fields = st.get_field_types();
            let is_plain_string = matches!(
                fields.as_slice(),
                [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(_)]
            );
            if fields.len() >= 2 && !is_plain_string {
                // 0.39.x (L1 parity fix): string-shaped fields used to be
                // CLAIMED only (ownership transfer). That is unsound when the
                // leaf merely ALIASES callee input — e.g. a `.rodata` literal
                // argument packed into a returned tuple/Result by a
                // monomorphized generic instance (`func split<T>(v: T) ->
                // (T, i32)` called with a literal): the caller-side tracking
                // then frees a global (free() abort). Normalize every
                // string-shaped leaf through the same runtime probe used for
                // top-level string returns: heap-owned values transfer
                // untouched, borrowed values are replaced by fresh
                // heap copies the caller legitimately owns.
                let mut rebuilt = sv;
                for (idx, fty) in fields.iter().enumerate() {
                    if Self::is_string_llvm_type(*fty) {
                        if let Ok(BasicValueEnum::StructValue(fsv)) =
                            self.build_extract_value(sv.into(), idx as u32, "agg_str_field")
                        {
                            let normalized = self.claim_resolved_string_return(fsv.into())?;
                            if let BasicValueEnum::StructValue(nsv) = normalized {
                                rebuilt = self
                                    .builder
                                    .build_insert_value(rebuilt, nsv, idx as u32, "agg_str_owned")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("agg str own rebuild: {e}"))
                                    })?
                                    .into_struct_value();
                            }
                            // Claim the original data pointer regardless: for
                            // heap-owned leaves this pops the callee-scope
                            // registration (transfer, as before); for copied
                            // leaves the borrowed source has no registration
                            // and the claim is a no-op.
                            if let BasicValueEnum::PointerValue(data) =
                                self.build_extract_value(fsv.into(), 0, "agg_str_data")?
                            {
                                self.claim_closure_env(data)?;
                            }
                        }
                    }
                }
                match pv {
                    // Implicit-return path loads the aggregate from the
                    // alloca AFTER this claim runs — write the normalized
                    // value back so the copies are visible.
                    Some(pv) => {
                        self.build_store(pv.into(), BasicValueEnum::StructValue(rebuilt))?;
                    }
                    None => {
                        val = BasicValueEnum::StructValue(rebuilt);
                    }
                }
            }
        }
        let is_string_struct = match ret_type {
            BasicTypeEnum::StructType(st) => {
                let fields = st.get_field_types();
                matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(_)]
                )
            }
            _ => false,
        };
        if !is_string_struct {
            // Check for variant struct (Option/Result) whose payload is a string.
            // E.g. `Ok(s + "-wrapped")` returns `Result<string, ...>` – the inner
            // string concat's heap allocation must survive free_heap_allocs.
            let is_variant_with_string_payload = match ret_type {
                BasicTypeEnum::StructType(st) => {
                    let fields = st.get_field_types();
                    matches!(
                        fields.as_slice(),
                        [BasicTypeEnum::IntType(it), payload, ..]
                            if it.get_bit_width() == 1 && Self::is_string_llvm_type(*payload)
                    )
                }
                _ => false,
            };
            // Also handle Result<T, string> (string in Err payload, widened
            // to i64 in the Result struct).  The LLVM type of field 2 is i64
            // (not a string struct), so we detect this by checking whether the
            // return expression is `Err(str_expr)` where str_expr produces a
            // heap-tracked string.
            let is_err_with_string_arg = match expr {
                Some(crate::ast::Expr::Call(callee, args)) => {
                    matches!(callee.unlocated(), crate::ast::Expr::Ident(n) if n == "Err")
                        && args.len() == 1
                }
                _ => false,
            };
            if is_variant_with_string_payload || is_err_with_string_arg {
                // Pop the inner string's heap allocation so free_heap_allocs
                // does not free the returned string before the caller can use it.
                if let Some(expr) = expr {
                    if let Expr::Call(_, args) = expr.unlocated() {
                        if let [arg] = args.as_slice() {
                            if Self::is_string_temp_expr(arg, &val) {
                                let _ = self.pop_last_heap_ptr();
                            }
                        }
                    }
                }
            }
            return Ok(val);
        }

        // Returning a string variable: load the struct value and null out the
        // variable slot's data pointer so the slot is not freed before return.
        if let Some(expr) = expr {
            if let Expr::Ident(name) = expr.unlocated() {
                if self
                    .var_type_names
                    .get(name)
                    .map(|t| t == "string")
                    .unwrap_or(false)
                {
                    if let Some(&(alloca, ty)) = vars.get(name) {
                        let loaded = self.build_load(ty, alloca, &format!("{}_ret", name))?;
                        let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                        if let BasicTypeEnum::StructType(st) = ty {
                            if let Ok(data_gep) = self.gep().build_struct_gep(
                                st,
                                alloca,
                                0,
                                &format!("{}_ret_null", name),
                            ) {
                                self.build_store(data_gep, null_ptr)?;
                            }
                        }
                        // CLOSE-GAP-5: heap-copy the loaded struct so the caller
                        // side has unambiguous ownership. The original data may be
                        // a `.rodata` global (for `let s = "hi"; s`), in which case
                        // without this copy the caller would `free()` a global
                        // pointer.
                        return self.heap_copy_string_value(loaded);
                    }
                }
            }
        }

        // For concat / fstring / builtin raw returns, the most-recent heap
        // registration owns the returned data; pop it so free_heap_allocs
        // doesn't release it before the caller sees it.
        let is_string_temp = match expr.map(Expr::unlocated) {
            Some(Expr::Binary(BinOp::Add, _, _)) => true,
            Some(Expr::Literal(Lit::FString(_))) => true,
            Some(Expr::Call(callee, _)) => {
                matches!(val, BasicValueEnum::PointerValue(_))
                    || matches!(
                        callee.unlocated(),
                        Expr::Ident(name) if name.starts_with("str_") || name == "to_string"
                    )
            }
            _ => false,
        };
        if is_string_temp {
            let _ = self.pop_last_heap_ptr();
        }

        match val {
            BasicValueEnum::PointerValue(pv) => {
                // Raw pointer result (string literal or builtin raw return).
                // `heap_copy_string_value` handles the wrap (via strlen) +
                // copy in one step.
                self.heap_copy_string_value(pv.into())
            }
            BasicValueEnum::StructValue(sv) => {
                // The struct's data pointer is referenced by the caller. If
                // ownership was transferred (pop in the previous block), the
                // data ptr is heap-owned by the result; the caller will free
                // it. If we did not pop (e.g. literal, expr = None), the data
                // ptr is a `.rodata` global — heap-copy it first.
                if is_string_temp {
                    Ok(BasicValueEnum::StructValue(sv))
                } else {
                    self.heap_copy_string_value(sv.into())
                }
            }
            _ => Ok(val),
        }
    }

    /// Resolved-emitter string-return ownership contract (deep-eval
    /// 2026-08-09; demos/04_adt_match native abort): legacy return funnels
    /// claim string ownership via expression-shape heuristics
    /// (`claim_string_return_value`), but resolved-emitter returns lack that
    /// path — a match/if merge can hand back a `.rodata` literal while the
    /// caller-side `track_string_return_lifetime` unconditionally frees the
    /// returned data pointer (free(global) → munmap_chunk abort).
    ///
    /// Runtime ownership probe: compare the data pointer against null and
    /// every live heap registration of the current function scope
    /// (`heap_probe_candidates`); when nothing matches, heap-copy so the
    /// returned pointer is always malloc-owned. Heap-matching values
    /// transfer ownership WITHOUT a copy (`drain_heap_scope` drops the
    /// registration on the return path), so concat-heavy returns neither
    /// crash nor leak. Null counts as owned: free(null) is a no-op.
    pub(in crate::codegen) fn claim_resolved_string_return(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let BasicValueEnum::StructValue(sv) = val else {
            return Ok(val);
        };
        let sty = sv.get_type();
        let fields = sty.get_field_types();
        let is_string_struct = matches!(
            fields.as_slice(),
            [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(_)]
        );
        if !is_string_struct {
            return Ok(val);
        }
        if self.value_glue_enabled() {
            return self.clone_value_with_derived_glue(
                &crate::codegen::abi::ownership::OwnershipClass::StringBox,
                val,
            );
        }
        let data_pv = match self.build_extract_value(sv.into(), 0, "res_ret_data")? {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Ok(val),
        };
        let i64_ty = self.context.i64_type();
        let mut data_i = self.build_ptr_to_int(data_pv, i64_ty, "res_ret_data_i")?;
        // CVP-CRASH-001 (0.39.x sweep): when data_pv is an LLVM CONSTANT (a
        // global string literal returned by value — `default_string()`), the
        // ptrtoint folds into a ConstantExpr and the whole ownership-probe
        // branch condition becomes `br i1 icmp constexpr ...` — invalid IR
        // that LLVM 18's CalledValuePropagation SIGSEGVs on (prelude.mimi
        // compile crash). Materialize constant probes through an alloca
        // round-trip so every probe node stays a real instruction.
        if data_i.is_const() {
            let slot = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "res_ret_data_i_slot")?;
            self.build_store(slot, data_i)?;
            data_i = self
                .build_load(BasicTypeEnum::IntType(i64_ty), slot, "res_ret_data_i_load")?
                .into_int_value();
        }
        let candidates = self.heap_probe_candidates();
        let mut owned = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                data_i,
                i64_ty.const_int(0, false),
                "res_ret_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("res ret null cmp: {}", e)))?;
        for cand in candidates {
            let eq = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, data_i, cand, "res_ret_cand_eq")
                .map_err(|e| CompileError::LlvmError(format!("res ret cmp: {}", e)))?;
            owned = self
                .builder
                .build_or(owned, eq, "res_ret_owned")
                .map_err(|e| CompileError::LlvmError(format!("res ret or: {}", e)))?;
        }
        let parent = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| CompileError::LlvmError("res ret claim outside function".into()))?;
        let heap_bb = self.context.append_basic_block(parent, "res_ret_heap");
        let copy_bb = self.context.append_basic_block(parent, "res_ret_copy");
        let cont_bb = self.context.append_basic_block(parent, "res_ret_cont");
        self.build_cond_br(owned, heap_bb, copy_bb)?;
        self.builder.position_at_end(heap_bb);
        self.build_br(cont_bb)?;
        self.builder.position_at_end(copy_bb);
        let copied = self.heap_copy_string_value(sv.into())?;
        // heap_copy_string_value branches internally (malloc OOM abort), so
        // the builder may now sit in a successor of copy_bb — the phi
        // predecessor must be that ACTUAL block.
        let copy_end_bb = self
            .builder
            .get_insert_block()
            .ok_or_else(|| CompileError::LlvmError("res ret copy lost block".into()))?;
        self.build_br(cont_bb)?;
        self.builder.position_at_end(cont_bb);
        let BasicValueEnum::StructValue(copied_sv) = copied else {
            return Ok(copied);
        };
        let phi = self
            .builder
            .build_phi(sty, "res_ret_val")
            .map_err(|e| CompileError::LlvmError(format!("res ret phi: {}", e)))?;
        phi.add_incoming(&[
            (&sv as &dyn inkwell::values::BasicValue, heap_bb),
            (&copied_sv as &dyn inkwell::values::BasicValue, copy_end_bb),
        ]);
        Ok(phi.as_basic_value())
    }

    /// L6: when a function returns a custom-enum-shaped value `{i32 tag, i64
    /// payload}`, claim the payload box pointer (field 1) so the callee's
    /// scope-exit free skips it — ownership transfers to the caller, which
    /// re-registers the box via `HeapEntry::EnumBox` (see
    /// `track_enum_box_return_lifetime`). Reuses the `claimed_returned_envs`
    /// pointer-compare guard (same mechanism as escaping closure envs, B9).
    ///
    /// Detection is by LLVM shape `{i32, i64}` — a harmless over-approximation:
    /// a record that happens to have that layout would claim a non-heap field
    /// value, which simply matches no registered heap slot (no-op). The precise
    /// custom-enum check happens at the caller registration (which has the
    /// callee's return-type AST), so records are never *freed* as enum boxes.
    /// Multi-target results share the shape but cannot be returned (flow-state
    /// linearity, E0421), so they never reach here.
    /// 0.35.20 (#6): transfer ownership of List data buffers that escape this
    /// function through the return value (returned directly as a List variable,
    /// or packed inside a returned tuple). Their `HeapEntry::Slot(data)`
    /// registrations are dropped so `flush_heap_scopes_to_boundary` skips them
    /// — the caller owns the buffers now. Without this, returning a List
    /// variable freed its data array before the caller could read it
    /// (use-after-free → garbage display / SIGSEGV). Detection is by AST shape
    /// (Ident of a List-typed var, or a tuple of such), covering the common
    /// `return xs` / `(yes, no)` patterns; unrecognized shapes keep the old
    /// scope-exit free.
    ///
    /// 0.35.24 (deep-eval): the walk intentionally stops at Call — the callee's
    /// arguments are INPUTS, not part of the returned value's ownership shape.
    /// Recursing into them (0.35.23) nulled local List variables that never
    /// escape (mutate-builtin tail calls return unit at the language level, so
    /// `return push(data, n)` cannot typecheck; for user functions the callee's
    /// own return path claims whatever it hands back). That nulled the slot and
    /// turned the scope-exit free into free(null): a per-call leak. Borrow
    /// params were guarded (K3), locals were not.
    pub(in crate::codegen) fn claim_returned_lists(
        &self,
        expr: Option<&Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) {
        match expr.map(|e| e.unlocated()) {
            Some(Expr::Ident(name)) => {
                // 0.35.23 deep-eval (mutate_list_push_allowed SIGSEGV): a
                // view/mutate borrow param's list storage IS the caller's
                // struct (pointer ABI). Nulling its data field here (the
                // implicit-return claim after `push(data, n)` recursed into
                // the call args) destroyed the CALLER's list — main's
                // `xs[2]` then loaded through a null data pointer.
                if self.borrow_param_names.contains(name.as_str()) {
                    return;
                }
                // List variables are stored as pointers to the {i64, ptr}
                // list struct (the struct itself is an unnamed local). Null
                // out the struct's data field: the scope-exit free loads the
                // slot and turns into free(null) — a no-op — while the
                // returned struct value (already loaded by the caller path)
                // keeps the live data pointer. Mirrors claim_string_return_value
                // (which nulls the string slot before heap cleanup).
                let is_list_var = self
                    .var_type_names
                    .get(name)
                    .map(|t| t == "List" || t.starts_with("List<"))
                    .unwrap_or(false);
                if is_list_var {
                    if let Some(&(alloca, ty)) = vars.get(name) {
                        let list_ty = self.list_struct_type();
                        let struct_ptr = match ty {
                            BasicTypeEnum::StructType(_) => alloca,
                            BasicTypeEnum::PointerType(_) => {
                                match self.build_load(
                                    self.context.ptr_type(inkwell::AddressSpace::default()),
                                    alloca,
                                    &format!("{}_ret_list", name),
                                ) {
                                    Ok(BasicValueEnum::PointerValue(p)) => p,
                                    _ => alloca,
                                }
                            }
                            _ => alloca,
                        };
                        if let Ok(data_gep) = self.gep().build_struct_gep(
                            list_ty,
                            struct_ptr,
                            1,
                            &format!("{}_ret_list_data", name),
                        ) {
                            let null_ptr = self
                                .context
                                .ptr_type(inkwell::AddressSpace::default())
                                .const_null();
                            let _ = self.build_store(data_gep, null_ptr);
                        }
                    }
                }
            }
            Some(Expr::Tuple(elems)) => {
                for e in elems {
                    self.claim_returned_lists(Some(e), vars);
                }
            }
            Some(Expr::Record { fields, .. }) => {
                for f in fields {
                    self.claim_returned_lists(Some(&f.value), vars);
                }
            }
            // 0.35.24: Call args are inputs — do not recurse (see fn doc).
            Some(Expr::Call(..)) => {}
            Some(Expr::Block(stmts)) => {
                if let Some(last) = stmts.last() {
                    if let Stmt::Expr(e) = last.unlocated() {
                        self.claim_returned_lists(Some(e), vars);
                    }
                }
            }
            _ => {}
        }
    }

    /// 0.35.20 (#6): deep-copy List *literals* that escape through a return.
    /// claim_returned_lists nulls the data slot of List *variables*, but a
    /// literal (`return [1, 2]` or a tuple like `([1, 2], [3, 4])`) has no
    /// named slot to null — its buffer is registered by build_list_struct and
    /// freed by the scope-exit flush, leaving the returned struct value
    /// dangling (garbage under O0; O1 happened to mask it). Copy the buffer
    /// here so the caller owns a fresh one while the literal's original is
    /// freed harmlessly. Recurses into tuple fields. llvm.memcpy with size 0
    /// permits null pointers (LangRef), so empty lists need no special case.
    pub(in crate::codegen) fn claim_returned_list_literals(
        &mut self,
        val: BasicValueEnum<'ctx>,
        expr: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match expr.map(|e| e.unlocated()) {
            Some(Expr::List(_)) => self.deep_copy_list_value(val),
            Some(Expr::Tuple(items)) => match val {
                BasicValueEnum::StructValue(sv) => {
                    let mut new_sv = sv;
                    for (i, item) in items.iter().enumerate() {
                        let fv = self.build_extract_value(new_sv.into(), i as u32, "ret_tup_f")?;
                        let nf = self.claim_returned_list_literals(fv, Some(item))?;
                        new_sv = self
                            .builder
                            .build_insert_value(new_sv, nf, i as u32, "ret_tup_nf")
                            .map_err(|e| CompileError::LlvmError(format!("tuple insert: {}", e)))?
                            .into_struct_value();
                    }
                    Ok(BasicValueEnum::StructValue(new_sv))
                }
                _ => Ok(val),
            },
            Some(Expr::Record { fields, .. }) => match val {
                BasicValueEnum::StructValue(sv) => {
                    let mut new_sv = sv;
                    for (i, f) in fields.iter().enumerate() {
                        let fv = self.build_extract_value(new_sv.into(), i as u32, "ret_rec_f")?;
                        let nf = self.claim_returned_list_literals(fv, Some(&f.value))?;
                        new_sv = self
                            .builder
                            .build_insert_value(new_sv, nf, i as u32, "ret_rec_nf")
                            .map_err(|e| CompileError::LlvmError(format!("record insert: {}", e)))?
                            .into_struct_value();
                    }
                    Ok(BasicValueEnum::StructValue(new_sv))
                }
                _ => Ok(val),
            },
            _ => Ok(val),
        }
    }

    /// Deep-copy a List struct value's data buffer ({i64 len, ptr data}).
    fn deep_copy_list_value(
        &mut self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let sv = match val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => {
                // build_list_struct returns an alloca pointer; load it.
                let list_ty = self.list_struct_type();
                self.build_load(BasicTypeEnum::StructType(list_ty), pv, "ret_list_ld")?
                    .into_struct_value()
            }
            _ => return Ok(val),
        };
        let len = self
            .build_extract_value(sv.into(), 0, "ret_list_len")?
            .into_int_value();
        let data = self
            .build_extract_value(sv.into(), 1, "ret_list_data")?
            .into_pointer_value();
        let i64_ty = self.context.i64_type();
        let bytes = self
            .builder
            .build_int_mul(len, i64_ty.const_int(8, false), "ret_list_bytes")
            .map_err(|e| CompileError::LlvmError(format!("mul: {}", e)))?;
        let new_data = self.malloc_or_abort(bytes, "ret_list_copy")?;
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        self.builder
            .build_call(
                memcpy_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(new_data),
                    BasicMetadataValueEnum::PointerValue(data),
                    BasicMetadataValueEnum::IntValue(bytes),
                ],
                "ret_list_memcpy",
            )
            .map_err(|e| CompileError::LlvmError(format!("memcpy: {}", e)))?;
        let list_ty = self.list_struct_type();
        let new_sv = self
            .builder
            .build_insert_value(list_ty.get_undef(), len, 0, "ret_list_len")
            .map_err(|e| CompileError::LlvmError(format!("insert len: {}", e)))?
            .into_struct_value();
        let new_sv = self
            .builder
            .build_insert_value(new_sv, new_data, 1, "ret_list_data")
            .map_err(|e| CompileError::LlvmError(format!("insert data: {}", e)))?
            .into_struct_value();
        Ok(BasicValueEnum::StructValue(new_sv))
    }

    pub(in crate::codegen) fn claim_returned_enum_box(
        &self,
        val: BasicValueEnum<'ctx>,
        ret_type: BasicTypeEnum<'ctx>,
    ) -> Result<(), CompileError> {
        let BasicTypeEnum::StructType(st) = ret_type else {
            return Ok(());
        };
        let fields = st.get_field_types();
        let is_enum_shape = matches!(
            fields.as_slice(),
            [
                BasicTypeEnum::IntType(tag),
                BasicTypeEnum::IntType(payload)
            ] if tag.get_bit_width() == 32 && payload.get_bit_width() == 64
        );
        if !is_enum_shape {
            return Ok(());
        }
        let BasicValueEnum::StructValue(sv) = val else {
            return Ok(());
        };
        let payload = self
            .builder
            .build_extract_value(sv, 1, "enum_ret_box_i64")
            .map_err(|e| CompileError::LlvmError(format!("enum ret box extract: {e}")))?;
        if let BasicValueEnum::IntValue(iv) = payload {
            let box_ptr = self.build_int_to_ptr(
                iv,
                self.context.ptr_type(AddressSpace::default()),
                "enum_ret_box",
            )?;
            // Reuse the closure-env claim set: a generic "skip this pointer's
            // free, the caller owns it now" mechanism (pointer-compare guard).
            self.claim_closure_env(box_ptr)?;
        }
        Ok(())
    }

    /// Heap-copy the data field of a Mimi `string` struct so the returned
    /// value is always backed by a freshly-allocated buffer. The caller (and
    /// only the caller) takes ownership. Non-string structs pass through.
    ///
    /// IMPORTANT: this does *not* register the freshly-allocated buffer on
    /// the callee's heap_allocs stack — that would cause `free_heap_allocs`
    /// to release the buffer before the return instruction completes. The
    /// caller is expected to register the resulting struct's data pointer
    /// (see `emit_function_call::track_string_return_lifetime`).
    pub(in crate::codegen) fn heap_copy_string_value(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (data_pv, len_iv) = match val {
            // A pointer-to-C-string (e.g. raw `i8*` from a literal, a string
            // variable holding a raw pointer, or a builtin raw-pointer
            // return). Compute the length via `strlen`, then build the
            // struct.
            BasicValueEnum::PointerValue(pv) => {
                let strlen_fn = self.get_runtime_fn("strlen")?;
                let length = self
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(pv)],
                        "ret_str_strlen",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
                    .into_int_value();
                (pv, length)
            }
            BasicValueEnum::StructValue(sv) => {
                let sty = sv.get_type();
                let fields = sty.get_field_types();
                let is_mimi_string_struct = matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(_)]
                );
                if !is_mimi_string_struct {
                    return Ok(sv.into());
                }
                let data_ptr = self.build_extract_value(sv.into(), 0, "ret_str_data")?;
                let data_pv = match data_ptr {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Ok(sv.into()),
                };
                let len_iv = match self.build_extract_value(sv.into(), 1, "ret_str_len")? {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Ok(sv.into()),
                };
                (data_pv, len_iv)
            }
            other => return Ok(other),
        };
        let i64_ty = self.context.i64_type();
        // len + 1 for the trailing nul so callers may use the result as a C
        // string.
        let alloc_len = self
            .builder
            .build_int_add(len_iv, i64_ty.const_int(1, false), "ret_str_alloc_len")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        // CG-H6 (audit): round `alloc_len` up to an 8-byte boundary so the
        // returned buffer is always 8-byte aligned. malloc() is allowed to
        // return any alignment, but downstream SIMD/word-size memcpy and
        // GEP operations assume 8-byte alignment on architectures such as
        // ARM/SPARC. Without this round-up we can produce UB on those
        // platforms.
        let seven = i64_ty.const_int(7, false);
        let rounded_minus_one = self
            .builder
            .build_int_add(alloc_len, seven, "ret_str_align_add")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let aligned_len = self
            .builder
            .build_and(
                rounded_minus_one,
                i64_ty.const_int(!7u64, false),
                "ret_str_align",
            )
            .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;
        // B4: NULL-checked malloc.
        let heap_ptr = self.malloc_or_abort(aligned_len, "ret_str_malloc")?;
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        self.build_call(
            memcpy_fn,
            &[
                BasicMetadataValueEnum::PointerValue(heap_ptr),
                BasicMetadataValueEnum::PointerValue(data_pv),
                BasicMetadataValueEnum::IntValue(len_iv),
            ],
            "ret_str_memcpy",
        )?;
        // Write nul terminator at heap_ptr[len].
        let i8_ty = self.context.i8_type();
        let nul_pos = self.build_in_bounds_gep(i8_ty, heap_ptr, &[len_iv], "ret_str_nul_pos")?;
        self.build_store(nul_pos, i8_ty.const_int(0, false))?;
        // Build the canonical {i8*, i64} struct.
        let sty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let new_sv = self
            .builder
            .build_insert_value(sty.get_undef(), heap_ptr, 0, "ret_str_new_data")
            .map_err(|e| CompileError::LlvmError(format!("insert str data: {}", e)))?
            .into_struct_value();
        let new_sv = self
            .builder
            .build_insert_value(new_sv, len_iv, 1, "ret_str_new_len")
            .map_err(|e| CompileError::LlvmError(format!("insert str len: {}", e)))?
            .into_struct_value();
        Ok(BasicValueEnum::StructValue(new_sv))
    }

    pub(in crate::codegen) fn normalize_block_last_string(
        &self,
        val: BasicValueEnum<'ctx>,
        block: &Block,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if let BasicValueEnum::PointerValue(_) = val {
            if let Some(last) = block.last() {
                if let Stmt::Expr(e) = last.unlocated() {
                    if self.expr_is_string(e) {
                        return self.wrap_raw_string_ptr(val.into_pointer_value());
                    }
                }
            }
        }
        Ok(val)
    }

    pub(super) fn coerce_variant_value(
        &self,
        val: BasicValueEnum<'ctx>,
        target_ty: BasicTypeEnum<'ctx>,
        ret_ty_ast: Option<&Type>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let ast_ty = match ret_ty_ast {
            Some(t) => t,
            None => return Ok(val),
        };
        let is_result = matches!(ast_ty.unlocated(), Type::Result(_, _))
            || matches!(ast_ty.unlocated(), Type::Name(n, args) if n == "Result" && args.len() == 2);
        let is_option = matches!(ast_ty.unlocated(), Type::Option(_))
            || matches!(ast_ty.unlocated(), Type::Name(n, args) if n == "Option" && args.len() == 1);
        if !is_result && !is_option {
            return Ok(val);
        }

        let target_st = match target_ty {
            BasicTypeEnum::StructType(st) => st,
            _ => return Ok(val),
        };

        // If the value is already a pointer (e.g. an alloca), try loading it as the
        // target type. When the pointer already points to the target layout this is
        // sufficient; generic allocas are handled by the StructValue path below.
        let sv = match val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => {
                let loaded = self.build_load(target_ty, pv, "coerce_load")?;
                match loaded {
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Ok(val),
                }
            }
            _ => return Ok(val),
        };

        let source_st = sv.get_type();
        if source_st == target_st {
            return Ok(val);
        }

        let source_fields = source_st.get_field_types();
        let target_fields = target_st.get_field_types();
        if source_fields.len() != target_fields.len() {
            return Ok(val);
        }

        let alloca = self.build_alloca(BasicTypeEnum::StructType(target_st), "variant_coerce")?;
        for (i, tf) in target_fields.iter().enumerate() {
            let gep = self
                .gep()
                .build_struct_gep(
                    BasicTypeEnum::StructType(target_st),
                    alloca,
                    i as u32,
                    "coerce_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("coerce gep: {}", e)))?;
            if i == 0 {
                let disc = self.build_extract_value(sv.into(), 0, "coerce_disc")?;
                self.build_store(gep, disc)?;
            } else if is_result && i == target_fields.len() - 1 {
                // CG-H6: extract Err field by target index (same layout index),
                // not source_fields.len()-1 which can mis-index when layouts differ
                // only in field types (same length was checked above).
                let err_idx = (target_fields.len() - 1) as u32;
                let err = self.build_extract_value(sv.into(), err_idx, "coerce_err")?;
                let err = self.coerce_field_to_type(err, *tf)?;
                self.build_store(gep, err)?;
            } else {
                let payload = self.build_extract_value(sv.into(), i as u32, "coerce_payload")?;
                let payload = self.coerce_field_to_type(payload, *tf)?;
                self.build_store(gep, payload)?;
            }
        }
        self.build_load(BasicTypeEnum::StructType(target_st), alloca, "coerced")
    }

    /// Helper used by `coerce_variant_value` to convert a single source field into
    /// the corresponding target field type.
    fn coerce_field_to_type(
        &self,
        val: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if val.get_type() == target {
            return Ok(val);
        }
        match (val, target) {
            // Wrap a raw C string pointer into the Mimi string struct {ptr, len}.
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st))
                if Self::is_mimi_string_struct(st) =>
            {
                self.wrap_c_string(pv)
            }
            // 0.35.8-fix (dx-backlog #20): pointer payload -> structured
            // target, load the pointee. `compile_ok_constructor` packs
            // {i1, ptr, i64} with the Ok payload ADDRESS in the ptr slot;
            // coercing to the declared Result<T,E> layout ({i1, T, E} with a
            // by-value T) previously stored the raw pointer into the T slot,
            // corrupting every struct payload (e.g. a Flow state with string
            // fields: `puts(0x1)` — the state's address bits read as the
            // first string's data pointer). The mimi-string wrap above keeps
            // raw C-string pointers intact; any other ptr-vs-struct mismatch
            // here is a boxed/alloca payload and must be dereferenced.
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => self
                .build_load(BasicTypeEnum::StructType(st), pv, "coerce_load_struct")
                .map_err(|e| CompileError::LlvmError(format!("coerce load: {}", e))),
            // Generic pad (i64 zero) -> structured payload: zero-initialize the target.
            (BasicValueEnum::IntValue(_), BasicTypeEnum::StructType(st)) => {
                Ok(BasicValueEnum::StructValue(st.const_zero()))
            }
            // Pointer -> integer (e.g. ptr err payload stored as i64).
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::IntType(it)) => {
                Ok(self.build_ptr_to_int(pv, it, "coerce_ptr_to_int")?.into())
            }
            // Integer width conversion.
            (BasicValueEnum::IntValue(_), BasicTypeEnum::IntType(_)) => {
                self.adjust_int_val(val, target)
            }
            _ => Ok(val),
        }
    }

    /// Returns true if `st` is the Mimi string struct `{ ptr, i64 }`.
    fn is_mimi_string_struct(st: inkwell::types::StructType<'ctx>) -> bool {
        let fields = st.get_field_types();
        matches!(
            fields.as_slice(),
            [
                BasicTypeEnum::PointerType(_),
                BasicTypeEnum::IntType(len)
            ] if len.get_bit_width() == 64
        )
    }

    /// Shared three-pass teardown of a partially-emitted function body
    /// (0.39.x stdlib matrix sweep, valgrind-pinned). Deleting blocks or
    /// instructions in appearance order corrupts the heap: LLVM destruction
    /// is use-before-def, and loop back-edges put defs (latch) after their
    /// users (header phis). Safe sequence:
    ///   pass 1 — erase every PHI and every terminator (kills all cross-block
    ///            edges in both directions);
    ///   pass 2 — erase remaining instructions in REVERSE order (SSA dominance
    ///            guarantees users die before defs);
    ///   pass 3 — delete the now-empty, now-unreferenced blocks.
    pub(crate) fn clear_partial_body(&self, function: inkwell::values::FunctionValue<'ctx>) {
        let blocks = function.get_basic_blocks();
        for bb in &blocks {
            while let Some(instruction) = bb.get_first_instruction() {
                if instruction.get_opcode() == inkwell::values::InstructionOpcode::Phi {
                    instruction.erase_from_basic_block();
                } else {
                    break;
                }
            }
            if let Some(terminator) = bb.get_terminator() {
                terminator.erase_from_basic_block();
            }
        }
        for bb in &blocks {
            while let Some(instruction) = bb.get_last_instruction() {
                instruction.erase_from_basic_block();
            }
        }
        for bb in blocks {
            // SAFETY: blocks are empty and unreferenced after the two erase
            // passes; delete() removes them from the function and their
            // addresses are not used afterwards.
            unsafe {
                let _ = bb.delete();
            }
        }
    }
}
