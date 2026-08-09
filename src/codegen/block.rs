#![allow(clippy::unwrap_used)]
use crate::ast::*;
use crate::codegen::call_try_basic_value;
use crate::codegen::types;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    /// 0.34.34: pierce `Type::Located` wrappers to reach the structural type
    /// name of an annotation. `let x: i32 = ...` parses the annotation as
    /// `Located { ty: Name("i32") }`, so a direct `Type::Name` match silently
    /// misses it (the exact bug that hid the SD-7 let-bind guard).
    pub(in crate::codegen) fn annotated_type_name(ty: &crate::ast::Type) -> Option<&str> {
        let mut t = ty.unlocated();
        while let crate::ast::Type::Located { ty: inner, .. } = t {
            t = inner.unlocated();
        }
        match t {
            crate::ast::Type::Name(n, _) => Some(n.as_str()),
            _ => None,
        }
    }

    pub(super) fn compile_block(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        // B9: remember the heap-scope depth before this block's own push.
        // A nested early return flushes scopes down to the function boundary
        // (including this block's scope), so the end-pop must be skipped when
        // the stack is already shallower than this block's push point.
        let heap_depth = self.heap_allocs.borrow().len();
        self.push_comp_scope();
        self.push_defer_scope();
        self.push_shared_scope();
        self.push_heap_scope();
        for stmt in block {
            // Run compensations before exit().
            //
            // 0.34.36 (cross-agent contract, audit §6/§9): `on failure`
            // handlers are registered at their EXECUTION POINT — the moment
            // the `Stmt::OnFailure` is compiled in statement order below — not
            // via a block pre-scan (the bytecode VM previously emitted a
            // block-entry SetFaultPc and is moving to a handler stack pushed
            // at statement execution). Because registration happens in the
            // same sequential pass, `compile_compensations` here can only see
            // handlers whose `on failure` statement precedes this `exit` in
            // source order — i.e. compensation fires only for faults AFTER the
            // handler statement executed. Do not pre-scan `block` for
            // OnFailure ahead of this loop.
            if let Stmt::Expr(expr) = stmt.unlocated() {
                if let Expr::Call(callee, _) = expr.unlocated() {
                    if let Expr::Ident(name) = callee.unlocated() {
                        if name == "exit" {
                            self.compile_compensations(vars)?;
                        }
                    }
                }
            }
            match stmt.unlocated() {
                Stmt::Expr(expr) => {
                    // 0.35.23 deep-eval: statement-position match arms have no
                    // value semantics (mimi-log `match content { Ok(d) => {
                    // lines = .. } Err(_) => { ok = false } }` — arm tails
                    // are heterogeneous assignments; forcing a phi rejected
                    // i1 vs ptr). Skip arm-value unification.
                    if let Expr::Match(scrutinee, arms) = expr.unlocated() {
                        self.compile_match_expr(scrutinee, arms, vars, true)?;
                    } else {
                        self.compile_expr(expr, vars)?;
                    }
                }
                Stmt::Return(Some(expr)) => {
                    let mut val = self.compile_expr(expr, vars)?;
                    // v0.34.16 (ADR-002): multi-target transition return —
                    // wrap the target state struct into the synthetic
                    // {i32 tag, i64 payload} union (payload = ptrtoint boxed
                    // state struct). Tag = the state's ordinal in
                    // multi_target_states (declared order).
                    if self.in_multi_target_transition {
                        let state_name = match expr.unlocated() {
                            Expr::Record { ty: Some(ty_name), .. } => ty_name.clone(),
                            Expr::Located { expr: inner, .. } => {
                                match inner.unlocated() {
                                    Expr::Record { ty: Some(ty_name), .. } => ty_name.clone(),
                                    _ => {
                                        return Err(CompileError::LlvmError(
                                            "multi-target transition return must construct a target state record (e.g. `return TargetState { ... }`)".to_string(),
                                        ))
                                    }
                                }
                            }
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "multi-target transition return must construct a target state record (e.g. `return TargetState { ... }`)".to_string(),
                                ))
                            }
                        };
                        // C1 fix: tag = flow-wide name-sorted ordinal (see
                        // register_flow_multi_target_enums), never the
                        // per-transition subset index — a subset-relative tag
                        // silently aliases another state of the union.
                        let tag = self
                            .multi_target_global_ordinals
                            .get(&self.current_flow_name)
                            .and_then(|m| m.get(&state_name))
                            .copied()
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "returned state '{state_name}' has no global multi-target ordinal (flow: {:?}, transition targets: {:?})",
                                    self.current_flow_name,
                                    self.multi_target_states
                                ))
                            })?;
                        let state_ty = self.flow_state_llvm_type(&state_name);
                        val = self.wrap_multi_target_value(val, tag, state_ty)?;
                    }
                    if self.in_fails_transition {
                        val = self.compile_ok_constructor(vec![val])?;
                    }
                    let ret_type = self
                        .current_fn_ret_type()
                        .unwrap_or_else(|| BasicTypeEnum::IntType(self.context.i64_type()));
                    val = self.adjust_int_val(val, ret_type)?;
                    // R-C8: claim heap-backed return ownership *before* free_heap_allocs,
                    // matching the func/actor/lambda return paths.
                    val = self.claim_string_return_value(val, ret_type, Some(expr), vars)?;
                    // L6: claim a returned custom-enum payload box so the callee's
                    // scope-exit free skips it (caller re-registers via EnumBox).
                    self.claim_returned_enum_box(val, ret_type)?;
                    // 0.34.24: same variant-layout coercion as the func.rs
                    // emit_return path — returning a differently-laid Result/
                    // Option variant (e.g. `return Err("…")` from
                    // `Result<f64, string>`) without it emits a ret of the
                    // wrong struct type (invalid IR → segfault at runtime).
                    val = self.coerce_variant_value(
                        val,
                        ret_type,
                        self.current_fn_ret_ty_ast.as_ref(),
                    )?;
                    // 0.34.41 第二档: ensures with a proper `result` binding
                    // (previously this path asserted with no result binding —
                    // "undefined variable 'result'"). Mirrors func.rs
                    // emit_return ordering: after coerce, before load/cleanup.
                    self.compile_ensures_asserts(Some(val), ret_type, vars)?;
                    val = self.load_return_value_if_needed(val)?;
                    self.emit_all_shared_releases()?;
                    self.discard_shared_scope();
                    self.flush_heap_scopes_to_boundary()?;
                    self.pop_defer_scope(vars)?;
                    self.pop_comp_scope();
                    self.build_return(Some(&val))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    let ret_type = self
                        .current_fn_ret_type()
                        .unwrap_or_else(|| BasicTypeEnum::IntType(self.context.i64_type()));
                    self.compile_ensures_asserts(None, ret_type, vars)?;
                    self.emit_all_shared_releases()?;
                    self.discard_shared_scope();
                    self.flush_heap_scopes_to_boundary()?;
                    self.pop_defer_scope(vars)?;
                    self.pop_comp_scope();
                    // 0.35.23 (deep-eval): bare `return` in a unit function —
                    // the unit signature is i64, so `ret void` (old) was
                    // invalid IR: O1 CalledValuePropagationPass SIGSEGV'd on
                    // "func f() { if true { return } }". Return the i64 zero.
                    let zero = self.zero_value_for(ret_type);
                    self.build_return(Some(&zero))?;
                    return Ok(());
                }
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ty,
                    ref_: ref_flag,
                    ..
                } => {
                    // dyn Trait let-binding: build fat pointer from concrete value (requires Variable pattern)
                    if let Some(Type::DynTrait(trait_names)) = ty.as_ref().map(Type::unlocated) {
                        let name = match &pat.kind {
                            PatternKind::Variable(n) => n.clone(),
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "dyn Trait binding requires a simple variable pattern"
                                        .to_string(),
                                ))
                            }
                        };
                        let concrete_val = self.compile_expr(init, vars)?;
                        let concrete_type = match init.unlocated() {
                            Expr::Record { ty: Some(tn), .. } => tn.clone(),
                            Expr::Ident(var_name) => self
                                .var_type_names
                                .get(var_name)
                                .cloned()
                                .unwrap_or_default(),
                            _ => {
                                return Err(CompileError::LlvmError(format!(
                                    "cannot infer concrete type for dyn Trait binding '{}'",
                                    name
                                )));
                            }
                        };
                        if concrete_type.is_empty() {
                            return Err(CompileError::LlvmError(format!(
                                "cannot infer concrete type for dyn Trait binding '{}'",
                                name
                            )));
                        }
                        let trait_name = &trait_names[0];
                        let concrete_ty = self
                            .type_llvm
                            .get(&concrete_type)
                            .cloned()
                            .unwrap_or_else(|| concrete_val.get_type());
                        let data_alloca =
                            self.build_alloca(concrete_ty, &format!("{}_data", name))?;
                        self.build_store(data_alloca, concrete_val)?;
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let data_ptr = self
                            .builder
                            .build_pointer_cast(data_alloca, i8_ptr, &format!("{}_data_i8", name))
                            .map_err(|e| {
                                CompileError::LlvmError(format!("pointer cast error: {}", e))
                            })?;
                        let vtable_key = format!("{}__{}", concrete_type, trait_name);
                        let vtable_gv = self.vtable_globals.get(&vtable_key).ok_or_else(|| {
                            CompileError::LlvmError(format!(
                                "no vtable for {}.{}",
                                concrete_type, trait_name
                            ))
                        })?;
                        let vtable_ptr = self
                            .builder
                            .build_pointer_cast(
                                vtable_gv.as_pointer_value(),
                                i8_ptr,
                                &format!("{}_vtable_i8", name),
                            )
                            .map_err(|e| {
                                CompileError::LlvmError(format!("pointer cast error: {}", e))
                            })?;
                        let fat_ty = BasicTypeEnum::StructType(self.context.struct_type(
                            &[
                                BasicTypeEnum::PointerType(i8_ptr),
                                BasicTypeEnum::PointerType(i8_ptr),
                            ],
                            false,
                        ));
                        let fat_alloca = self.build_alloca(fat_ty, &name)?;
                        let data_gep = self
                            .gep()
                            .build_struct_gep(fat_ty, fat_alloca, 0, &format!("{}_data_gep", name))
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.build_store(data_gep, data_ptr)?;
                        let vtable_gep = self
                            .gep()
                            .build_struct_gep(
                                fat_ty,
                                fat_alloca,
                                1,
                                &format!("{}_vtable_gep", name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.build_store(vtable_gep, vtable_ptr)?;
                        let ty_ref = ty.as_ref().ok_or_else(|| {
                            CompileError::LlvmError(format!("missing type for variable '{}'", name))
                        })?;
                        let dyn_type_str = crate::core::fmt_type(ty_ref);
                        self.var_type_names.insert(name.clone(), dyn_type_str);
                        vars.insert(name, (fat_alloca, fat_ty));
                        continue;
                    }
                    // Shared ref copy: let v = shared_var
                    if let PatternKind::Variable(name) = &pat.kind {
                        if let Expr::Ident(src_name) = init.unlocated() {
                            if self.shared_var_names.contains(src_name.as_str()) {
                                self.compile_shared_ref_copy(name, src_name, vars)?;
                                continue;
                            }
                        }
                    }
                    // Shared var clone: let v = shared_var.clone()
                    if let PatternKind::Variable(name) = &pat.kind {
                        if let Expr::Call(callee, cargs) = init.unlocated() {
                            if cargs.is_empty() {
                                if let Expr::Field(obj, method_name) = callee.unlocated() {
                                    if method_name == "clone" {
                                        if let Expr::Ident(src_name) = obj.unlocated() {
                                            if self.shared_var_names.contains(src_name.as_str()) {
                                                self.compile_shared_ref_copy(name, src_name, vars)?;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Non-dyn Trait: compile init and bind via recursive pattern matching
                    // Typed list literals: seed element type so Result/Option
                    // list elems pack with a uniform layout.
                    let saved_list_elem = self.pending_list_elem_type.take();
                    if matches!(init.unlocated(), Expr::List(_)) {
                        if let Some(decl_ty) = ty.as_ref() {
                            if let Type::Name(n, args) = decl_ty.unlocated() {
                                if n == "List" && args.len() == 1 {
                                    self.pending_list_elem_type = Some(args[0].clone());
                                }
                            }
                        }
                    }
                    let mut val = self.compile_expr(init, vars)?;
                    self.pending_list_elem_type = saved_list_elem;
                    if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        // SD-7 (0.34.34): a narrowing bind into an annotated i32
                        // slot must range-check BEFORE the silent truncate in
                        // adjust_int_val — out-of-range is E0802 overflow, not a
                        // wrap. Mirrors the VM CheckI32 let-guard. Explicit `as`
                        // casts keep wrap semantics; annotated binds trap.
                        if Self::annotated_type_name(decl_ty) == Some("i32") {
                            if let (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(it)) =
                                (val, target)
                            {
                                if iv.get_type().get_bit_width() > it.get_bit_width() {
                                    self.emit_i32_range_guard(iv, "let-bind")?;
                                }
                            }
                        }
                        val = self.adjust_int_val(val, target)?;
                        // CG-H4 (audit): for `let xs: List<T> = ...`, do NOT load the
                        // struct from the pointer — that creates a stack temporary copy
                        // whose mutations are lost. Keep the pointer so subsequent
                        // list operations update the original alloca.
                        // For non-List complex types we still need the load because
                        // compile_pattern_bind stores into a fresh alloca by value.
                        if let crate::ast::Type::Name(tn, _) = decl_ty.unlocated() {
                            if tn == "List" {
                                // Skip the load — val stays as PointerValue. The
                                // downstream compile_pattern_bind will store the
                                // pointer into the variable's alloca (PointerType
                                // matches the struct pointer).
                            }
                        }
                    }
                    // For simple Variable patterns, track type info
                    if let PatternKind::Variable(name) = &pat.kind {
                        if *ref_flag {
                            // 0.35.23 deep-eval: `let ref x = ...` stores the
                            // value in a plain slot (value layout T, surface
                            // type &T). Record it so `*x` derefs by identity.
                            self.ref_bound_vars.insert(name.clone());
                        }
                        if let Expr::Field(_, _) = init.unlocated() {
                            let field_ty = self.infer_object_type(init, vars);
                            if !field_ty.is_empty() {
                                self.var_type_names.insert(name.clone(), field_ty);
                            }
                        }
                        if let Some(decl_ty) = ty.as_ref() {
                            if let Some(full) = self.get_full_type_name(decl_ty) {
                                self.var_type_names.insert(name.clone(), full);
                            } else if let Type::Name(tn, args) = decl_ty.unlocated() {
                                if args.is_empty() {
                                    self.var_type_names.insert(name.clone(), tn.clone());
                                }
                            }
                        } else if self.expr_is_string(init) {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        } else if let Expr::Record { ty: None, .. } = init.unlocated() {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        } else if let Expr::Record {
                            ty: Some(tn),
                            fields,
                        } = init.unlocated()
                        {
                            self.var_type_names.insert(name.clone(), tn.clone());
                            // Infer concrete generic args from field values (e.g.
                            // `Pair { a: 10, b: 20 }` → `Pair<i32>`).
                            if let Some(td) = self.type_defs.get(tn) {
                                if !td.generics.is_empty() {
                                    let type_params: Vec<String> =
                                        td.generics.iter().map(|g| g.name.clone()).collect();
                                    let param_types: HashMap<String, Type> = self
                                        .try_infer_generic_from_fields(
                                            td,
                                            fields,
                                            vars,
                                            &type_params,
                                        );
                                    if param_types.len() == td.generics.len() {
                                        let args: Vec<Type> =
                                            td.generics
                                                .iter()
                                                .map(|g| {
                                                    param_types.get(&g.name).cloned().unwrap_or(
                                                        Type::Name(g.name.clone(), vec![]),
                                                    )
                                                })
                                                .collect();
                                        self.var_types
                                            .insert(name.clone(), Type::Name(tn.clone(), args));
                                    }
                                }
                            }
                        } else if let Expr::Ident(src_name) = init.unlocated() {
                            // 0.35.23 deep-eval (mimi-log main): `let y = x`
                            // must inherit the source variable's tracked
                            // type. `let mut display = filtered` above a
                            // `for e in display` lost the List<LogEntry>
                            // element type, so `e` bound as bare i64 and
                            // `e.timestamp` failed E0700 in the legacy
                            // emitter (the resolved/legacy split left main
                            // with the legacy for-loop path).
                            if let Some(src_ty) = self.var_type_names.get(src_name).cloned() {
                                self.var_type_names.insert(name.clone(), src_ty);
                            }
                            if let Some(src_ty) = self.var_types.get(src_name).cloned() {
                                self.var_types.insert(name.clone(), src_ty);
                            }
                        } else if matches!(init.unlocated(), Expr::SetLiteral(_)) {
                            self.var_type_names.insert(name.clone(), "set".to_string());
                        } else if let Expr::List(list_elems) = init.unlocated() {
                            // D1: infer List<T> type from first element
                            if let Some(first) = list_elems.first() {
                                let elem_type = self.infer_object_type(first, vars);
                                if !elem_type.is_empty() {
                                    let full = format!("List<{}>", elem_type);
                                    self.var_type_names.insert(name.clone(), full.clone());
                                    // Register List AST + struct elem LLVM (tuples/records).
                                    if let Some(parsed) =
                                        crate::codegen::extract_list_elem_type(&full)
                                    {
                                        let list_ast = Type::Name("List".into(), vec![parsed]);
                                        self.var_types.insert(name.clone(), list_ast.clone());
                                        self.register_list_elem_type(name, &list_ast);
                                    } else {
                                        let list_ast = Type::Name(
                                            "List".into(),
                                            vec![Type::Name(elem_type, vec![])],
                                        );
                                        self.var_types.insert(name.clone(), list_ast.clone());
                                        self.register_list_elem_type(name, &list_ast);
                                    }
                                }
                            }
                        } else if let Expr::Index(_, _) = init.unlocated() {
                            // D1: infer element type via infer_object_type (handles List<T> stripping)
                            let elem_type = self.infer_object_type(init, vars);
                            if !elem_type.is_empty() {
                                self.var_type_names.insert(name.clone(), elem_type);
                            }
                        } else if let Expr::SliceExpr { target, .. } = init.unlocated() {
                            // 0.34.36 (audit wave-2 #6): a slice `xs[a .. b]`
                            // keeps the target's element type (List<T> →
                            // List<T>). Without this registration,
                            // `let sub = xs[1 .. 3]` left `sub` untyped, so
                            // `println(sub)` fell into the puts fast path and
                            // printed the list struct pointer as a C string
                            // (garbage); scope-exit also double-freed the
                            // aliased buffer. Mirror the source list's type
                            // so println dispatches to the list formatter.
                            let target_type = self.infer_object_type(target, vars);
                            if target_type.starts_with("List") || target_type == "set" {
                                self.var_type_names
                                    .insert(name.clone(), target_type.clone());
                                if let Some(parsed) =
                                    crate::codegen::extract_list_elem_type(&target_type)
                                {
                                    let list_ast = Type::Name("List".into(), vec![parsed]);
                                    self.var_types.insert(name.clone(), list_ast.clone());
                                    self.register_list_elem_type(name, &list_ast);
                                }
                            }
                        } else if let Expr::Call(callee, call_args) = init.unlocated() {
                            if let Expr::Field(obj, method_name) = callee.unlocated() {
                                if method_name == "spawn" || method_name == "spawn_detached" {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if !obj_type.is_empty() {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(
                                    method_name.as_str(),
                                    "map" | "and_then" | "map_err" | "ok_or"
                                ) {
                                    // ok_or converts Option<T> → Result<T,E>;
                                    // map/and_then/map_err preserve the caller's variant type.
                                    if method_name == "ok_or" {
                                        self.var_type_names
                                            .insert(name.clone(), "Result".to_string());
                                    } else {
                                        let obj_type = self.infer_object_type(obj, vars);
                                        if obj_type.starts_with("Result") {
                                            self.var_type_names
                                                .insert(name.clone(), "Result".to_string());
                                        } else if obj_type.starts_with("Option") {
                                            self.var_type_names
                                                .insert(name.clone(), "Option".to_string());
                                        }
                                    }
                                } else if matches!(method_name.as_str(), "insert" | "remove") {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type.starts_with("Set") || obj_type == "set" {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    } else if let Expr::Ident(flow_name) = obj.unlocated() {
                                        // Flow::transition(from, ...) — insert/remove may be
                                        // flow transition names, not Set operations.
                                        if let Some(flow) = self.flow_defs.get(flow_name) {
                                            let from_type = call_args
                                                .first()
                                                .map(|a| self.infer_object_type(a, vars))
                                                .unwrap_or_default();
                                            let t = flow.transitions.iter().find(|t| {
                                                t.name == *method_name && t.from_state == from_type
                                            });
                                            if let Some(t) = t {
                                                let from_state = t.from_state.clone();
                                                let to_states = t.to_states.clone();
                                                let fails = t.fails.clone();
                                                if let Some(to) = to_states.first() {
                                                    self.var_type_names
                                                        .insert(name.clone(), to.clone());
                                                    self.track_flow_result_type(
                                                        name,
                                                        &from_state,
                                                        to,
                                                        fails,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                } else if method_name == "upgrade" {
                                    self.track_weak_upgrade_type(name, obj);
                                } else {
                                    // Generic method call: infer return type
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type == "string" {
                                        let ret_type =
                                            self.infer_string_method_return_type(method_name);
                                        if !ret_type.is_empty() {
                                            self.var_type_names.insert(name.clone(), ret_type);
                                        } else {
                                            // Q3 (rc-quality-gate-0.34.25a): trait-impl
                                            // methods on string receivers (e.g.
                                            // JsonExt::get_float) — register the
                                            // declared return type so downstream
                                            // display/dispatch sees Result<…>.
                                            let impl_ret = self.infer_impl_method_return_type(
                                                &obj_type,
                                                method_name,
                                            );
                                            if !impl_ret.is_empty() {
                                                self.var_type_names.insert(name.clone(), impl_ret);
                                            }
                                        }
                                    } else if let Expr::Ident(flow_name) = obj.unlocated() {
                                        // Flow::transition(from, ...) → matching overload's to-state
                                        if let Some(flow) = self.flow_defs.get(flow_name) {
                                            let from_type = call_args
                                                .first()
                                                .map(|a| self.infer_object_type(a, vars))
                                                .unwrap_or_default();
                                            let t = flow.transitions.iter().find(|t| {
                                                t.name == *method_name && t.from_state == from_type
                                            });
                                            if let Some(t) = t {
                                                let from_state = t.from_state.clone();
                                                let to_states = t.to_states.clone();
                                                let fails = t.fails.clone();
                                                if let Some(to) = to_states.first() {
                                                    self.var_type_names
                                                        .insert(name.clone(), to.clone());
                                                    self.track_flow_result_type(
                                                        name,
                                                        &from_state,
                                                        to,
                                                        fails,
                                                    );
                                                }
                                            }
                                        } else {
                                            // Q3: trait-impl method on a
                                            // non-string receiver.
                                            let impl_ret = self.infer_impl_method_return_type(
                                                &obj_type,
                                                method_name,
                                            );
                                            if !impl_ret.is_empty() {
                                                self.var_type_names.insert(name.clone(), impl_ret);
                                            }
                                        }
                                    } else {
                                        // Q3: trait-impl method on a
                                        // non-flow/non-string receiver.
                                        let impl_ret = self
                                            .infer_impl_method_return_type(&obj_type, method_name);
                                        if !impl_ret.is_empty() {
                                            self.var_type_names.insert(name.clone(), impl_ret);
                                        }
                                    }
                                }
                            } else if let Expr::Ident(func_name) = callee.unlocated() {
                                match func_name.as_str() {
                                    "Ok" | "Err" => {
                                        // Do not overwrite a full annotated type
                                        // like `Result<P,string>` with bare `Result`.
                                        if !self
                                            .var_type_names
                                            .get(name)
                                            .is_some_and(|t| t.starts_with("Result"))
                                        {
                                            self.var_type_names
                                                .insert(name.clone(), "Result".to_string());
                                        }
                                    }
                                    "Some" | "None" => {
                                        // Prefer annotated `Option<P>` / inferred full type.
                                        if !self
                                            .var_type_names
                                            .get(name)
                                            .is_some_and(|t| t.starts_with("Option"))
                                        {
                                            let inferred = self.infer_object_type(init, vars);
                                            if inferred.starts_with("Option<") {
                                                self.var_type_names.insert(name.clone(), inferred);
                                            } else {
                                                self.var_type_names
                                                    .insert(name.clone(), "Option".to_string());
                                            }
                                        }
                                    }
                                    _ => {
                                        // Known builtins that return Result<string,string>
                                        if matches!(
                                            func_name.as_str(),
                                            "read_file"
                                                | "read_file_partial"
                                                | "read_file_bytes"
                                                | "getenv"
                                                | "base64_decode"
                                                | "mimi_lexer_tokenize"
                                                | "mimi_parse_source"
                                        ) {
                                            self.var_type_names.insert(
                                                name.clone(),
                                                "Result<string,string>".to_string(),
                                            );
                                        } else if matches!(
                                            func_name.as_str(),
                                            "write_file" | "write_file_bytes"
                                        ) {
                                            self.var_type_names.insert(
                                                name.clone(),
                                                "Result<(), string>".to_string(),
                                            );
                                        } else if let Some((type_name, _)) =
                                            self.find_variant_owner(func_name)
                                        {
                                            self.var_type_names.insert(name.clone(), type_name);
                                        } else if crate::codegen::builtins::is_builtin(func_name) {
                                            let obj_type = self.infer_object_type(init, vars);
                                            if !obj_type.is_empty()
                                                && obj_type.as_str() != func_name.as_str()
                                            {
                                                self.var_type_names.insert(name.clone(), obj_type);
                                            }
                                            // map_set / map_remove return Map; prefer value-type
                                            // from third arg when present (e.g. product-tuple).
                                            match func_name.as_str() {
                                                "map_new" => {
                                                    self.var_type_names
                                                        .insert(name.clone(), "Map".to_string());
                                                }
                                                "map_set" | "map_remove" => {
                                                    if let Expr::Call(_, args) = init.unlocated() {
                                                        if let Some(val_arg) = args.get(2) {
                                                            let vt = self
                                                                .infer_object_type(val_arg, vars);
                                                            if vt.starts_with('(')
                                                                || self.is_product_tuple_alias(&vt)
                                                            {
                                                                let resolved = if self
                                                                    .is_product_tuple_alias(&vt)
                                                                {
                                                                    self.resolve_alias_type_name(
                                                                        &vt,
                                                                    )
                                                                } else {
                                                                    vt
                                                                };
                                                                self.var_type_names.insert(
                                                                    name.clone(),
                                                                    format!(
                                                                        "Map<string, {}>",
                                                                        resolved
                                                                    ),
                                                                );
                                                            } else if !vt.is_empty()
                                                                && vt != "i64"
                                                                && vt != "int"
                                                            {
                                                                self.var_type_names.insert(
                                                                    name.clone(),
                                                                    format!("Map<string, {}>", vt),
                                                                );
                                                            } else {
                                                                self.var_type_names.insert(
                                                                    name.clone(),
                                                                    "Map".to_string(),
                                                                );
                                                            }
                                                        } else {
                                                            self.var_type_names.insert(
                                                                name.clone(),
                                                                "Map".to_string(),
                                                            );
                                                        }
                                                    } else {
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            "Map".to_string(),
                                                        );
                                                    }
                                                }
                                                "set_new" | "set_insert" | "set_remove" => {
                                                    self.var_type_names
                                                        .insert(name.clone(), "Set".to_string());
                                                }
                                                _ => {}
                                            }
                                        } else if let Some((ret_ty, is_async)) = self
                                            .func_defs
                                            .get(func_name)
                                            .map(|fdef| (fdef.ret.clone(), fdef.is_async))
                                        {
                                            if let Some(ret_ty) = ret_ty {
                                                match ret_ty.unlocated() {
                                                    Type::ImplTrait(traits) => {
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            format!("impl {}", traits.join(" + ")),
                                                        );
                                                    }
                                                    Type::Name(tn, _) => {
                                                        // Resolve generic type params (e.g. T→User) from the
                                                        // calling context's type_map before computing the full name.
                                                        let resolved =
                                                            self.substitute_type_params(&ret_ty);
                                                        let type_name = if let Some(full) =
                                                            self.get_full_type_name(&resolved)
                                                        {
                                                            full
                                                        } else {
                                                            tn.clone()
                                                        };
                                                        self.var_type_names
                                                            .insert(name.clone(), type_name);
                                                        // Register list element LLVM type for list-typed results
                                                        // so index access can reconstruct struct-typed elements.
                                                        self.register_list_elem_type(
                                                            name, &resolved,
                                                        );
                                                    }
                                                    // Newtype constructors: use the newtype name instead of
                                                    // the transparent inner type so method dispatch works.
                                                    Type::Newtype(n, _) => {
                                                        self.var_type_names
                                                            .insert(name.clone(), n.clone());
                                                    }
                                                    // Remaining variants: no var_type_names
                                                    // tracking needed for these return types.
                                                    Type::Located { .. }
                                                    | Type::Ref(..)
                                                    | Type::RefMut(..)
                                                    | Type::Option(..)
                                                    | Type::Result(..)
                                                    | Type::Tuple(..)
                                                    | Type::Func(..)
                                                    | Type::ExternFunc(..)
                                                    | Type::CBuffer(..)
                                                    | Type::Cap(..)
                                                    | Type::CapAtom(..)
                                                    | Type::Shared(..)
                                                    | Type::LocalShared(..)
                                                    | Type::Weak(..)
                                                    | Type::WeakLocal(..)
                                                    | Type::Nothing
                                                    | Type::Allocator
                                                    | Type::Array(..)
                                                    | Type::Slice(..)
                                                    | Type::DynTrait(..)
                                                    | Type::RawPtr(..)
                                                    | Type::RawPtrMut(..)
                                                    | Type::CShared(..)
                                                    | Type::CBorrow(..)
                                                    | Type::CBorrowMut(..)
                                                    | Type::RawString
                                                    | Type::Infer
                                                    | Type::TyErr
                                                    | Type::TypeVar(..)
                                                    | Type::ForAll(..) => {}
                                                }
                                                // For async functions, track the inner result type for await.
                                                if is_async {
                                                    if let Some(llvm_ret) =
                                                        self.llvm_type_for(&ret_ty)
                                                    {
                                                        self.async_var_inner_types
                                                            .insert(name.clone(), llvm_ret);
                                                    }
                                                }
                                            }
                                        } else if let Some(crate::ast::Type::Name(tn, _)) = self
                                            .extern_func_defs
                                            .get(func_name)
                                            .and_then(|ef| ef.ret.as_ref())
                                            .map(crate::ast::Type::unlocated)
                                        {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                        }
                                        // 0.35.11-fix: nested-block counterpart
                                        // of the func.rs list-builtin tracker
                                        // (map/filter/reverse/sort/range).
                                        if let Some(list_ty) = self.infer_list_builtin_return_type(
                                            func_name, call_args, vars,
                                        ) {
                                            self.var_type_names.insert(name.clone(), list_ty);
                                        }
                                        // Track return types for builtins
                                        match func_name.as_str() {
                                            "listdir" | "walk_dir" | "str_split" | "keys" => {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    "List<string>".to_string(),
                                                );
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "List".into(),
                                                        vec![Type::Name("string".into(), vec![])],
                                                    ),
                                                );
                                            }
                                            "sort_str" => {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    "List<string>".to_string(),
                                                );
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "List".into(),
                                                        vec![Type::Name("string".into(), vec![])],
                                                    ),
                                                );
                                            }
                                            "sort_f64" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "List<f64>".to_string());
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "List".into(),
                                                        vec![Type::Name("f64".into(), vec![])],
                                                    ),
                                                );
                                            }
                                            "exec" | "exec_safe" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "ExecResult".to_string());
                                            }
                                            "file_stat" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "StatResult".to_string());
                                            }
                                            "append_file" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "bool".to_string());
                                            }
                                            "set_env" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "bool".to_string());
                                            }
                                            "getenv" | "base64_decode" => {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    "Result<string,string>".to_string(),
                                                );
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "Result".into(),
                                                        vec![
                                                            Type::Name("string".into(), vec![]),
                                                            Type::Name("string".into(), vec![]),
                                                        ],
                                                    ),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            } else if let Expr::Turbofish(_func_name, turbo_type_args, _) =
                                init.unlocated()
                            {
                                if let Some(ta) = turbo_type_args.first() {
                                    if let Type::Name(tn, args) = ta.unlocated() {
                                        if tn == "List" && !args.is_empty() {
                                            if let Some(full) = self.get_full_type_name(ta) {
                                                self.var_type_names.insert(name.clone(), full);
                                            }
                                            self.var_types.insert(name.clone(), ta.clone());
                                            self.register_list_elem_type(name, ta);
                                        } else if (tn == "Map" || tn == "Set") && !args.is_empty() {
                                            if let Some(full) = self.get_full_type_name(ta) {
                                                self.var_type_names.insert(name.clone(), full);
                                            } else {
                                                self.var_type_names
                                                    .insert(name.clone(), tn.clone());
                                            }
                                            self.var_types.insert(name.clone(), ta.clone());
                                        } else {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                            self.var_types.insert(name.clone(), ta.clone());
                                        }
                                    }
                                }
                            }
                        }
                        // 0.35.23 deep-eval (mimi-make E0707): `let r0 =
                        // if n > 0 { parse_one(..) } else { Rule { .. } }`
                        // — the chain above has no Expr::If / Expr::Block
                        // arm, so `r0` stayed unregistered in
                        // var_type_names and `r0.target` failed E0707.
                        // Backfill from infer_object_type (which walks the
                        // branch tails) when nothing else registered a type.
                        if !self.var_type_names.contains_key(name)
                            && matches!(init.unlocated(), Expr::If { .. } | Expr::Block(_))
                        {
                            let inferred = self.infer_object_type(init, vars);
                            if !inferred.is_empty() {
                                self.var_type_names.insert(name.clone(), inferred);
                            }
                        }
                    }
                    // Track list element type for nested List<List<T>> indexing
                    if let PatternKind::Variable(name) = &pat.kind {
                        if let Some(decl_ty) = &ty {
                            self.register_list_elem_type(name, decl_ty);
                            self.var_types.insert(name.clone(), decl_ty.clone());
                        }
                        // Track standalone turbofish type (e.g. from_json::<List<f64>>("..."))
                        if let Expr::Turbofish(_func_name, turbo_type_args, _) = init.unlocated() {
                            if let Some(ta) = turbo_type_args.first() {
                                if let Type::Name(tn, args) = ta.unlocated() {
                                    if tn == "List" && !args.is_empty() {
                                        if let Some(full) = self.get_full_type_name(ta) {
                                            self.var_type_names.insert(name.clone(), full);
                                        }
                                        self.var_types.insert(name.clone(), ta.clone());
                                        self.register_list_elem_type(name, ta);
                                    } else if (tn == "Map" || tn == "Set") && !args.is_empty() {
                                        if let Some(full) = self.get_full_type_name(ta) {
                                            self.var_type_names.insert(name.clone(), full);
                                        } else {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                        }
                                        self.var_types.insert(name.clone(), ta.clone());
                                    } else {
                                        self.var_type_names.insert(name.clone(), tn.clone());
                                        self.var_types.insert(name.clone(), ta.clone());
                                    }
                                }
                            }
                        }
                    }
                    // 0.35.23 deep-eval: `let y = x` inherits the source
                    // variable's tracked type — the compile_block_last_val
                    // counterpart of the compile_block fix above (mimi-log
                    // main's `let mut display = filtered` inside an if/else
                    // branch compiles through THIS path; without it
                    // `for e in display` bound `e` as bare i64 and field
                    // access failed E0700).
                    if let (PatternKind::Variable(name), Expr::Ident(src_name)) =
                        (&pat.kind, init.unlocated())
                    {
                        if !self.var_type_names.contains_key(name.as_str()) {
                            if let Some(src_ty) = self.var_type_names.get(src_name).cloned() {
                                self.var_type_names.insert(name.clone(), src_ty);
                            }
                        }
                        if !self.var_types.contains_key(name.as_str()) {
                            if let Some(src_ty) = self.var_types.get(src_name).cloned() {
                                self.var_types.insert(name.clone(), src_ty);
                            }
                        }
                    }
                    self.compile_pattern_bind(pat, val, vars)?;
                    if let PatternKind::Tuple(sub_pats) = &pat.kind {
                        if let Expr::Call(callee, _) = init.unlocated() {
                            if let Expr::Ident(func_name) = callee.unlocated() {
                                if func_name == "map_get" && sub_pats.len() == 2 {
                                    if let PatternKind::Variable(name) = &sub_pats[1].kind {
                                        self.var_type_names.insert(name.clone(), "any".to_string());
                                    }
                                }
                            }
                        }
                    }
                    if let PatternKind::Variable(name) = &pat.kind {
                        if let Expr::Ident(fn_name) = init.unlocated() {
                            if self.module.get_function(fn_name).is_some() {
                                self.fn_ptr_var_names.insert(name.clone());
                            }
                            if self.cap_type_names.contains(fn_name.as_str()) {
                                self.var_type_names.insert(name.clone(), fn_name.clone());
                            }
                        }
                        // 0.35.14 (DX backlog #18): tuple fn-element extraction.
                        self.record_tuple_fn_elems(name, init);
                        self.register_tuple_index_fn_binding(name, init);
                    }
                }
                Stmt::Let {
                    pat,
                    init: None,
                    ty,
                    ..
                } => {
                    // let x; or let (a, b); — needs type annotation
                    if let PatternKind::Variable(name) = &pat.kind {
                        let llvm_ty = match ty {
                            Some(decl_ty) => types::mimi_type_to_llvm(self.context, decl_ty)
                                .ok_or_else(|| {
                                    CompileError::LlvmError(format!(
                                        "unknown type for 'let {};'",
                                        name
                                    ))
                                })?,
                            None => {
                                return Err(CompileError::LlvmError(format!(
                                    "'let {};' requires an explicit type annotation",
                                    name
                                )))
                            }
                        };
                        let alloca = self.build_alloca(llvm_ty, name)?;
                        // Zero-initialize the alloca so that `let x;` without an
                        // initializer does not leave LLVM undef (UB on first read).
                        // StructType uses const_zero (recursive zero-init of all fields).
                        // ArrayType uses get_undef (LLVM does not guarantee zero-init
                        // of array elements, but no struct-like holes exist).
                        match llvm_ty {
                            BasicTypeEnum::IntType(ty) => {
                                self.build_store(alloca, ty.const_int(0, false))?;
                            }
                            BasicTypeEnum::FloatType(ty) => {
                                self.build_store(alloca, ty.const_float(0.0))?;
                            }
                            BasicTypeEnum::PointerType(ty) => {
                                self.build_store(alloca, ty.const_null())?;
                            }
                            BasicTypeEnum::StructType(ty) => {
                                self.build_store(alloca, ty.const_zero())?;
                            }
                            BasicTypeEnum::ArrayType(ty) => {
                                self.build_store(alloca, ty.get_undef())?;
                            }
                            _ => {}
                        }
                        vars.insert(name.clone(), (alloca, llvm_ty));
                    } else {
                        return Err(CompileError::LlvmError(
                            "'let' with no initializer requires a simple variable pattern"
                                .to_string(),
                        ));
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    self.compile_if_stmt(cond, then_, else_, vars, true)?;
                }
                Stmt::IfLet {
                    pat,
                    init,
                    then_,
                    else_,
                } => {
                    // C2 (audit-syntax): desugar to match (see compile_if_let_stmt).
                    self.compile_if_let_stmt(pat, init, then_, else_, vars)?;
                }
                Stmt::Break(_) => {
                    self.compile_break_stmt()?;
                }
                Stmt::Continue => {
                    self.compile_continue_stmt()?;
                }
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and discard result (for linear capabilities)
                    // H4 (audit 2026-08-03): also release the runtime cap handle.
                    // mimi_cap_register only ever appended to CAP_TABLE; without
                    // this, every `drop(cap)` in a loop leaked an entry while the
                    // bytecode VM (pure Value::Cap, no registry) stayed flat.
                    if let Expr::Ident(name) = expr.unlocated() {
                        if self.is_cap_var(name) {
                            if let Some(drop_fn) = self.module.get_function("mimi_cap_drop") {
                                if let Some((alloca, _)) = vars.get(name) {
                                    let handle = self
                                        .build_load(
                                            self.context.i64_type(),
                                            *alloca,
                                            "cap_drop_handle",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "cap drop load error: {}",
                                                e
                                            ))
                                        })?;
                                    let _ = self.builder.build_call(
                                        drop_fn,
                                        &[handle.into()],
                                        "cap_drop",
                                    );
                                }
                            }
                            self.consume_cap(name)?;
                        }
                    }
                    self.compile_expr(expr, vars)?;
                }
                Stmt::Defer(block) => {
                    // 0.31.24: Register defer block for LIFO execution on scope exit
                    self.register_defer(block);
                }
                Stmt::SharedLet {
                    kind,
                    name,
                    ty,
                    init,
                } => {
                    self.compile_shared_let_stmt(kind, name, ty, init, vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit.
                    //
                    // 0.34.36 (cross-agent contract, audit §6/§9): registration
                    // happens HERE — at the `on failure` statement's execution
                    // point in the sequential pass — never via a block pre-scan.
                    // Combined with the per-statement exit() hook above, this
                    // guarantees compensation fires only for faults that occur
                    // AFTER this statement executed (matching the VM's new
                    // handler-stack model). Keep this inline; do not hoist it
                    // into a pre-pass over `block`.
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    self.compile_arena_block(block, vars, "arena")?;
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, vars)?;
                }
                Stmt::IeeeFloat(block) => {
                    // v0.34.10a (SD-9): suspend finiteness trap inside.
                    self.ieee_depth += 1;
                    let r = self.compile_block(block, vars);
                    self.ieee_depth -= 1;
                    r?;
                }
                Stmt::Alloc {
                    kind: AllocKind::Arena,
                    body,
                } => {
                    self.compile_arena_block(body, vars, "alloc(Arena)")?;
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified)
                    self.compile_block(body, vars)?;
                }
                Stmt::Requires(..)
                | Stmt::Ensures(..)
                | Stmt::Invariant(..)
                | Stmt::Math(_)
                | Stmt::Ellipsis => {
                    // Skip contract-related statements in codegen
                }
                Stmt::Block(block) => {
                    self.compile_block(block, vars)?;
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, vars)?;
                }
                Stmt::WhileLet { pat, init, body } => {
                    self.compile_while_let_stmt(pat, init, body, vars)?;
                }
                Stmt::Loop(body) => {
                    self.compile_loop_stmt(body, vars)?;
                }
                Stmt::For {
                    var,
                    iterable,
                    body,
                } => {
                    self.compile_for_stmt(var, iterable, body, vars)?;
                }
                Stmt::Pinned { expr, var, body } => {
                    // v0.34.3: synchronous pinned timeout abolished (clause 10);
                    // only the pin + body remain.
                    // Evaluate pinned expression, bind optional |var|, run body.
                    // (Implicit Active→FFI_Pinned→Active: body is the pinned region.)
                    let val = self.compile_expr(expr, vars)?;
                    if let Some(v) = var {
                        let ty = val.get_type();
                        let alloca = self.build_alloca(ty, v)?;
                        self.build_store(alloca, val)?;
                        vars.insert(v.clone(), (alloca, ty));
                    }
                    self.compile_block(body, vars)?;
                }
                // Located is stripped by unlocated(); Func is handled at
                // declaration level, not as a block statement in codegen.
                Stmt::Located { .. } | Stmt::Func(_) => {}
            }
        }
        self.pop_shared_scope()?;
        if self.heap_allocs.borrow().len() > heap_depth {
            self.free_heap_allocs()?;
        }
        // 0.31.24: Defer blocks always run (LIFO), regardless of exit path
        self.pop_defer_scope(vars)?;
        self.pop_comp_scope();
        Ok(())
    }

    /// Compile a `break` statement by branching to the current loop break target.
    fn compile_break_stmt(&mut self) -> Result<(), CompileError> {
        if let Some(target) = self.loop_break {
            self.build_br(target)?;
            let function = self.current_function().ok_or_else(|| {
                CompileError::LlvmError("codegen: no current function for break".to_string())
            })?;
            let unreachable = self.context.append_basic_block(function, "unreachable");
            self.builder.position_at_end(unreachable);
            Ok(())
        } else {
            Err(CompileError::BreakOutsideLoop)
        }
    }

    /// Compile a `continue` statement by branching to the current loop continue target.
    fn compile_continue_stmt(&mut self) -> Result<(), CompileError> {
        if let Some(target) = self.loop_continue {
            self.build_br(target)?;
            let function = self.current_function().ok_or_else(|| {
                CompileError::LlvmError("codegen: no current function for continue".to_string())
            })?;
            let unreachable = self.context.append_basic_block(function, "unreachable");
            self.builder.position_at_end(unreachable);
            Ok(())
        } else {
            Err(CompileError::ContinueOutsideLoop)
        }
    }

    /// Compile an `if` statement or if-expression.
    ///
    /// When `merge_vars` is `true`, variables introduced in either branch are merged
    /// back into `vars` (used for statement-position `if`). When `false`, the value
    /// of the branches is merged with a phi node and returned (used for
    /// `compile_block_last_val`).
    pub(in crate::codegen) fn compile_if_stmt(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: &Option<Block>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
        merge_vars: bool,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let cond_val = self.compile_expr(cond, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            // Builtin predicates return i64 booleans; normalize to i1 so the
            // branch instruction is well-typed (br i64 crashes instruction
            // selection and is invalid IR per the verifier). H-22: the zero
            // constant uses the condition's own width (icmp operands must
            // match; a hard-coded i64 zero against a narrower int is invalid
            // IR for the same reason).
            if iv.get_type().get_bit_width() == 1 {
                iv
            } else {
                self.builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        iv,
                        iv.get_type().const_int(0, false),
                        "cond_bool",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cond normalize: {}", e)))?
            }
        } else {
            let function = self.current_function().ok_or_else(|| {
                CompileError::LlvmError("codegen: no current function for if block".to_string())
            })?;
            let fn_name = function.get_name().to_str().unwrap_or("unknown");
            return Err(CompileError::TypeMismatch(format!(
                "if condition must be bool, got {} in function '{}'",
                cond_val.get_type(),
                fn_name
            )));
        };

        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("codegen: no current function for if block".to_string())
        })?;
        let (then_label, else_label, merge_label) = if merge_vars {
            ("then", "else", "ifcont")
        } else {
            ("blt_then", "blt_else", "blt_merge")
        };
        let then_bb = self.context.append_basic_block(function, then_label);
        let else_bb = self.context.append_basic_block(function, else_label);
        let merge_bb = self.context.append_basic_block(function, merge_label);

        self.build_cond_br(cond_bool, then_bb, else_bb)?;

        // Then branch
        self.builder.position_at_end(then_bb);
        let mut then_vars = vars.clone();
        let then_val = if merge_vars {
            self.compile_block(then_, &mut then_vars)?;
            None
        } else {
            Some(self.compile_block_last_val(then_, &mut then_vars)?)
        };
        let then_reaches = !self.block_has_terminator();
        if then_reaches {
            self.build_br(merge_bb)?;
        }
        let then_bb_end = self.builder.get_insert_block().ok_or_else(|| {
            CompileError::LlvmError("codegen: no insert block after then branch".to_string())
        })?;

        // Else branch
        self.builder.position_at_end(else_bb);
        let mut else_vars = vars.clone();
        let else_val = if let Some(else_block) = else_ {
            if merge_vars {
                self.compile_block(else_block, &mut else_vars)?;
                None
            } else {
                Some(self.compile_block_last_val(else_block, &mut else_vars)?)
            }
        } else if merge_vars {
            None
        } else {
            // No else block: fall through to merge with a default value.
            Some(self.context.i64_type().const_int(0, false).into())
        };
        let else_reaches = !self.block_has_terminator();
        if else_reaches {
            self.build_br(merge_bb)?;
        }
        let else_bb_end = self.builder.get_insert_block().ok_or_else(|| {
            CompileError::LlvmError("codegen: no insert block after else branch".to_string())
        })?;

        // Merge branch-local variables back into the outer scope when compiling a statement.
        if merge_vars {
            // Remove outer variables shadowed by either branch, then insert
            // branch-local bindings. For keys defined in both branches,
            // then_vars takes priority (or_insert).
            for k in then_vars.keys() {
                vars.remove(k);
            }
            vars.extend(then_vars);
            if else_.is_some() {
                for (k, v) in else_vars {
                    vars.entry(k).or_insert(v);
                }
            }
            self.builder.position_at_end(merge_bb);
            return Ok(None);
        }

        // Value mode: build a phi of the values produced by each reaching branch.
        // Unify integer widths: after A1 restoration, then and else branches
        // may produce different-width integers (e.g. i64 literal vs i32 expr).
        // Extend the narrower value IN ITS PREDECESSOR BLOCK (before the br)
        // so the phi has a consistent type without SSA dominance violations.
        let then_bw = match &then_val {
            Some(BasicValueEnum::IntValue(iv)) => iv.get_type().get_bit_width(),
            _ => 0,
        };
        let else_bw = match &else_val {
            Some(BasicValueEnum::IntValue(iv)) => iv.get_type().get_bit_width(),
            _ => 0,
        };
        let (then_val, else_val) = if then_bw > 0 && else_bw > 0 && then_bw != else_bw {
            // 0.34.36 (audit §6.8): the unified target width is the MAX of the
            // two branch widths, and BOTH branches are extended to it. The old
            // code chose an arbitrary i64/i32 target from the sign of the width
            // comparison and only extended the branch the comparison happened to
            // select, so e.g. `then: i8 / else: i16` picked target i32, left the
            // i16 else value un-extended, and then discarded it (phi_type came
            // from the then value, the mismatched else fell into the zero-fill
            // below) — silently wrong results plus width-inconsistent phis.
            let target_bw = then_bw.max(else_bw);
            // target_bw > 0 here (both branch widths are > 0 by the guard
            // above), so the NonZeroU32 construction cannot fail.
            let target_ty = self
                .context
                .custom_width_int_type(std::num::NonZeroU32::new(target_bw as u32).unwrap())
                .map_err(|e| CompileError::LlvmError(format!("if target width: {}", e)))?;
            // Extend then_val to the target width inside its own predecessor
            // block (before the terminator) so the phi stays type-uniform
            // without SSA dominance violations. i1 (bool) uses z_extend so
            // `true` widens to 1, not -1 (A1 convention).
            let then_val = if then_bw < target_bw && then_reaches {
                self.builder.position_at_end(then_bb_end);
                if let Some(term) = then_bb_end.get_terminator() {
                    self.builder.position_before(&term);
                }
                let tv = then_val
                    .ok_or_else(|| {
                        CompileError::LlvmError("if-then ext: missing then value".into())
                    })?
                    .into_int_value();
                let widened = if tv.get_type().get_bit_width() == 1 {
                    self.builder
                        .build_int_z_extend(tv, target_ty, "if_then_zext")
                        .map_err(|e| CompileError::LlvmError(format!("if then z_ext: {}", e)))?
                } else {
                    self.builder
                        .build_int_s_extend(tv, target_ty, "if_then_sext")
                        .map_err(|e| CompileError::LlvmError(format!("if then s_ext: {}", e)))?
                };
                BasicValueEnum::IntValue(widened)
            } else {
                then_val
                    .ok_or_else(|| CompileError::LlvmError("if-then: missing then value".into()))?
            };
            // Extend else_val to the target width, same reasoning.
            let else_val = if else_bw < target_bw && else_reaches {
                self.builder.position_at_end(else_bb_end);
                if let Some(term) = else_bb_end.get_terminator() {
                    self.builder.position_before(&term);
                }
                let ev = else_val
                    .ok_or_else(|| {
                        CompileError::LlvmError("if-else ext: missing else value".into())
                    })?
                    .into_int_value();
                let widened = if ev.get_type().get_bit_width() == 1 {
                    self.builder
                        .build_int_z_extend(ev, target_ty, "if_else_zext")
                        .map_err(|e| CompileError::LlvmError(format!("if else z_ext: {}", e)))?
                } else {
                    self.builder
                        .build_int_s_extend(ev, target_ty, "if_else_sext")
                        .map_err(|e| CompileError::LlvmError(format!("if else s_ext: {}", e)))?
                };
                BasicValueEnum::IntValue(widened)
            } else {
                else_val
                    .ok_or_else(|| CompileError::LlvmError("if-else: missing else value".into()))?
            };
            (then_val, else_val)
        } else {
            (
                then_val.unwrap_or(self.context.i64_type().const_int(0, false).into()),
                else_val.unwrap_or(self.context.i64_type().const_int(0, false).into()),
            )
        };
        // Deep-eval 2026-08-09 (demos/04 describe_point; nested `else if`
        // with a concat arm + literal final else): string branch mismatch —
        // one branch yields a raw C-string pointer (plain literal) while the
        // other yields the Mimi string struct {ptr,i64} (concat/to_string).
        // The old path fell into the zero-fill below and silently dropped
        // the literal branch (printed empty). Wrap the raw pointer into a
        // string struct inside its own predecessor block so the phi stays
        // type-uniform and the value survives.
        let is_str_struct = |t: BasicTypeEnum<'ctx>| {
            matches!(t, BasicTypeEnum::StructType(st) if {
                let f = st.get_field_types();
                f.len() == 2
                    && matches!(f[0], BasicTypeEnum::PointerType(_))
                    && matches!(f[1], BasicTypeEnum::IntType(_))
            })
        };
        let (then_val, else_val) = match (&then_val, &else_val) {
            (BasicValueEnum::PointerValue(tv), ev)
                if is_str_struct(ev.get_type()) && then_reaches =>
            {
                self.builder.position_at_end(then_bb_end);
                if let Some(term) = then_bb_end.get_terminator() {
                    self.builder.position_before(&term);
                }
                let wrapped = self.wrap_c_string(*tv)?;
                (wrapped, else_val)
            }
            (tv, BasicValueEnum::PointerValue(ev))
                if is_str_struct(tv.get_type()) && else_reaches =>
            {
                self.builder.position_at_end(else_bb_end);
                if let Some(term) = else_bb_end.get_terminator() {
                    self.builder.position_before(&term);
                }
                let wrapped = self.wrap_c_string(*ev)?;
                (then_val, wrapped)
            }
            _ => (then_val, else_val),
        };
        self.builder.position_at_end(merge_bb);
        // Determine the authoritative phi type from a branch that actually
        // reaches the merge. Using then_val unconditionally was wrong when the
        // then branch terminated (returned): its phantom width dictated phi_type
        // and the live else value could be zero-filled below.
        let phi_type = if then_reaches {
            then_val.get_type()
        } else if else_reaches {
            else_val.get_type()
        } else {
            then_val.get_type()
        };
        // If the else branch's value STILL has a different type (e.g. then is a struct
        // but else fell through with i64 0 because there was no else block),
        // promote the else value to a zero of the phi type to avoid LLVM
        // physreg COPY errors from type-mismatched phi nodes.
        let else_val = if else_val.get_type() != phi_type {
            self.const_zero_for_type(phi_type)
        } else {
            else_val
        };
        let mut incoming: Vec<(
            &dyn inkwell::values::BasicValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        if then_reaches {
            incoming.push((&then_val, then_bb_end));
        }
        if else_reaches {
            incoming.push((&else_val, else_bb_end));
        }
        if !incoming.is_empty() {
            let phi = self
                .builder
                .build_phi(phi_type, "if_lastval")
                .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
            phi.add_incoming(&incoming);
            Ok(Some(phi.as_basic_value()))
        } else {
            // Both branches returned; the merge block is unreachable.
            Ok(Some(then_val))
        }
    }

    /// C2 (audit-syntax 2026-08-03): native lowering for `if let`.
    ///
    /// `if let PAT = INIT { THEN } else { ELSE }` desugars to the match
    /// expression
    ///
    /// ```text
    /// match INIT {
    ///     PAT => { THEN; () }
    ///     _   => { ELSE; () }      // bare () when there is no else branch
    /// }
    /// ```
    ///
    /// Statement form discards the result; both arms are normalized to unit
    /// (trailing `()` appended) so the merge phi is type-consistent regardless
    /// of what the branch bodies evaluate to. Reuses the existing match
    /// codegen (constructor/literal/wildcard dispatch) — no new pattern
    /// machinery. Mirrors the bytecode VM and the checker, both of which
    /// already support if-let.
    pub(in crate::codegen) fn compile_if_let_stmt(
        &mut self,
        pat: &Pattern,
        init: &Expr,
        then_: &Block,
        else_: &Option<Block>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let meta = AstNodeMeta::synthetic(AstOrigin::RuntimeSystem("codegen.if_let_lowering"));
        let unit_tail = Stmt::Expr(Expr::Literal(Lit::Unit));
        let mut then_body = then_.clone();
        then_body.push(unit_tail.clone());
        let else_body = match else_ {
            Some(block) => {
                let mut b = block.clone();
                b.push(unit_tail);
                Expr::Block(b)
            }
            None => Expr::Literal(Lit::Unit),
        };
        let arms = vec![
            MatchArm {
                meta,
                pat: pat.clone(),
                guard: None,
                body: Expr::Block(then_body),
            },
            MatchArm {
                meta,
                pat: Pattern::new(meta, PatternKind::Wildcard),
                guard: None,
                body: else_body,
            },
        ];
        self.compile_match_expr(init, &arms, vars, true)?;
        Ok(())
    }

    /// Call @llvm.stacksave() to capture the current stack pointer for arena region management
    pub(super) fn build_stacksave(&self) -> MimiResult<inkwell::values::PointerValue<'ctx>> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_type = i8_ptr.fn_type(&[], false);
        let fn_val = self
            .module
            .get_function("llvm.stacksave")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "llvm.stacksave",
                    fn_type,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let call = self
            .builder
            .build_call(fn_val, &[], "saved_stack")
            .map_err(|e| CompileError::LlvmError(format!("stacksave: {}", e)))?;
        let val = call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("stacksave returned void".to_string()))?;
        match val {
            BasicValueEnum::PointerValue(ptr) => Ok(ptr),
            _ => Err(CompileError::LlvmError(format!(
                "stacksave didn't return pointer, got {:?}",
                val
            ))),
        }
    }

    /// Call @llvm.stackrestore(i8*) to restore the stack pointer, freeing arena allocations
    pub(super) fn build_stackrestore(
        &self,
        saved: inkwell::values::PointerValue<'ctx>,
    ) -> MimiResult<()> {
        let i8_ptr_meta = BasicMetadataTypeEnum::PointerType(
            self.context.ptr_type(inkwell::AddressSpace::default()),
        );
        let fn_type = self.context.void_type().fn_type(&[i8_ptr_meta], false);
        let fn_val = self
            .module
            .get_function("llvm.stackrestore")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "llvm.stackrestore",
                    fn_type,
                    Some(inkwell::module::Linkage::External),
                )
            });
        self.builder
            .build_call(fn_val, &[BasicMetadataValueEnum::PointerValue(saved)], "")
            .map_err(|e| CompileError::LlvmError(format!("stackrestore: {}", e)))?;
        Ok(())
    }

    /// Compile a block and return the value of its last expression (for if-expressions)
    ///
    /// 0.34.36 (audit §6.7): scope bookkeeping parity with `compile_block`.
    /// This function also serves as the generic-function BODY compiler
    /// (`compile_generic_func`); previously `defer` / `on failure` / `shared
    /// let` statements fell into the catch-all and were silently dropped (the
    /// bytecode VM executes all of them), and value-position `return`s exited
    /// without the shared-release / defer / compensation cleanup that
    /// `compile_block`'s Return path emits (block.rs:136-140). The comp /
    /// defer / shared frames are pushed here (NOT heap — callers rely on this
    /// path not pushing a heap scope, see expr/match.rs:1947) and popped on
    /// every exit path.
    pub(super) fn compile_block_last_val(
        &mut self,
        block: &Block,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        self.push_comp_scope();
        self.push_defer_scope();
        self.push_shared_scope();
        let mut last_val = self.context.i64_type().const_int(0, false).into();
        for stmt in block {
            // Run compensations before exit() — same execution-point hook as
            // compile_block (0.34.36, audit §6/§9). Without it, an `on failure`
            // registered in this block would be discarded by pop_comp_scope
            // instead of firing on a later `exit` in the same block.
            if let Stmt::Expr(expr) = stmt.unlocated() {
                if let Expr::Call(callee, _) = expr.unlocated() {
                    if let Expr::Ident(name) = callee.unlocated() {
                        if name == "exit" {
                            self.compile_compensations(vars)?;
                        }
                    }
                }
            }
            match stmt.unlocated() {
                Stmt::Expr(e) => {
                    last_val = self.compile_expr(e, vars)?;
                }
                Stmt::Return(Some(e)) => {
                    let mut val = self.compile_expr(e, vars)?;
                    // v0.34.16 (ADR-002): multi-target transition return
                    // inside an if-branch — wrap into {i32 tag, i64 payload}.
                    if self.in_multi_target_transition {
                        let state_name = match e.unlocated() {
                            Expr::Record { ty: Some(ty_name), .. } => ty_name.clone(),
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "multi-target transition return must construct a target state record (e.g. `return TargetState { ... }`)".to_string(),
                                ))
                            }
                        };
                        // C1 fix: tag = flow-wide name-sorted ordinal (see
                        // register_flow_multi_target_enums), never the
                        // per-transition subset index.
                        let tag = self
                            .multi_target_global_ordinals
                            .get(&self.current_flow_name)
                            .and_then(|m| m.get(&state_name))
                            .copied()
                            .ok_or_else(|| {
                                CompileError::LlvmError(format!(
                                    "returned state '{state_name}' has no global multi-target ordinal (flow: {:?}, transition targets: {:?})",
                                    self.current_flow_name,
                                    self.multi_target_states
                                ))
                            })?;
                        let state_ty = self.flow_state_llvm_type(&state_name);
                        val = self.wrap_multi_target_value(val, tag, state_ty)?;
                    }
                    let ret_type = self
                        .current_fn_ret_type()
                        .unwrap_or_else(|| BasicTypeEnum::IntType(self.context.i64_type()));
                    val = self.adjust_int_val(val, ret_type)?;
                    // P0-4: heap-copy string returns so the caller
                    // doesn't later free() a .rodata literal pointer.
                    val = self.claim_string_return_value(val, ret_type, Some(e), vars)?;
                    // L6: claim a returned custom-enum payload box so the
                    // callee's scope-exit free skips it (caller re-registers
                    // via EnumBox). Mirrors the func.rs emit_return path.
                    self.claim_returned_enum_box(val, ret_type)?;
                    // 0.34.24: variant-layout coercion (mirror of handler
                    // above and func.rs emit_return) — see the comment there.
                    val = self.coerce_variant_value(
                        val,
                        ret_type,
                        self.current_fn_ret_ty_ast.as_ref(),
                    )?;
                    // 0.34.41 第二档: ensures with `result` binding (same fix
                    // as compile_block's Return; was unbound here too).
                    self.compile_ensures_asserts(Some(val), ret_type, vars)?;
                    val = self.load_return_value_if_needed(val)?;
                    // 0.34.36 (audit §6.7): full return-path cleanup parity
                    // with compile_block's Return (block.rs:128-140). The old
                    // code only flushed heap scopes: registered shared
                    // releases never ran and defer blocks were dropped on
                    // value-position returns (the VM runs both).
                    self.emit_all_shared_releases()?;
                    self.discard_shared_scope();
                    self.flush_heap_scopes_to_boundary()?;
                    self.pop_defer_scope(vars)?;
                    self.pop_comp_scope();
                    self.build_return(Some(&val))?;
                    return Ok(val);
                }
                Stmt::Return(None) => {
                    // 0.34.36 (audit §6.7): same cleanup parity as the
                    // valued-Return arm above (mirrors compile_block:144-159).
                    let ret_type = self
                        .current_fn_ret_type()
                        .unwrap_or_else(|| BasicTypeEnum::IntType(self.context.i64_type()));
                    self.compile_ensures_asserts(None, ret_type, vars)?;
                    self.emit_all_shared_releases()?;
                    self.discard_shared_scope();
                    self.flush_heap_scopes_to_boundary()?;
                    self.pop_defer_scope(vars)?;
                    self.pop_comp_scope();
                    // 0.35.23 (deep-eval): bare `return` in a unit function —
                    // the unit signature is i64, so `ret void` (old) was
                    // invalid IR: O1 CalledValuePropagationPass SIGSEGV'd on
                    // "func f() { if true { return } }". Return the i64 zero.
                    let zero = self.zero_value_for(ret_type);
                    self.build_return(Some(&zero))?;
                    return Ok(self.context.i64_type().const_int(0, false).into());
                }
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ty,
                    mut_: _,
                    ref_,
                } => {
                    let ref_flag = *ref_;
                    // 0.35.23 deep-eval: `let ref x: i32 = 42` in the
                    // compile_block_last_val path (legacy funcs with bare
                    // `return`) binds the value into a plain slot; record it
                    // so `*x` derefs by identity (VM parity).
                    if ref_flag {
                        if let PatternKind::Variable(name) = &pat.kind {
                            self.ref_bound_vars.insert(name.clone());
                        }
                    }
                    // Typed list literals: seed pending_list_elem_type so
                    // Result/Option list elements pack with a uniform layout.
                    let saved_list_elem = self.pending_list_elem_type.take();
                    if matches!(init.unlocated(), Expr::List(_)) {
                        if let Some(decl_ty) = ty.as_ref() {
                            if let Type::Name(n, args) = decl_ty.unlocated() {
                                if n == "List" && args.len() == 1 {
                                    self.pending_list_elem_type = Some(args[0].clone());
                                }
                            }
                        }
                    }
                    let val = self.compile_expr(init, vars)?;
                    self.pending_list_elem_type = saved_list_elem;
                    let val = self.normalize_string_value(val, init)?;
                    // SD-7 (0.34.34): narrowing bind into an annotated i32 slot
                    // range-checks before the silent truncate (compile_block's
                    // top-level bodies flow through here via compile_func_legacy).
                    if let Some(decl_ty) = &ty {
                        if Self::annotated_type_name(decl_ty) == Some("i32") {
                            if let BasicValueEnum::IntValue(iv) = val {
                                if iv.get_type().get_bit_width() > 32 {
                                    self.emit_i32_range_guard(iv, "let-bind")?;
                                }
                            }
                        }
                    }
                    let val = if let Some(decl_ty) = &ty {
                        // Populate var_type_names from the type annotation so that
                        // infer_object_type can return e.g. "Option<string>" instead
                        // of just "Option" for generic variant types.
                        if let Some(full) = self.get_full_type_name(decl_ty) {
                            if let PatternKind::Variable(name) = &pat.kind {
                                self.var_type_names.insert(name.clone(), full.clone());
                            }
                        }
                        self.inflate_variant_struct(val, decl_ty)?
                    } else {
                        val
                    };
                    self.compile_pattern_bind(pat, val, vars)?;
                    if let PatternKind::Tuple(sub_pats) = &pat.kind {
                        if let Expr::Call(callee, _) = init.unlocated() {
                            if let Expr::Ident(func_name) = callee.unlocated() {
                                if func_name == "map_get" && sub_pats.len() == 2 {
                                    if let PatternKind::Variable(name) = &sub_pats[1].kind {
                                        self.var_type_names.insert(name.clone(), "any".to_string());
                                    }
                                }
                            }
                        }
                    }
                    if let PatternKind::Variable(name) = &pat.kind {
                        // 0.35.23 deep-eval: `let y = x` inherits the source
                        // variable's tracked type — the compile_block_last_val
                        // counterpart (mimi-log main's `let mut display =
                        // filtered` inside an if/else branch compiles through
                        // THIS path; without it `for e in display` bound `e`
                        // as bare i64 and field access failed E0700).
                        if let Expr::Ident(src_name) = init.unlocated() {
                            if !self.var_type_names.contains_key(name.as_str()) {
                                if let Some(src_ty) = self.var_type_names.get(src_name).cloned() {
                                    self.var_type_names.insert(name.clone(), src_ty);
                                }
                            }
                            if !self.var_types.contains_key(name.as_str()) {
                                if let Some(src_ty) = self.var_types.get(src_name).cloned() {
                                    self.var_types.insert(name.clone(), src_ty);
                                }
                            }
                        }
                        if self.expr_is_string(init) {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        }
                        // 0.35.23 deep-eval: turbofish-typed bindings in the
                        // compile_block_last_val path (mimichat
                        // display_server_message's `let room_list =
                        // from_json::<List<string>>(rooms_json)` inside an
                        // if-branch) — without this `for r in room_list {
                        // println("    - " + r) }` bound `r` as bare i64 and
                        // the string concat failed "add requires same numeric
                        // types".
                        if let Expr::Turbofish(_func_name, turbo_type_args, _) = init.unlocated() {
                            if let Some(ta) = turbo_type_args.first() {
                                if let Type::Name(tn, args) = ta.unlocated() {
                                    if tn == "List" && !args.is_empty() {
                                        if let Some(full) = self.get_full_type_name(ta) {
                                            self.var_type_names.insert(name.clone(), full);
                                        } else {
                                            self.var_type_names
                                                .insert(name.clone(), crate::core::fmt_type(ta));
                                        }
                                        self.var_types.insert(name.clone(), ta.clone());
                                        self.register_list_elem_type(name, ta);
                                    } else if (tn == "Map" || tn == "Set") && !args.is_empty() {
                                        if let Some(full) = self.get_full_type_name(ta) {
                                            self.var_type_names.insert(name.clone(), full);
                                        } else {
                                            self.var_type_names.insert(name.clone(), tn.clone());
                                        }
                                        self.var_types.insert(name.clone(), ta.clone());
                                    } else {
                                        self.var_type_names.insert(name.clone(), tn.clone());
                                        self.var_types.insert(name.clone(), ta.clone());
                                    }
                                }
                            }
                        }
                        // 2026-08-06 (audit 1j): Set literals `{1, 2}` compile
                        // to an opaque i64 handle with no var_type_names entry,
                        // so `let s = {1, 2}; contains(s, x)` fell through to
                        // compile_contains ("expected a list"). Track the Set
                        // type name so the contains dispatch can route Set
                        // haystacks to mimi_set_contains.
                        if matches!(init.unlocated(), Expr::SetLiteral(_)) {
                            self.var_type_names.insert(name.clone(), "Set".to_string());
                        }
                        if matches!(init.unlocated(), Expr::MapLiteral { .. }) {
                            self.var_type_names.insert(name.clone(), "Map".to_string());
                        }
                        // 0.35.14 (DX backlog #18): tuple fn-element extraction.
                        self.record_tuple_fn_elems(name, init);
                        self.register_tuple_index_fn_binding(name, init);
                        if let Expr::Ident(fn_name) = init.unlocated() {
                            if self.module.get_function(fn_name.as_str()).is_some() {
                                self.fn_ptr_var_names.insert(name.clone());
                            }
                            if self.cap_type_names.contains(fn_name.as_str()) {
                                self.var_type_names.insert(name.clone(), fn_name.clone());
                            }
                            // Track return types for builtins whose result is
                            // a List<T> or other type the caller needs to
                            // recover when indexing. Without this, `let xs =
                            // sort_str(ys)` would leave `xs` untyped and
                            // `xs[i]` would be returned as i64 (the raw
                            // element slot) instead of the proper struct/
                            // string pointer.
                            match fn_name.as_str() {
                                "listdir" | "walk_dir" | "str_split" | "sort_str" | "keys" => {
                                    self.var_type_names
                                        .insert(name.clone(), "List<string>".to_string());
                                    self.var_types.insert(
                                        name.clone(),
                                        Type::Name(
                                            "List".into(),
                                            vec![Type::Name("string".into(), vec![])],
                                        ),
                                    );
                                }
                                "sort_f64" => {
                                    self.var_type_names
                                        .insert(name.clone(), "List<f64>".to_string());
                                    self.var_types.insert(
                                        name.clone(),
                                        Type::Name(
                                            "List".into(),
                                            vec![Type::Name("f64".into(), vec![])],
                                        ),
                                    );
                                }
                                "exec" | "exec_safe" => {
                                    self.var_type_names
                                        .insert(name.clone(), "ExecResult".to_string());
                                }
                                "file_stat" => {
                                    self.var_type_names
                                        .insert(name.clone(), "StatResult".to_string());
                                }
                                _ => {}
                            }
                        }
                        // Track return types for calls that produce List<string>
                        // (e.g. std::strings::words/lines/split).  The callee is a
                        // function name, not a bare identifier, so it is not covered
                        // by the branch above.
                        if let Expr::Call(callee, args) = init.unlocated() {
                            if let Expr::Ident(fn_name) = callee.unlocated() {
                                // General user-function return-type tracking (e.g. std::csv::parse
                                // returns List<List<string>>). This lets downstream indexing and
                                // printing recover the concrete element type.
                                if let Some(fdef) = self.func_defs.get(fn_name.as_str()) {
                                    if let Some(ret_ty) = &fdef.ret {
                                        if let Some(full) = self.get_full_type_name(ret_ty) {
                                            self.var_type_names.insert(name.clone(), full);
                                        }
                                        self.var_types.insert(name.clone(), ret_ty.clone());
                                    }
                                }
                                match fn_name.as_str() {
                                    // 0.35.23 deep-eval: `let rr = read_file(..)`
                                    // inside an if/then branch (compile_block_last_val
                                    // path) left rr untyped, so `rr.is_ok()` failed
                                    // with "method 'is_ok' not compiled for type 'rr'"
                                    // (mimi-todo load_tasks). Mirror compile_block's
                                    // builtin Result registration here.
                                    "read_file"
                                    | "read_file_partial"
                                    | "read_file_bytes"
                                    | "getenv"
                                    | "base64_decode"
                                    | "mimi_lexer_tokenize"
                                    | "mimi_parse_source" => {
                                        self.var_type_names.insert(
                                            name.clone(),
                                            "Result<string,string>".to_string(),
                                        );
                                    }
                                    "write_file" | "write_file_bytes" => {
                                        self.var_type_names
                                            .insert(name.clone(), "Result<(), string>".to_string());
                                    }
                                    "words" | "lines" | "split" | "str_split" | "listdir"
                                    | "walk_dir" | "sort_str" | "keys" => {
                                        self.var_type_names
                                            .insert(name.clone(), "List<string>".to_string());
                                        self.var_types.insert(
                                            name.clone(),
                                            Type::Name(
                                                "List".into(),
                                                vec![Type::Name("string".into(), vec![])],
                                            ),
                                        );
                                    }
                                    "sort_f64" => {
                                        self.var_type_names
                                            .insert(name.clone(), "List<f64>".to_string());
                                        self.var_types.insert(
                                            name.clone(),
                                            Type::Name(
                                                "List".into(),
                                                vec![Type::Name("f64".into(), vec![])],
                                            ),
                                        );
                                    }
                                    "map_new" => {
                                        self.var_type_names.insert(name.clone(), "Map".to_string());
                                    }
                                    "map_set" | "map_remove" => {
                                        if let Some(val_arg) = args.get(2) {
                                            let vt = self.infer_object_type(val_arg, vars);
                                            if vt.starts_with('(')
                                                || self.is_product_tuple_alias(&vt)
                                            {
                                                let resolved = if self.is_product_tuple_alias(&vt) {
                                                    self.resolve_alias_type_name(&vt)
                                                } else {
                                                    vt
                                                };
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    format!("Map<string, {}>", resolved),
                                                );
                                            } else if !vt.is_empty() && vt != "i64" && vt != "int" {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    format!("Map<string, {}>", vt),
                                                );
                                            } else {
                                                self.var_type_names
                                                    .insert(name.clone(), "Map".to_string());
                                            }
                                        } else {
                                            self.var_type_names
                                                .insert(name.clone(), "Map".to_string());
                                        }
                                    }
                                    "set_new" | "set_insert" | "set_remove" => {
                                        self.var_type_names.insert(name.clone(), "Set".to_string());
                                    }
                                    _ => {}
                                }
                            } else if let Expr::Field(obj, method_name) = callee.unlocated() {
                                // Method call return type: flow transition dispatch
                                // (FlowName::transition(from, ...). Similar to the
                                // tracking in compile_stmts lines 432-458.
                                if let Expr::Ident(flow_name) = obj.unlocated() {
                                    if let Some(flow) = self.flow_defs.get(flow_name) {
                                        let from_type = args
                                            .first()
                                            .map(|a| self.infer_object_type(a, vars))
                                            .unwrap_or_default();
                                        let t = flow.transitions.iter().find(|t| {
                                            t.name == *method_name && t.from_state == from_type
                                        });
                                        if let Some(t) = t {
                                            let from_state = t.from_state.clone();
                                            let to_states = t.to_states.clone();
                                            let fails = t.fails.clone();
                                            if let Some(to) = to_states.first() {
                                                self.var_type_names
                                                    .insert(name.clone(), to.clone());
                                                self.track_flow_result_type(
                                                    name,
                                                    &from_state,
                                                    to,
                                                    fails,
                                                );
                                            }
                                        }
                                    }
                                }
                                // Q3 (rc-quality-gate-0.34.25a): non-flow method
                                // calls — infer the declared return type from
                                // string builtins or trait impls so `let r =
                                // s.get_float(k)` keeps its Result<…> identity
                                // for downstream display/dispatch.
                                if !self.var_type_names.contains_key(name.as_str()) {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    let ret = if obj_type == "string" {
                                        let r = self.infer_string_method_return_type(method_name);
                                        if r.is_empty() {
                                            self.infer_impl_method_return_type(
                                                &obj_type,
                                                method_name,
                                            )
                                        } else {
                                            r
                                        }
                                    } else {
                                        self.infer_impl_method_return_type(&obj_type, method_name)
                                    };
                                    if !ret.is_empty() {
                                        self.var_type_names.insert(name.clone(), ret);
                                    }
                                }
                            }
                        }
                        // from_json::<Map<…>> / Set turbofish type tracking
                        if let Expr::Turbofish(_fn, turbo_type_args, _) = init.unlocated() {
                            if let Some(ta) = turbo_type_args.first() {
                                if let Type::Name(tn, args) = ta.unlocated() {
                                    if (tn == "Map" || tn == "Set") && !args.is_empty() {
                                        if let Some(full) = self.get_full_type_name(ta) {
                                            self.var_type_names.insert(name.clone(), full);
                                        }
                                        self.var_types.insert(name.clone(), ta.clone());
                                    }
                                }
                            }
                        }
                        // 0.35.23 deep-eval (mimi-make E0707): the
                        // compile_block_last_val counterpart of the
                        // compile_block If/Block backfill — `let r0 =
                        // if n > 0 { parse_one(..) } else { Rule {..} }`
                        // in a value-position body (main) flows through
                        // THIS path; without it `r0.target` fails E0707.
                        if !self.var_type_names.contains_key(name.as_str())
                            && matches!(init.unlocated(), Expr::If { .. } | Expr::Block(_))
                        {
                            let inferred = self.infer_object_type(init, vars);
                            if !inferred.is_empty() {
                                self.var_type_names.insert(name.clone(), inferred);
                            }
                        }
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    if let Some(v) = self.compile_if_stmt(cond, then_, else_, vars, false)? {
                        last_val = v;
                    }
                }
                Stmt::IfLet {
                    pat,
                    init,
                    then_,
                    else_,
                } => {
                    // C2 (audit-syntax): desugar to match; if-let yields unit, so
                    // it does not contribute to the block's last value.
                    self.compile_if_let_stmt(pat, init, then_, else_, vars)?;
                }
                Stmt::Break(_) => {
                    self.compile_break_stmt()?;
                }
                Stmt::Continue => {
                    self.compile_continue_stmt()?;
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, vars)?;
                }
                Stmt::WhileLet { pat, init, body } => {
                    self.compile_while_let_stmt(pat, init, body, vars)?;
                }
                Stmt::Loop(body) => {
                    self.compile_loop_stmt(body, vars)?;
                }
                Stmt::For {
                    var,
                    iterable,
                    body,
                } => {
                    self.compile_for_stmt(var, iterable, body, vars)?;
                }
                Stmt::Block(block) => {
                    let inner_vars = &mut vars.clone();
                    last_val = self.compile_block_last_val(block, inner_vars)?;
                    // Merge inner variable bindings back to outer scope
                    vars.extend(std::mem::take(inner_vars));
                }
                Stmt::Unsafe(block) => {
                    // Mirror the Block arm: unwrap and compile for value.
                    let inner_vars = &mut vars.clone();
                    last_val = self.compile_block_last_val(block, inner_vars)?;
                    vars.extend(std::mem::take(inner_vars));
                }
                // 0.34.36 (audit §6.7): these statements previously fell into
                // the catch-all and were silently dropped in value position
                // (if-expression arms and generic function bodies). The VM
                // executes all of them, so they are now lowered explicitly.
                Stmt::Defer(block) => {
                    // Register defer block for LIFO execution at scope exit.
                    self.register_defer(block);
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for error-exit execution.
                    self.register_comp(block);
                }
                Stmt::SharedLet {
                    kind,
                    name,
                    ty,
                    init,
                } => {
                    self.compile_shared_let_stmt(kind, name, ty, init, vars)?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate and, for a capability variable, release the
                    // runtime cap handle (mirrors compile_block's Drop, H4).
                    if let Expr::Ident(name) = expr.unlocated() {
                        if self.is_cap_var(name) {
                            if let Some(drop_fn) = self.module.get_function("mimi_cap_drop") {
                                if let Some((alloca, _)) = vars.get(name) {
                                    let handle = self.build_load(
                                        self.context.i64_type(),
                                        *alloca,
                                        "cap_drop_handle",
                                    )?;
                                    let _ = self.builder.build_call(
                                        drop_fn,
                                        &[handle.into()],
                                        "cap_drop",
                                    );
                                }
                            }
                            self.consume_cap(name)?;
                        }
                    }
                    self.compile_expr(expr, vars)?;
                }
                Stmt::Requires(..)
                | Stmt::Ensures(..)
                | Stmt::Invariant(..)
                | Stmt::Math(_)
                | Stmt::Ellipsis
                | Stmt::Located { .. }
                | Stmt::Func(_) => {
                    // Super-comments / contracts / declaration-level items carry
                    // no runtime semantics in value position.
                }
                _ => {}
            }
        }
        // 0.34.36 (audit §6.7): pop the frames pushed on entry, mirroring
        // compile_block's block-end cleanup. Order matters: release this
        // block's shared registrations, then run defers (always, LIFO), then
        // discard compensations (normal exit does not run them).
        self.pop_shared_scope()?;
        self.pop_defer_scope(vars)?;
        self.pop_comp_scope();
        Ok(last_val)
    }

    /// Given a type definition with generic params and record field expressions,
    /// infer the concrete types for the generic params by examining the field values.
    /// This is needed so `var_types` can store the full concrete type (e.g.
    /// `Pair<i32>`) for record literals like `Pair { a: 10, b: 20 }`.
    pub(super) fn try_infer_generic_from_fields(
        &self,
        td: &TypeDef,
        fields: &[RecordFieldExpr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        type_params: &[String],
    ) -> HashMap<String, Type> {
        fn type_ident_name(ty: &Type) -> String {
            if let Type::Name(n, _) = ty.unlocated() {
                n.clone()
            } else {
                String::new()
            }
        }
        let mut param_types: HashMap<String, Type> = HashMap::new();
        if let TypeDefKind::Record(field_defs) = &td.kind {
            for rf in fields {
                if let Some(fd) = field_defs.iter().find(|f| f.name == rf.name) {
                    let field_ty_name = self.infer_object_type(&rf.value, vars);
                    let ftn = type_ident_name(&fd.ty);
                    if !field_ty_name.is_empty()
                        && field_ty_name != "unknown"
                        && type_params.contains(&ftn)
                    {
                        param_types.insert(ftn, Type::Name(field_ty_name, vec![]));
                    }
                }
            }
        }
        param_types
    }
}
