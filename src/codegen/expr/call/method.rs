use crate::ast::*;
use crate::codegen::{call_try_basic_value, CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};
use inkwell::IntPredicate;
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    /// Handle method dispatch for obj.method(args) calls.
    pub(in crate::codegen) fn compile_method_call(
        &mut self,
        obj: &Expr,
        method_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Method call: obj.method(args)
        // Determine the type of the object to find the actor/trait name
        let obj_type = self.infer_object_type(obj, vars);

        // 0. Special case: weak<T>.upgrade() -> Option<T*>
        if method_name == "upgrade"
            && (obj_type.starts_with("weak ") || obj_type.starts_with("weak_local "))
        {
            if let Expr::Ident(name) = obj {
                return self.compile_weak_upgrade(name, vars);
            }
        }

        // 0.5. Shared variable deref: if obj is a shared var and method is "deref",
        // load the value from the heap pointer directly.
        // Also handles raw pointer values from w.upgrade() which returns i8*,
        // and Option<shared T> values where deref extracts payload+loads.
        if method_name == "deref" {
            if let Expr::Ident(name) = obj {
                if let Some(val) = self.compile_shared_deref(name, vars)? {
                    return Ok(val);
                }
            }
        }

        // 0.6. Weak variable upgrade without explicit type annotation:
        // fallback when infer_object_type returns the variable name, not "weak T".
        if method_name == "upgrade" {
            if let Expr::Ident(name) = obj {
                if self.shared_var_names.contains(name.as_str()) {
                    return self.compile_weak_upgrade(name, vars);
                }
            }
        }

        let actor_method = format!("{}__{}__method", obj_type, method_name);

        // 1. Try actor mailbox method dispatch (v0.28.19)
        //    This routes through mimi_actor_call for real concurrency.
        //    Falls back to direct call for self-calls inside actor methods.
        if self.actor_names.contains(&obj_type) {
            if let Some(result) =
                self.try_compile_actor_mailbox_call(obj, method_name, args, vars)?
            {
                return Ok(result);
            }
        }

        // 1a. Fallback: direct actor method dispatch (legacy / self-call)
        if let Some(function) = self.module.get_function(&actor_method) {
            return self.compile_self_method_call(obj, args, vars, function, "method_call");
        }

        // 1.2. Variant method dispatch (Result/Option combinators)
        if obj_type.starts_with("Result<")
            || obj_type.starts_with("Option<")
            || obj_type == "Result"
            || obj_type == "Option"
        {
            if let Ok(result) = self.compile_variant_method(obj, method_name, args, vars) {
                return Ok(result);
            }
        }

        // 1.5. Special case: Type.spawn() constructor call for actors
        if method_name == "spawn" {
            let spawn_name = format!("{}_spawn", obj_type);
            if let Some(spawn_fn) = self.module.get_function(&spawn_name) {
                let call = self.build_call(spawn_fn, &[], "actor_spawn")?;
                return Ok(call_try_basic_value(&call)
                    .unwrap_or(self.context.i64_type().const_int(0, false).into()));
            }
        }

        // v0.29.37: Type.spawn_detached() — for codegen, use the same spawn path.
        // The detached flag is a runtime concept; in codegen the actor handle
        // is returned the same way as regular spawn.
        if method_name == "spawn_detached" {
            let spawn_name = format!("{}_spawn", obj_type);
            if let Some(spawn_fn) = self.module.get_function(&spawn_name) {
                let call = self.build_call(spawn_fn, &[], "actor_spawn_detached")?;
                return Ok(call_try_basic_value(&call)
                    .unwrap_or(self.context.i64_type().const_int(0, false).into()));
            }
        }

        // 1.7. Cap method dispatch: split() for capability types
        if self.cap_type_names.contains(&obj_type) && method_name == "split" {
            // For now, return a dummy i64 value.
            // The result is not used in current tests; a proper runtime
            // mimi_cap_split function would be needed for real usage.
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        // 2. Try trait method dispatch: type_impls[type_name][trait_name][method_name]
        // Impl blocks are keyed by the base type name (e.g. "List") even when the
        // call site sees a concrete instantiation like "List<T>" or "List<i32>".
        // For generic impls called with concrete types, monomorphize on-demand.
        let base_obj_type = base_type_name(&obj_type);
        // Find the trait method and fn_name (need immutable borrow first)
        let trait_method_info: Option<(String, bool, Vec<Type>)> =
            self.type_impls.get(base_obj_type).and_then(|trait_impls| {
                trait_impls.iter().find_map(|(trait_name, methods)| {
                    if !methods.iter().any(|m| m.name == *method_name) {
                        return None;
                    }
                    let fn_name = format!("{}__{}__{}", base_obj_type, trait_name, method_name);
                    let impl_type_args = self
                        .impl_type_args
                        .get(base_obj_type)
                        .cloned()
                        .unwrap_or_default();
                    let is_generic_call = impl_type_args.iter().any(|ta| {
                        if let Type::Name(tn, _) = ta {
                            obj_type.contains(&format!("<{}>", tn))
                        } else {
                            false
                        }
                    });
                    Some((fn_name, !is_generic_call, impl_type_args))
                })
            });
        if let Some((fn_name, is_concrete_call, impl_type_args)) = trait_method_info {
            // Try monomorphized version for concrete types
            if is_concrete_call && !impl_type_args.is_empty() {
                let inner = obj_type.strip_prefix("List<").and_then(|s| {
                    let mut depth = 1u32;
                    let mut end = 0;
                    for (i, ch) in s.char_indices() {
                        match ch {
                            '<' => depth += 1,
                            '>' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = i;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if end > 0 {
                        Some(&s[..end])
                    } else {
                        None
                    }
                });
                if let Some(inner) = inner {
                    let concrete_types: Vec<&str> = inner.split(',').map(|s| s.trim()).collect();
                    if concrete_types.len() == impl_type_args.len() {
                        let mut type_map: HashMap<String, Type> = HashMap::new();
                        for (ta, ct) in impl_type_args.iter().zip(concrete_types.iter()) {
                            if let Type::Name(tn, _) = ta {
                                type_map.insert(tn.clone(), Type::Name(ct.to_string(), vec![]));
                            }
                        }
                        let mangled = Self::mangle_name(&fn_name, &type_map);
                        if self.module.get_function(&mangled).is_none() {
                            // Clone the func def from type_impls (no borrow conflict)
                            let mut func =
                                self.type_impls.get(base_obj_type).and_then(|trait_impls| {
                                    trait_impls.values().find_map(|methods| {
                                        methods.iter().find(|m| m.name == *method_name).cloned()
                                    })
                                });
                            // Prepend self parameter (was prepended during compile_impl_methods)
                            if let Some(ref mut f) = func {
                                // Rename func to fn_name so compile_generic_func mangles
                                // with the full path (e.g. List__ListExt__map$T_Item),
                                // matching the caller's expected mangled name.
                                f.name = fn_name.clone();
                                let self_ty = Type::Ref(
                                    None,
                                    Box::new(Type::Name(
                                        base_obj_type.to_string(),
                                        impl_type_args.clone(),
                                    )),
                                );
                                f.params.insert(
                                    0,
                                    crate::ast::Param {
                                        name: "self".into(),
                                        ty: self_ty,
                                        mut_: false,
                                        default_value: None,
                                        borrow: None,
                                    },
                                );
                                self.compile_generic_func(f, &type_map)?;
                            }
                        }
                        if let Some(function) = self.module.get_function(&mangled) {
                            return self.compile_self_method_call(
                                obj,
                                args,
                                vars,
                                function,
                                "trait_call",
                            );
                        }
                    }
                }
            }
            // Fallback: use the generic (non-monomorphized) version
            if let Some(function) = self.module.get_function(&fn_name) {
                return self.compile_self_method_call(obj, args, vars, function, "trait_call");
            }
        }

        // 2b. Built-in clone: any concrete value can be cloned by copying the loaded value
        if method_name == "clone" && args.is_empty() {
            let obj_val = self.compile_expr(obj, vars)?;
            return Ok(obj_val);
        }

        // 3. True vtable indirect dispatch for dyn Trait objects
        if obj_type.starts_with("dyn ") {
            let trait_name = obj_type.strip_prefix("dyn ").unwrap_or("");
            if !trait_name.is_empty() && !trait_name.contains(' ') {
                return self.compile_dyn_trait_call(obj, method_name, args, vars, trait_name);
            }
            return Err(format!(
                "[E0708] cannot dispatch method '{}' on {}",
                method_name, obj_type
            )
            .into());
        }

        // 3b. Try impl Trait dispatch (same logic as dyn Trait)
        if obj_type.starts_with("impl ") {
            let trait_name = obj_type.strip_prefix("impl ").unwrap_or("");
            if !trait_name.is_empty() && !trait_name.contains(' ') {
                return self.compile_impl_trait_call(obj, method_name, args, vars, trait_name);
            }
            return Err(format!(
                "[E0708] cannot dispatch method '{}' on {}",
                method_name, obj_type
            )
            .into());
        }

        // 4. Try enum constructor: {Type}_{Variant}(args)
        if self.type_defs.contains_key(&obj_type) {
            let ctor_name = format!("{}_{}", obj_type, method_name);
            if let Some(function) = self.module.get_function(&ctor_name) {
                return self.compile_enum_constructor_call(args, vars, function);
            }
        }

        // 5. Set built-in method dispatch
        if obj_type == "set" || obj_type.starts_with("Set") || obj_type.starts_with("set") {
            return self.compile_set_method(obj, method_name, args, vars);
        }

        // 5b. Builtin string method fallback: s.trim() → str_trim(s)
        //     Mirrors the interpreter's hardcoded string methods (interp/call.rs:704-769).
        if obj_type == "string" {
            if method_name == "len" {
                // len needs pending_len_is_string set before compile_call checks it.
                self.pending_len_is_string = true;
                let obj_expr = obj.clone();
                let call_expr =
                    Expr::Call(Box::new(Expr::Ident("len".to_string())), vec![obj_expr]);
                return self.compile_expr(&call_expr, vars);
            }
            if let Some(builtin_name) = string_method_to_builtin(method_name) {
                let obj_expr = obj.clone();
                let mut all_args = vec![obj_expr];
                all_args.extend(args.iter().cloned());
                let call_expr =
                    Expr::Call(Box::new(Expr::Ident(builtin_name.to_string())), all_args);
                return self.compile_expr(&call_expr, vars);
            }
        }

        // 5c. Builtin List method dispatch: parts.len() → len(parts)
        if (obj_type.starts_with("List<") || obj_type == "List") && method_name == "len" {
            let obj_expr = obj.clone();
            let call_expr = Expr::Call(Box::new(Expr::Ident("len".to_string())), vec![obj_expr]);
            return self.compile_expr(&call_expr, vars);
        }

        // 6. Flow transition call: FlowName::transition(from_state, ...event_args)
        //    When `obj` is a flow type name (not an instance), dispatch to the
        //    synthetic transition function. The first argument is the from-state
        //    payload; remaining args are the event parameters.
        if let Expr::Ident(flow_name) = obj {
            if self.flow_defs.contains_key(flow_name) {
                return self.compile_flow_transition_call(flow_name, method_name, args, vars);
            }
        }
        // Also detect when obj_type is a bare flow name (via type tracking).
        if self.flow_defs.contains_key(&obj_type) {
            return self.compile_flow_transition_call(&obj_type, method_name, args, vars);
        }

        Err(CompileError::Generic(format!(
            "method '{}' not compiled for type '{}' (missing crate?)",
            method_name, obj_type
        )))
    }

    /// Compile `FlowName::transition(from_state, ...args)` as a direct call to
    /// the synthetic LLVM function `{Flow}__{transition}__from_{FromState}`.
    fn compile_flow_transition_call(
        &mut self,
        flow_name: &str,
        transition_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let flow = self
            .flow_defs
            .get(flow_name)
            .ok_or_else(|| CompileError::Generic(format!("unknown flow '{}'", flow_name)))?;

        // Find matching transition by name. Prefer the one whose from_state
        // matches the first arg's inferred type when multiple share a name.
        let from_type = args
            .first()
            .map(|a| self.infer_object_type(a, vars))
            .unwrap_or_default();

        let candidates: Vec<&TransitionDef> = flow
            .transitions
            .iter()
            .filter(|t| t.name == transition_name)
            .collect();
        if candidates.is_empty() {
            return Err(CompileError::Generic(format!(
                "flow '{}' has no transition '{}'",
                flow_name, transition_name
            )));
        }
        let t = candidates
            .iter()
            .find(|t| t.from_state == from_type)
            .or_else(|| candidates.first())
            .copied()
            .ok_or_else(|| {
                CompileError::Generic(format!(
                    "flow '{}' has no transition '{}'",
                    flow_name, transition_name
                ))
            })?;

        let is_fallback = t.is_fallback;
        let enters_fault = is_fallback || t.to_states.iter().any(|s| s == "Fault");
        let fn_name = CodeGenerator::transition_fn_name(flow_name, &t.name, &t.from_state);
        let function = self.module.get_function(&fn_name).ok_or_else(|| {
            CompileError::Generic(format!(
                "flow transition function '{}' not found (was the flow compiled?)",
                fn_name
            ))
        })?;

        // Compile args by value (self payload + event params).
        // Record construction yields an alloca pointer; ordinary Ident loads
        // yield StructValue. The synthetic transition function takes structs
        // by value (same as declare_func/compile_func), so load any pointer
        // args that correspond to struct parameters.
        let mut compiled_args = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let mut val = self.compile_expr(arg, vars)?;
            if let Some(param) = function.get_nth_param(i as u32) {
                if let BasicTypeEnum::StructType(sty) = param.get_type() {
                    if let BasicValueEnum::PointerValue(pv) = val {
                        val =
                            self.build_load(BasicTypeEnum::StructType(sty), pv, "flow_arg_load")?;
                    }
                }
            }
            compiled_args.push(val);
        }
        let metadata_args = self.values_to_metadata(&compiled_args);
        let call = self.build_call(function, &metadata_args, "flow_transition")?;
        let result = call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into());

        // v0.29.11 / H1: Fault absorption — short-circuit mailboxes of any Actor
        // handles in the from-state payload (bare handle or nested in records).
        if enters_fault {
            if let Some(first) = compiled_args.first() {
                self.emit_fault_actors_in_payload(*first, &from_type)?;
            }
        }

        // H2: recover dirty-check — see `inject_system_verbs` (keep=true body) and
        // `persistent_dirty_for_recover` in the interpreter. Codegen recover is the
        // injected transition that restores persistent shadows; mid-turn WAL dirty
        // degrade-to-reset is interp-primary (codegen has no live tx snapshot).

        Ok(result)
    }

    /// H1: Recursively short-circuit Actor handles inside a from-state payload.
    ///
    /// - Bare `i8*` / `i64` actor handle → `mimi_actor_fault`
    /// - Record/state struct → GEP each field; recurse on nested records;
    ///   fault fields whose type name is a registered actor
    ///
    /// Depth is capped at 8 (same as nested payload defaults).
    fn emit_fault_actors_in_payload(
        &mut self,
        payload: BasicValueEnum<'ctx>,
        type_name: &str,
    ) -> Result<(), CompileError> {
        self.emit_fault_actors_in_payload_depth(payload, type_name, 0)
    }

    fn emit_fault_actors_in_payload_depth(
        &mut self,
        payload: BasicValueEnum<'ctx>,
        type_name: &str,
        depth: usize,
    ) -> Result<(), CompileError> {
        if depth > 8 {
            return Ok(());
        }
        let Ok(fault_fn) = self.get_runtime_fn("mimi_actor_fault") else {
            return Ok(());
        };

        // Bare actor handle (pointer or i64-as-ptr).
        if self.actor_names.contains(type_name) {
            let handle_ptr = match payload {
                BasicValueEnum::PointerValue(pv) => pv,
                BasicValueEnum::IntValue(iv) => self
                    .builder
                    .build_int_to_ptr(
                        iv,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "actor_h_as_ptr",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?,
                _ => return Ok(()),
            };
            let meta = self.values_to_metadata(&[BasicValueEnum::PointerValue(handle_ptr)]);
            let _ = self.build_call(fault_fn, &meta, "flow_actor_fault");
            return Ok(());
        }

        // Nested record / flow state: walk fields.
        let Some(td) = self.type_defs.get(type_name).cloned() else {
            // Unknown type: try bare pointer short-circuit (legacy path).
            if let BasicValueEnum::PointerValue(pv) = payload {
                let meta = self.values_to_metadata(&[BasicValueEnum::PointerValue(pv)]);
                let _ = self.build_call(fault_fn, &meta, "flow_actor_fault");
            }
            return Ok(());
        };
        let TypeDefKind::Record(fields) = td.kind else {
            return Ok(());
        };
        if fields.is_empty() {
            return Ok(());
        }

        // Materialize struct pointer for GEP.
        let sty = match self.type_llvm.get(type_name).copied() {
            Some(BasicTypeEnum::StructType(s)) => s,
            _ => return Ok(()),
        };
        let base_ptr = match payload {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::StructValue(sv) => {
                let alloca = self.build_alloca(BasicTypeEnum::StructType(sty), "fault_walk_tmp")?;
                self.build_store(alloca, BasicValueEnum::StructValue(sv))?;
                alloca
            }
            _ => return Ok(()),
        };

        for (idx, field) in fields.iter().enumerate() {
            let field_ty_name = match &field.ty {
                Type::Name(n, _) => n.as_str(),
                _ => continue,
            };
            let gep = self
                .gep()
                .build_struct_gep(sty, base_ptr, idx as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("fault walk gep: {}", e)))?;
            let load_ty = self
                .llvm_type_for(&field.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let loaded = self.build_load(load_ty, gep, &field.name)?;
            if self.actor_names.contains(field_ty_name)
                || self.type_defs.contains_key(field_ty_name)
            {
                self.emit_fault_actors_in_payload_depth(loaded, field_ty_name, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Build a `weak<T>.upgrade()` call, returning `Option<T*>` as `{ i1, i64 }`.
    fn compile_weak_upgrade(
        &mut self,
        name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let &(alloca, _val_ty) = vars.get(name).ok_or_else(|| {
            CompileError::LlvmError(format!("weak variable '{}' not found", name))
        })?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let heap_ptr = self
            .build_load(BasicTypeEnum::PointerType(ptr_ty), alloca, "weak_heap_ptr")?
            .into_pointer_value();
        let heap_i8 = self
            .builder
            .build_pointer_cast(heap_ptr, i8_ptr, "weak_heap_i8")
            .map_err(|e| CompileError::LlvmError(format!("weak cast: {}", e)))?;
        let upgrade_fn = self.get_runtime_fn("mimi_rc_upgrade")?;
        let upgraded = self
            .build_call(
                upgrade_fn,
                &[BasicMetadataValueEnum::PointerValue(heap_i8)],
                "weak_upgrade",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_rc_upgrade returned void".to_string()))?
            .into_pointer_value();
        // upgrade() returns Option<shared T>; if Some, it owns a strong reference.
        // Register the pointer so it is released when the Option leaves scope.
        // mimi_rc_release is a no-op for null, so unconditional registration is safe.
        self.register_shared_var(upgraded);
        self.build_option_i64(upgraded, "upgrade_opt")
    }

    /// Build an `Option<i64>` (used to represent `Option<T*>`) from a raw pointer.
    fn build_option_i64(
        &self,
        ptr: PointerValue<'ctx>,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let option_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let option_alloca = self.build_alloca(option_ty, name)?;
        let disc_gep = self
            .gep()
            .build_struct_gep(option_ty, option_alloca, 0, "disc_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let payload_gep = self
            .gep()
            .build_struct_gep(option_ty, option_alloca, 1, "payload_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let is_some = self
            .builder
            .build_int_compare(IntPredicate::NE, ptr, i8_ptr.const_null(), "is_some")
            .map_err(|e| CompileError::LlvmError(format!("icmp: {}", e)))?;
        self.build_store(disc_gep, is_some)?;
        let payload = self
            .builder
            .build_ptr_to_int(ptr, self.context.i64_type(), "option_payload")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?;
        self.build_store(payload_gep, payload)?;
        self.build_load(option_ty, option_alloca, "option_val")
    }

    /// Deref a shared variable, raw pointer, or `Option<shared T>`.
    /// Returns `None` when `name` is not one of the supported deref sources.
    fn compile_shared_deref(
        &mut self,
        name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        if self.shared_var_names.contains(name) {
            let &(alloca, ty) = vars.get(name).ok_or_else(|| {
                CompileError::LlvmError(format!("shared variable '{}' not found", name))
            })?;
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let heap_ptr = self
                .build_load(
                    BasicTypeEnum::PointerType(ptr_ty),
                    alloca,
                    &format!("{}_deref_ptr", name),
                )?
                .into_pointer_value();
            let val = self.build_load(ty, heap_ptr, &format!("{}_deref", name))?;
            return Ok(Some(val));
        }

        if let Some(&(alloca, ty)) = vars.get(name) {
            if let BasicTypeEnum::PointerType(inner_ptr_ty) = ty {
                let ptr_val = self
                    .build_load(
                        BasicTypeEnum::PointerType(inner_ptr_ty),
                        alloca,
                        &format!("{}_ptr", name),
                    )?
                    .into_pointer_value();
                let i64_ty = self.context.i64_type();
                let val = self.build_load(i64_ty, ptr_val, &format!("{}_deref", name))?;
                return Ok(Some(val));
            }

            if let BasicTypeEnum::StructType(_st) = ty {
                let option_val = self.build_load(ty, alloca, &format!("{}_opt", name))?;
                let payload_int = self
                    .builder
                    .build_extract_value(option_val.into_struct_value(), 1, "payload_int")
                    .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;
                let payload_ptr = self
                    .builder
                    .build_int_to_ptr(
                        payload_int.into_int_value(),
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "payload_ptr",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?;
                let i64_ty = self.context.i64_type();
                let val = self.build_load(i64_ty, payload_ptr, &format!("{}_deref", name))?;
                return Ok(Some(val));
            }
        }

        Ok(None)
    }

    /// Compile a method call that takes `self` as its first argument.
    /// Struct `self` values are converted to a pointer, re-using the variable's
    /// alloca when available so that mutable actor fields can be mutated in place.
    fn compile_self_method_call(
        &mut self,
        obj: &Expr,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        function: inkwell::values::FunctionValue<'ctx>,
        call_name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let obj_val = self.compile_expr(obj, vars)?;
        let obj_val = self.ensure_self_pointer(obj_val, obj, vars)?;
        let mut compiled_args = Vec::new();
        compiled_args.push(obj_val);
        for (i, arg) in args.iter().enumerate() {
            let val = self.compile_expr(arg, vars)?;
            // A1: adjust integer arg width to match the function's declared param type.
            // Param 0 is self; user args start at index 1.
            let param_idx = i + 1;
            let val = if param_idx < function.count_params() as usize {
                let param_ty = function
                    .get_nth_param(param_idx as u32)
                    .map(|p| p.get_type())
                    .unwrap_or(val.get_type());
                self.adjust_int_value_width(val, param_ty, call_name)?
            } else {
                val
            };
            compiled_args.push(val);
        }
        let metadata_args = self.values_to_metadata(&compiled_args);
        let call = self.build_call(function, &metadata_args, call_name)?;
        Ok(call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into()))
    }

    /// Ensure a `self` argument is passed as a pointer when it is a struct value.
    /// For string ({ptr, i64}) structs, extract the data pointer instead of
    /// passing the struct address — string builtins expect char*, not {ptr,i64}*.
    fn ensure_self_pointer(
        &mut self,
        obj_val: BasicValueEnum<'ctx>,
        obj: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match obj_val {
            BasicValueEnum::StructValue(sv) => {
                let struct_ty = sv.get_type();
                // String struct is {ptr, i64} — extract data pointer for builtins.
                if self.is_string_struct_type(struct_ty) {
                    let data_ptr = self
                        .builder
                        .build_extract_value(sv, 0, "self_str_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("extract self str: {}", e)))?;
                    return Ok(data_ptr);
                }
                if let Expr::Ident(name) = obj {
                    if let Some(&(alloca, _)) = vars.get(name.as_str()) {
                        Ok(alloca.into())
                    } else {
                        let tmp = self.build_alloca(struct_ty, "self_tmp")?;
                        self.build_store(tmp, obj_val)?;
                        Ok(tmp.into())
                    }
                } else {
                    let tmp = self.build_alloca(struct_ty, "self_tmp")?;
                    self.build_store(tmp, obj_val)?;
                    Ok(tmp.into())
                }
            }
            other => Ok(other),
        }
    }

    /// Convert a slice of `BasicValueEnum` values into LLVM metadata arguments.
    fn values_to_metadata(
        &self,
        values: &[BasicValueEnum<'ctx>],
    ) -> Vec<BasicMetadataValueEnum<'ctx>> {
        values
            .iter()
            .map(|v| match v {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                BasicValueEnum::ScalableVectorValue(_) => {
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false))
                }
            })
            .collect()
    }

    /// True vtable indirect dispatch for `dyn Trait` objects.
    fn compile_dyn_trait_call(
        &mut self,
        obj: &Expr,
        method_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        trait_name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let method_idx = self
            .trait_defs
            .get(trait_name)
            .and_then(|tdef| tdef.methods.iter().position(|m| m.name == *method_name));
        let idx = match method_idx {
            Some(idx) => idx,
            None => {
                return Err(format!(
                    "[E0708] cannot dispatch method '{}' on dyn {}",
                    method_name, trait_name
                )
                .into());
            }
        };

        let vtable_ty = self
            .vtable_types
            .get(trait_name)
            .copied()
            .ok_or("no vtable type for trait")?;

        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let fat_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::PointerType(i8_ptr_ty),
            ],
            false,
        );

        let obj_val = self.compile_expr(obj, vars)?;
        let fat_ptr = match obj_val {
            BasicValueEnum::StructValue(_) => {
                let alloca = self.build_alloca(BasicTypeEnum::StructType(fat_ty), "fat_tmp")?;
                self.build_store(alloca, obj_val)?;
                alloca
            }
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("dyn Trait value must be a struct or pointer".into()),
        };

        let vtable_gep = self
            .gep()
            .build_struct_gep(BasicTypeEnum::StructType(fat_ty), fat_ptr, 1, "vtable_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let vtable_ptr = self
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                vtable_gep,
                "vtable_ptr",
            )?
            .into_pointer_value();

        let method_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(vtable_ty),
                vtable_ptr,
                idx as u32,
                "method_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let fn_ptr = self
            .build_load(BasicTypeEnum::PointerType(i8_ptr_ty), method_gep, "fn_ptr")?
            .into_pointer_value();

        // CG-H7: guard against null vtable entry (unimplemented trait method).
        // Instead of calling null (SIGSEGV), emit trap + unreachable.
        let is_null = self
            .builder
            .build_is_null(fn_ptr, "fn_null")
            .map_err(|e| CompileError::LlvmError(format!("null check: {}", e)))?;
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for dyn trait".to_string())?;
        let null_bb = self.context.append_basic_block(function, "null_method");
        let call_bb = self.context.append_basic_block(function, "call_method");
        self.builder
            .build_conditional_branch(is_null, null_bb, call_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // null path: mimi_runtime_abort (noreturn) + unreachable (CG-H7)
        self.builder.position_at_end(null_bb);
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr(
                "unimplemented trait method (null vtable entry)",
                "null_vtable_msg",
            )
            .map_err(|e| CompileError::LlvmError(format!("global string: {}", e)))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
            "abort_unimplemented",
        )?;
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable: {}", e)))?;

        // call path: proceed with dispatch
        self.builder.position_at_end(call_bb);

        let data_gep = self
            .gep()
            .build_struct_gep(BasicTypeEnum::StructType(fat_ty), fat_ptr, 0, "data_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_ptr =
            self.build_load(BasicTypeEnum::PointerType(i8_ptr_ty), data_gep, "data_ptr")?;

        let fn_sig = self.find_trait_method_signature(trait_name, method_name);
        if let Some((fn_val, _)) = fn_sig {
            let fn_llvm = fn_val.into_function_value();
            let fn_type = fn_llvm.get_type();
            let fn_ptr_cast = self
                .builder
                .build_pointer_cast(
                    fn_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "fn_cast",
                )
                .map_err(|e| CompileError::LlvmError(format!("cast error: {}", e)))?;

            let mut compiled_args = Vec::new();
            compiled_args.push(data_ptr);
            for arg in args {
                compiled_args.push(self.compile_expr(arg, vars)?);
            }
            let metadata_args = self.values_to_metadata(&compiled_args);
            let call = self
                .builder
                .build_indirect_call(fn_type, fn_ptr_cast, &metadata_args, "dyn_call")
                .map_err(|e| CompileError::LlvmError(format!("dyn indirect call error: {}", e)))?;
            return Ok(call_try_basic_value(&call)
                .unwrap_or(self.context.i64_type().const_int(0, false).into()));
        }

        Err(format!(
            "[E0708] cannot dispatch method '{}' on dyn {}",
            method_name, trait_name
        )
        .into())
    }

    /// Find any mangled implementation of a trait method to extract its LLVM signature.
    fn find_trait_method_signature(
        &self,
        trait_name: &str,
        method_name: &str,
    ) -> Option<(inkwell::values::AnyValueEnum<'ctx>, String)> {
        for (tn, timpls) in &self.type_impls {
            if let Some(methods) = timpls.get(trait_name) {
                if methods.iter().any(|m| m.name == *method_name) {
                    let mangled = format!("{}__{}__{}", tn, trait_name, method_name);
                    if let Some(f) = self.module.get_function(&mangled) {
                        return Some((inkwell::values::AnyValueEnum::FunctionValue(f), mangled));
                    }
                }
            }
        }
        None
    }

    /// Dispatch a method on an `impl Trait` object by looking up a concrete impl.
    fn compile_impl_trait_call(
        &mut self,
        obj: &Expr,
        method_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        trait_name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        for (type_name, trait_impls) in &self.type_impls {
            if let Some(methods) = trait_impls.get(trait_name) {
                if methods.iter().any(|m| m.name == *method_name) {
                    let mangled = format!("{}__{}__{}", type_name, trait_name, method_name);
                    if let Some(function) = self.module.get_function(&mangled) {
                        return self.compile_self_method_call(
                            obj,
                            args,
                            vars,
                            function,
                            "impl_trait_call",
                        );
                    }
                }
            }
        }
        Err(format!(
            "[E0708] cannot dispatch method '{}' on impl {}",
            method_name, trait_name
        )
        .into())
    }

    /// Compile an enum constructor call: `{Type}_{Variant}(args)`.
    fn compile_enum_constructor_call(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        function: inkwell::values::FunctionValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let mut compiled_args = Vec::new();
        for arg in args {
            compiled_args.push(self.compile_expr(arg, vars)?);
        }
        let call_args = CodeGenerator::maybe_pack_enum_ctor_args(self, &compiled_args, function)?;
        let metadata_args = self.values_to_metadata(&call_args);
        let call = self.build_call(function, &metadata_args, "enum_ctor")?;
        Ok(call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into()))
    }

    /// Built-in method dispatch for `Set<T>` values.
    fn compile_set_method(
        &mut self,
        obj: &Expr,
        method_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let obj_val = self.compile_expr(obj, vars)?;
        let set_handle = match obj_val {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::PointerValue(pv) => self
                .builder
                .build_ptr_to_int(pv, self.context.i64_type(), "set_handle")
                .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?,
            _ => return Err(CompileError::Generic("expected set handle (i64)".into())),
        };
        let i64_ty = self.context.i64_type();

        match method_name {
            "size" | "len" => {
                let func = self.get_runtime_fn("mimi_set_size")?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::IntValue(set_handle)],
                    "set_size",
                )?;
                Ok(self.expect_basic_value(&result, "set_size")?)
            }
            "is_empty" => {
                let func = self.get_runtime_fn("mimi_set_size")?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::IntValue(set_handle)],
                    "set_size",
                )?;
                let size = self
                    .expect_basic_value(&result, "set_size")?
                    .into_int_value();
                let zero = i64_ty.const_int(0, false);
                let is_empty = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, size, zero, "is_empty")
                    .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                Ok(BasicValueEnum::IntValue(is_empty))
            }
            "contains" => {
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount(
                        "set.contains expects 1 argument".into(),
                    ));
                }
                let arg = self.compile_expr(&args[0], vars)?;
                let arg_handle = self.any_value_to_handle(arg)?;
                let func = self.get_runtime_fn("mimi_set_contains")?;
                let result = self.build_call(
                    func,
                    &[
                        BasicMetadataValueEnum::IntValue(set_handle),
                        BasicMetadataValueEnum::IntValue(arg_handle),
                    ],
                    "set_contains",
                )?;
                let iv = self
                    .expect_basic_value(&result, "set_contains")?
                    .into_int_value();
                let one = i64_ty.const_int(1, false);
                let bv = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, iv, one, "to_bool")
                    .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                Ok(BasicValueEnum::IntValue(bv))
            }
            "insert" => {
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount(
                        "set.insert expects 1 argument".into(),
                    ));
                }
                let arg = self.compile_expr(&args[0], vars)?;
                let arg_handle = self.any_value_to_handle(arg)?;
                let func = self.get_runtime_fn("mimi_set_insert")?;
                self.build_call(
                    func,
                    &[
                        BasicMetadataValueEnum::IntValue(set_handle),
                        BasicMetadataValueEnum::IntValue(arg_handle),
                    ],
                    "set_insert",
                )?;
                Ok(BasicValueEnum::IntValue(set_handle))
            }
            "remove" => {
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount(
                        "set.remove expects 1 argument".into(),
                    ));
                }
                let arg = self.compile_expr(&args[0], vars)?;
                let arg_handle = self.any_value_to_handle(arg)?;
                let func = self.get_runtime_fn("mimi_set_remove")?;
                self.build_call(
                    func,
                    &[
                        BasicMetadataValueEnum::IntValue(set_handle),
                        BasicMetadataValueEnum::IntValue(arg_handle),
                    ],
                    "set_remove",
                )?;
                Ok(BasicValueEnum::IntValue(set_handle))
            }
            "to_list" => {
                let out_len_alloca = self.build_alloca(i64_ty, "out_len")?;
                let func = self.get_runtime_fn("mimi_set_to_list")?;
                let result = self.build_call(
                    func,
                    &[
                        BasicMetadataValueEnum::IntValue(set_handle),
                        BasicMetadataValueEnum::PointerValue(out_len_alloca),
                    ],
                    "set_to_list",
                )?;
                let data_ptr = self
                    .expect_basic_value(&result, "set_to_list")?
                    .into_pointer_value();
                let len_val = self
                    .build_load(i64_ty, out_len_alloca, "list_len")?
                    .into_int_value();
                let list_ty = self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(i64_ty),
                        BasicTypeEnum::PointerType(
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                        ),
                    ],
                    false,
                );
                let alloca = self.build_alloca(list_ty, "list_ret")?;
                let len_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 0, "len_gep")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.build_store(len_gep, len_val)?;
                let data_gep = self
                    .gep()
                    .build_struct_gep(list_ty, alloca, 1, "data_gep")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                let data_i64 = self
                    .builder
                    .build_ptr_to_int(data_ptr, i64_ty, "data_ptr_int")
                    .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?;
                self.build_store(data_gep, data_i64)?;
                self.build_load(list_ty, alloca, "list_ret_val")
            }
            _ => Err(CompileError::Generic(format!(
                "Set has no method '{}'",
                method_name
            ))),
        }
    }

    pub(in crate::codegen) fn compile_turbofish_expr(
        &mut self,
        name: &str,
        type_args: &[Type],
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Special case: from_json::<T>(s) — typed JSON deserialization
        if name == "from_json" && !type_args.is_empty() {
            return self.compile_from_json_turbofish(type_args, args, vars);
        }

        // Monomorphized call: func::<Type>(args)
        // Build type_map from explicit type args
        let func = self.find_func_def(name)?;
        if func.generics.len() != type_args.len() {
            return Err(CompileError::Generic(format!(
                "[E0720] turbofish for '{}' expects {} type args, got {}",
                name,
                func.generics.len(),
                type_args.len()
            )));
        }
        let mut turbo_map: HashMap<String, crate::ast::Type> = HashMap::new();
        for (gp, ta) in func.generics.iter().zip(type_args.iter()) {
            turbo_map.insert(gp.name.clone(), ta.clone());
        }
        // Merge with current type_map (for nested generics)
        let mut merged_map = self.type_map.clone();
        merged_map.extend(turbo_map);
        let mangled = Self::mangle_name(name, &merged_map);
        // Compile the specialized version if not yet compiled
        if self.module.get_function(&mangled).is_none() {
            self.compile_generic_func(&func, &merged_map)
                .map_err(|e| CompileError::Generic(e.to_string()))?;
        }
        // Call the mangled function
        self.compile_call_mangled(&mangled, args, vars)
    }

    /// Typed `from_json::<T>(s)` deserialization.
    /// Decode a JSON C string pointer into an internal value of `ty`.
    /// Used by turbofish `from_json::<T>` and by export-wrapper JSON ABI.
    pub(in crate::codegen) fn compile_from_json_raw(
        &mut self,
        ty: &Type,
        raw_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        self.compile_from_json_turbofish_with_ptr(std::slice::from_ref(ty), raw_ptr)
    }

    fn compile_from_json_turbofish(
        &mut self,
        type_args: &[Type],
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "from_json::<T> expects 1 argument".into(),
            ));
        }
        let json_val = self.compile_expr(&args[0], vars)?;
        let raw_ptr = match &json_val {
            BasicValueEnum::PointerValue(pv) => *pv,
            BasicValueEnum::StructValue(sv) => self
                .builder
                .build_extract_value(*sv, 0, "json_str_ptr")
                .map_err(|e| CompileError::LlvmError(format!("extract json string: {}", e)))?
                .into_pointer_value(),
            _ => return Err("from_json argument must be a string".into()),
        };
        self.compile_from_json_turbofish_with_ptr(type_args, raw_ptr)
    }

    fn compile_from_json_turbofish_with_ptr(
        &mut self,
        type_args: &[Type],
        raw_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match &type_args[0] {
            Type::Name(n, _) if n == "i32" || n == "i64" => {
                let func = self.get_runtime_fn("mimi_json_as_i64")?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "json_as_i64",
                )?;
                let iv = self
                    .expect_basic_value(&result, "json_as_i64")?
                    .into_int_value();
                Ok(BasicValueEnum::IntValue(iv))
            }
            Type::Name(n, _) if n == "f32" || n == "f64" => {
                let func = self.get_runtime_fn("mimi_json_as_f64")?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "json_as_f64",
                )?;
                let fv = self
                    .expect_basic_value(&result, "json_as_f64")?
                    .into_float_value();
                Ok(BasicValueEnum::FloatValue(fv))
            }
            Type::Name(n, _) if n == "bool" => {
                let func = self.get_runtime_fn("mimi_json_as_bool")?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "json_as_bool",
                )?;
                let iv = self
                    .expect_basic_value(&result, "json_as_bool")?
                    .into_int_value();
                let one = self.context.i64_type().const_int(1, false);
                let bv = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, iv, one, "to_bool")
                    .map_err(|e| CompileError::LlvmError(format!("to_bool: {}", e)))?;
                Ok(BasicValueEnum::IntValue(bv))
            }
            Type::Name(n, _) if n == "string" => {
                let func = self.get_runtime_fn("mimi_from_json")?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "from_json_string",
                )?;
                let pv = self
                    .expect_basic_value(&result, "from_json_string")?
                    .into_pointer_value();
                let strlen_fn = self.get_runtime_fn("strlen")?;
                let len = self
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(pv)],
                        "strlen",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                self.build_string_struct(pv, len)
            }
            Type::Name(n, type_params) if n == "List" => {
                // from_json::<List<T>>(s): deserialize JSON array into Mimi list
                if type_params.len() != 1 {
                    return Err(CompileError::Generic(
                        "List expects 1 type parameter".into(),
                    ));
                }
                let inner_ty = &type_params[0];
                let i64_ty = self.context.i64_type();
                let json_arr_len_fn = self.get_runtime_fn("json_array_length")?;
                let json_get_elem_fn = self.get_runtime_fn("json_get_element")?;

                // Get array length
                let len_val = self
                    .build_call(
                        json_arr_len_fn,
                        &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                        "json_arr_len",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("json_array_length returned void")?
                    .into_int_value();

                // Allocate data buffer: len * 8 bytes
                let sizeof_i64 = i64_ty.const_int(8, false);
                let alloc_size = self
                    .builder
                    .build_int_mul(len_val, sizeof_i64, "alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("mul: {}", e)))?;
                // B4: OOM-safe list data buffer for from_json arrays.
                let data_ptr = self.malloc_or_abort(alloc_size, "malloc_data")?;

                // Cast to i64* for element access
                let data_i64_ptr = self.build_pointer_cast(
                    data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "data_i64",
                )?;

                // Build list struct {i64 len, i8* data}
                let list_alloca = self.alloc_list_result(len_val, data_ptr)?;

                // Determine if list elements are heap-allocated pointers (string or record)
                // that need individual freeing at scope exit.
                let needs_element_free = matches!(
                    inner_ty,
                    Type::Name(n, _)
                        if *n == "string"
                            || self.type_defs.get(n).is_some_and(|td|
                                matches!(td.kind, crate::ast::TypeDefKind::Record(_)))
                );

                // Build loop: for i = 0; i < len; i++
                let function = self
                    .current_function()
                    .ok_or_else(|| "codegen: no current function".to_string())?;
                let loop_bb = self.context.append_basic_block(function, "json_list_loop");
                let body_bb = self.context.append_basic_block(function, "json_list_body");
                let done_bb = self.context.append_basic_block(function, "json_list_done");
                let idx_alloca = self.build_alloca(i64_ty, "idx")?;
                self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
                self.build_br(loop_bb)?;
                self.builder.position_at_end(loop_bb);
                let idx = self
                    .build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")?
                    .into_int_value();
                let cond = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, idx, len_val, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                self.build_cond_br(cond, body_bb, done_bb)?;
                self.builder.position_at_end(body_bb);

                // Get element JSON fragment
                let elem_json = self
                    .build_call(
                        json_get_elem_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(raw_ptr),
                            BasicMetadataValueEnum::IntValue(idx),
                        ],
                        "elem_json",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("json_get_element returned void")?
                    .into_pointer_value();

                // Parse element and store based on inner type
                let elem_i64 = match inner_ty {
                    Type::Name(n, _) if n == "i32" || n == "i64" => {
                        let parser = self.get_runtime_fn("mimi_json_as_i64")?;
                        let val = self
                            .build_call(
                                parser,
                                &[BasicMetadataValueEnum::PointerValue(elem_json)],
                                "elem_val",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_json_as_i64 returned void")?;
                        val.into_int_value()
                    }
                    Type::Name(n, _) if n == "string" => {
                        // json_get_element already returns a heap-allocated C-string.
                        // List<string> stores elements as bare C-string pointers (i8*)
                        // in the data array (ptrtoint'd to i64). This matches the
                        // convention used by mimi_list_free.
                        self.build_ptr_to_int(elem_json, i64_ty, "elem_as_i64")?
                    }
                    Type::Name(n, _) if n == "f32" || n == "f64" => {
                        let parser = self.get_runtime_fn("mimi_json_as_f64")?;
                        let val = self
                            .build_call(
                                parser,
                                &[BasicMetadataValueEnum::PointerValue(elem_json)],
                                "elem_val",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_json_as_f64 returned void")?
                            .into_float_value();
                        // bitcast f64 → i64 for list storage
                        self.build_bit_cast(
                            BasicValueEnum::FloatValue(val),
                            BasicTypeEnum::IntType(i64_ty),
                            "f64_to_i64",
                        )?
                        .into_int_value()
                    }
                    Type::Name(n, _) if n == "bool" => {
                        let parser = self.get_runtime_fn("mimi_json_as_bool")?;
                        let val = self
                            .build_call(
                                parser,
                                &[BasicMetadataValueEnum::PointerValue(elem_json)],
                                "elem_val",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_json_as_bool returned void")?
                            .into_int_value();
                        val
                    }
                    Type::Name(n, args) if n == "List" => {
                        // Nested List: recursive from_json on element JSON fragment.
                        let nested_ty = Type::Name("List".into(), args.clone());
                        let nested =
                            self.compile_from_json_turbofish_with_ptr(&[nested_ty], elem_json)?;
                        match nested {
                            BasicValueEnum::StructValue(sv) => {
                                let list_ty = self.list_struct_type();
                                let size =
                                    self.llvm_type_size_bytes(BasicTypeEnum::StructType(list_ty));
                                let heap = self.malloc_or_abort(
                                    i64_ty.const_int(size, false),
                                    "list_list_heap",
                                )?;
                                let i8_ptr =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let typed = self
                                    .build_bit_cast(
                                        heap.into(),
                                        BasicTypeEnum::PointerType(i8_ptr),
                                        "list_list_ptr",
                                    )?
                                    .into_pointer_value();
                                self.build_store(typed, sv)?;
                                self.build_ptr_to_int(typed, i64_ty, "list_list_as_i64")?
                            }
                            BasicValueEnum::PointerValue(pv) => {
                                self.build_ptr_to_int(pv, i64_ty, "list_list_ptr_i64")?
                            }
                            BasicValueEnum::IntValue(iv) => iv,
                            other => {
                                return Err(CompileError::Generic(format!(
                                    "from_json List List: unexpected {:?}",
                                    other.get_type()
                                )));
                            }
                        }
                    }
                    Type::Name(n, args) if n == "Map" => {
                        let val_is_string = args
                            .get(1)
                            .map(|t| matches!(t, Type::Name(tn, _) if tn == "string"))
                            .unwrap_or(false);
                        let val_is_float = args
                            .get(1)
                            .map(|t| matches!(t, Type::Name(tn, _) if tn == "f32" || tn == "f64"))
                            .unwrap_or(false);
                        let fn_name = if val_is_string {
                            "mimi_map_from_json_string"
                        } else if val_is_float {
                            "mimi_map_from_json_f64"
                        } else {
                            "mimi_map_from_json_i64"
                        };
                        let func = self.get_runtime_fn(fn_name)?;
                        let r = self.build_call(
                            func,
                            &[BasicMetadataValueEnum::PointerValue(elem_json)],
                            "list_map_from_json",
                        )?;
                        self.expect_basic_value(&r, fn_name)?.into_int_value()
                    }
                    Type::Name(n, args) if n == "Set" => {
                        let elem_is_string = args
                            .first()
                            .map(|t| matches!(t, Type::Name(tn, _) if tn == "string"))
                            .unwrap_or(false);
                        let elem_is_float = args
                            .first()
                            .map(|t| matches!(t, Type::Name(tn, _) if tn == "f32" || tn == "f64"))
                            .unwrap_or(false);
                        let fn_name = if elem_is_string {
                            "mimi_set_from_json_string"
                        } else if elem_is_float {
                            "mimi_set_from_json_f64"
                        } else {
                            "mimi_set_from_json_i64"
                        };
                        let func = self.get_runtime_fn(fn_name)?;
                        let r = self.build_call(
                            func,
                            &[BasicMetadataValueEnum::PointerValue(elem_json)],
                            "list_set_from_json",
                        )?;
                        self.expect_basic_value(&r, fn_name)?.into_int_value()
                    }
                    Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                        // List of Option: store ptrtoint of Option {i1,i64} on heap.
                        let json_as_i64_fn = self.module.get_function("mimi_json_as_i64");
                        let json_as_f64_fn = self.module.get_function("mimi_json_as_f64");
                        let json_as_bool_fn = self.module.get_function("mimi_json_as_bool");
                        let opt_val = self.compile_json_option_field(
                            "List",
                            &args[0],
                            elem_json,
                            json_as_i64_fn,
                            json_as_f64_fn,
                            json_as_bool_fn,
                        )?;
                        match opt_val {
                            BasicValueEnum::StructValue(sv) => {
                                let sty = sv.get_type();
                                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
                                let heap = self.malloc_or_abort(
                                    i64_ty.const_int(size, false),
                                    "list_opt_heap",
                                )?;
                                let i8_ptr =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let typed = self
                                    .build_bit_cast(
                                        heap.into(),
                                        BasicTypeEnum::PointerType(i8_ptr),
                                        "list_opt_ptr",
                                    )?
                                    .into_pointer_value();
                                self.build_store(typed, sv)?;
                                self.build_ptr_to_int(typed, i64_ty, "list_opt_as_i64")?
                            }
                            BasicValueEnum::IntValue(iv) => iv,
                            other => {
                                return Err(CompileError::Generic(format!(
                                    "from_json List Option: unexpected {:?}",
                                    other.get_type()
                                )));
                            }
                        }
                    }
                    Type::Name(n, args) if n == "Result" && !args.is_empty() => {
                        // List of Result: bare JSON value → Ok(T) via scalar Ok path.
                        let ok_val = self.compile_from_json_scalar_ok(&args[0], elem_json)?;
                        let res_val = self.compile_constructor("Ok", vec![ok_val])?;
                        match res_val {
                            BasicValueEnum::StructValue(sv) => {
                                let sty = sv.get_type();
                                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
                                let heap = self.malloc_or_abort(
                                    i64_ty.const_int(size, false),
                                    "list_res_heap",
                                )?;
                                let i8_ptr =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let typed = self
                                    .build_bit_cast(
                                        heap.into(),
                                        BasicTypeEnum::PointerType(i8_ptr),
                                        "list_res_ptr",
                                    )?
                                    .into_pointer_value();
                                self.build_store(typed, sv)?;
                                self.build_ptr_to_int(typed, i64_ty, "list_res_as_i64")?
                            }
                            BasicValueEnum::IntValue(iv) => iv,
                            other => {
                                return Err(CompileError::Generic(format!(
                                    "from_json List Result: unexpected {:?}",
                                    other.get_type()
                                )));
                            }
                        }
                    }
                    Type::Option(inner) => {
                        let json_as_i64_fn = self.module.get_function("mimi_json_as_i64");
                        let json_as_f64_fn = self.module.get_function("mimi_json_as_f64");
                        let json_as_bool_fn = self.module.get_function("mimi_json_as_bool");
                        let opt_val = self.compile_json_option_field(
                            "List",
                            inner,
                            elem_json,
                            json_as_i64_fn,
                            json_as_f64_fn,
                            json_as_bool_fn,
                        )?;
                        match opt_val {
                            BasicValueEnum::StructValue(sv) => {
                                let sty = sv.get_type();
                                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
                                let heap = self.malloc_or_abort(
                                    i64_ty.const_int(size, false),
                                    "list_opt_heap2",
                                )?;
                                let i8_ptr =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let typed = self
                                    .build_bit_cast(
                                        heap.into(),
                                        BasicTypeEnum::PointerType(i8_ptr),
                                        "list_opt_ptr2",
                                    )?
                                    .into_pointer_value();
                                self.build_store(typed, sv)?;
                                self.build_ptr_to_int(typed, i64_ty, "list_opt_as_i64_2")?
                            }
                            BasicValueEnum::IntValue(iv) => iv,
                            other => {
                                return Err(CompileError::Generic(format!(
                                    "from_json List Option: unexpected {:?}",
                                    other.get_type()
                                )));
                            }
                        }
                    }
                    Type::Name(inner_n, _) => {
                        // Record type: deserialize each JSON element into a heap-allocated struct
                        let fields_opt =
                            self.type_defs.get(inner_n).and_then(|td| match &td.kind {
                                crate::ast::TypeDefKind::Record(fields) => Some(fields.clone()),
                                _ => None,
                            });
                        if let Some(fields) = fields_opt {
                            let llvm_ty = *self.type_llvm.get(inner_n).ok_or_else(|| {
                                CompileError::Generic(format!("type '{}' not registered", inner_n))
                            })?;
                            let BasicTypeEnum::StructType(sty) = llvm_ty else {
                                return Err(CompileError::Generic(format!(
                                    "type '{}' is not a struct",
                                    inner_n
                                )));
                            };
                            let struct_size =
                                self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
                            let size_val = i64_ty.const_int(struct_size, false);
                            // B4: OOM-safe heap record for from_json object decode.
                            let heap_ptr = self.malloc_or_abort(size_val, "malloc_record")?;
                            let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                            let typed_ptr = self
                                .build_bit_cast(
                                    heap_ptr.into(),
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    "typed_ptr",
                                )?
                                .into_pointer_value();
                            let json_get_fn = self.get_runtime_fn("json_get_string")?;
                            let json_as_i64_fn = self.module.get_function("mimi_json_as_i64");
                            let json_as_f64_fn = self.module.get_function("mimi_json_as_f64");
                            let json_as_bool_fn = self.module.get_function("mimi_json_as_bool");
                            for (i, field) in fields.iter().enumerate() {
                                let key_global = self
                                    .builder
                                    .build_global_string_ptr(&field.name, "field_key")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("global str: {}", e))
                                    })?;
                                let raw_val = self
                                    .build_call(
                                        json_get_fn,
                                        &[
                                            BasicMetadataValueEnum::PointerValue(elem_json),
                                            BasicMetadataValueEnum::PointerValue(
                                                key_global.as_pointer_value(),
                                            ),
                                        ],
                                        "json_field",
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("json_get_string returned void")?
                                    .into_pointer_value();
                                let gep = self
                                    .gep()
                                    .build_struct_gep(sty, typed_ptr, i as u32, &field.name)
                                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                                let field_val = self.compile_json_scalar_field(
                                    inner_n,
                                    field,
                                    raw_val,
                                    json_as_i64_fn,
                                    json_as_f64_fn,
                                    json_as_bool_fn,
                                )?;
                                self.build_store(gep, field_val)?;
                            }
                            self.build_ptr_to_int(typed_ptr, i64_ty, "record_as_i64")?
                        } else {
                            return Err(CompileError::Generic(format!(
                                "from_json::<List<T>>: type '{}' is not a record",
                                inner_n
                            )));
                        }
                    }
                    _ => {
                        return Err(CompileError::Generic(format!(
                            "from_json::<List<T>>: unsupported element type {:?}",
                            inner_ty
                        )))
                    }
                };

                // Store i64 to data[i]
                let elem_gep = self
                    .gep()
                    .build_in_bounds_gep(i64_ty, data_i64_ptr, &[idx], "elem_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.build_store(elem_gep, elem_i64)?;

                // Increment and loop
                let next = self
                    .builder
                    .build_int_add(idx, i64_ty.const_int(1, false), "next")
                    .map_err(|e| CompileError::LlvmError(format!("add: {}", e)))?;
                self.build_store(idx_alloca, next)?;
                self.build_br(loop_bb)?;
                self.builder.position_at_end(done_bb);

                // Register heap cleanup if elements need individual freeing.
                // v0.28.29 fix for mimichat gap #2: do NOT register the temporary
                // `list_alloca` here. The caller stores the returned list struct into
                // its own variable alloca (`%l`), and in-place mutations like `push(l, ...)`
                // rewrite `%l.data` (via the caller-provided alloca path). Registering
                // the temporary means the scope-exit cleanup reads stale `list_alloca.data`
                // which has been freed by an intervening realloc — causing double-free /
                // SIGSEGV. The list elements are intentionally leaked at scope exit; the
                // process reclaim at termination keeps the codegen correct without a
                // crash. This trades memory hygiene for the ability to mutate lists
                // returned by `from_json`.
                let _ = needs_element_free;

                // Load and return list struct
                let list_ty = self.list_struct_type();
                self.build_load(BasicTypeEnum::StructType(list_ty), list_alloca, "list")
            }
            Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                let json_as_i64_fn = self.module.get_function("mimi_json_as_i64");
                let json_as_f64_fn = self.module.get_function("mimi_json_as_f64");
                let json_as_bool_fn = self.module.get_function("mimi_json_as_bool");
                self.compile_json_option_field(
                    "Option",
                    &args[0],
                    raw_ptr,
                    json_as_i64_fn,
                    json_as_f64_fn,
                    json_as_bool_fn,
                )
            }
            Type::Option(inner) => {
                let json_as_i64_fn = self.module.get_function("mimi_json_as_i64");
                let json_as_f64_fn = self.module.get_function("mimi_json_as_f64");
                let json_as_bool_fn = self.module.get_function("mimi_json_as_bool");
                self.compile_json_option_field(
                    "Option",
                    inner,
                    raw_ptr,
                    json_as_i64_fn,
                    json_as_f64_fn,
                    json_as_bool_fn,
                )
            }
            Type::Name(n, args) if n == "Map" => {
                // Map<string, i32|i64|bool|f32|f64|string> from JSON object.
                let val_ty = args.get(1);
                let val_is_int = val_ty
                    .map(|t| {
                        matches!(
                            t,
                            Type::Name(tn, _) if tn == "i32" || tn == "i64" || tn == "bool"
                        )
                    })
                    .unwrap_or(true);
                let val_is_float = val_ty
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "f32" || tn == "f64"))
                    .unwrap_or(false);
                let val_is_string = val_ty
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "string"))
                    .unwrap_or(false);
                if !val_is_int && !val_is_float && !val_is_string {
                    return Err(CompileError::Generic(
                        "from_json::<Map>: only Map<string, i32|i64|bool|f32|f64|string> is supported in codegen"
                            .into(),
                    ));
                }
                let fn_name = if val_is_string {
                    "mimi_map_from_json_string"
                } else if val_is_float {
                    "mimi_map_from_json_f64"
                } else {
                    "mimi_map_from_json_i64"
                };
                let func = self.get_runtime_fn(fn_name)?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "map_from_json",
                )?;
                Ok(self.expect_basic_value(&result, fn_name)?.into())
            }
            Type::Name(n, args) if n == "Set" => {
                let elem_ty = args.first();
                let elem_is_int = elem_ty
                    .map(|t| {
                        matches!(
                            t,
                            Type::Name(tn, _) if tn == "i32" || tn == "i64" || tn == "bool"
                        )
                    })
                    .unwrap_or(true);
                let elem_is_float = elem_ty
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "f32" || tn == "f64"))
                    .unwrap_or(false);
                let elem_is_string = elem_ty
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "string"))
                    .unwrap_or(false);
                if !elem_is_int && !elem_is_float && !elem_is_string {
                    return Err(CompileError::Generic(
                        "from_json::<Set>: only Set<i32|i64|bool|f32|f64|string> is supported in codegen"
                            .into(),
                    ));
                }
                // bool elements use i64 path (true→1, false→0) via mimi_json_as_i64.
                let fn_name = if elem_is_string {
                    "mimi_set_from_json_string"
                } else if elem_is_float {
                    "mimi_set_from_json_f64"
                } else {
                    "mimi_set_from_json_i64"
                };
                let func = self.get_runtime_fn(fn_name)?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "set_from_json",
                )?;
                Ok(self.expect_basic_value(&result, fn_name)?.into())
            }
            Type::Name(n, args) if n == "Result" && !args.is_empty() => {
                let ok_val = self.compile_from_json_scalar_ok(&args[0], raw_ptr)?;
                self.compile_constructor("Ok", vec![ok_val])
            }
            Type::Result(ok, _) => {
                let ok_val = self.compile_from_json_scalar_ok(ok, raw_ptr)?;
                self.compile_constructor("Ok", vec![ok_val])
            }
            Type::Name(type_name, _) => {
                // Record type: deserialize JSON object into struct fields
                let fields_opt = self.type_defs.get(type_name).and_then(|td| {
                    if let crate::ast::TypeDefKind::Record(fields) = &td.kind {
                        Some(fields.clone())
                    } else {
                        None
                    }
                });
                if let Some(fields) = fields_opt {
                    return self.compile_from_json_record(type_name, &fields, raw_ptr);
                }
                Err(CompileError::Generic(format!(
                    "from_json::<{}>: unsupported type (supported: Record, Map/Set of i32|i64, \
                     Option/Result of scalars, nested Record fields)",
                    type_name
                )))
            }
            _ => Err(CompileError::Generic(format!(
                "from_json::<{:?}> codegen not yet implemented \
                 (supported: scalars, Option, Result Ok-wrap, Map/Set i64, Record)",
                type_args[0]
            ))),
        }
    }

    /// Parse JSON into a scalar Ok payload for `from_json::<Result<T,_>>`.
    fn compile_from_json_scalar_ok(
        &mut self,
        ok_ty: &Type,
        raw_ptr: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match ok_ty {
            Type::Name(tn, _) if tn == "i32" || tn == "i64" => {
                let func = self.get_runtime_fn("mimi_json_as_i64")?;
                let r = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "json_as_i64",
                )?;
                Ok(BasicValueEnum::IntValue(
                    self.expect_basic_value(&r, "json_as_i64")?.into_int_value(),
                ))
            }
            Type::Name(tn, _) if tn == "f64" || tn == "f32" => {
                let func = self.get_runtime_fn("mimi_json_as_f64")?;
                let r = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "json_as_f64",
                )?;
                Ok(BasicValueEnum::FloatValue(
                    self.expect_basic_value(&r, "json_as_f64")?.into_float_value(),
                ))
            }
            Type::Name(tn, _) if tn == "bool" => {
                let func = self.get_runtime_fn("mimi_json_as_bool")?;
                let r = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "json_as_bool",
                )?;
                let iv = self
                    .expect_basic_value(&r, "json_as_bool")?
                    .into_int_value();
                let one = self.context.i64_type().const_int(1, false);
                Ok(BasicValueEnum::IntValue(
                    self.builder
                        .build_int_compare(IntPredicate::EQ, iv, one, "to_bool")
                        .map_err(|e| CompileError::LlvmError(format!("to_bool: {}", e)))?,
                ))
            }
            Type::Name(tn, _) if tn == "string" => {
                let func = self.get_runtime_fn("mimi_from_json")?;
                let r = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "from_json_str",
                )?;
                let pv = self
                    .expect_basic_value(&r, "from_json_str")?
                    .into_pointer_value();
                let strlen_fn = self.get_runtime_fn("strlen")?;
                let len = self
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(pv)],
                        "strlen",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                self.build_string_struct(pv, len)
            }
            Type::Name(n, args) if n == "List" => {
                let nested_ty = Type::Name("List".into(), args.clone());
                self.compile_from_json_turbofish_with_ptr(&[nested_ty], raw_ptr)
            }
            Type::Name(n, args) if n == "Map" => {
                let val_is_string = args
                    .get(1)
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "string"))
                    .unwrap_or(false);
                let val_is_float = args
                    .get(1)
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "f32" || tn == "f64"))
                    .unwrap_or(false);
                let fn_name = if val_is_string {
                    "mimi_map_from_json_string"
                } else if val_is_float {
                    "mimi_map_from_json_f64"
                } else {
                    "mimi_map_from_json_i64"
                };
                let func = self.get_runtime_fn(fn_name)?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "map_from_json_ok",
                )?;
                Ok(self.expect_basic_value(&result, fn_name)?.into())
            }
            Type::Name(n, args) if n == "Set" => {
                let elem_is_string = args
                    .first()
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "string"))
                    .unwrap_or(false);
                let elem_is_float = args
                    .first()
                    .map(|t| matches!(t, Type::Name(tn, _) if tn == "f32" || tn == "f64"))
                    .unwrap_or(false);
                let fn_name = if elem_is_string {
                    "mimi_set_from_json_string"
                } else if elem_is_float {
                    "mimi_set_from_json_f64"
                } else {
                    "mimi_set_from_json_i64"
                };
                let func = self.get_runtime_fn(fn_name)?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                    "set_from_json_ok",
                )?;
                Ok(self.expect_basic_value(&result, fn_name)?.into())
            }
            _ => Err(CompileError::Generic(format!(
                "from_json::<Result<{:?},_>>: unsupported Ok type",
                ok_ty
            ))),
        }
    }

    /// Build the Mimi string struct `{ ptr: i8*, len: i64 }` on the stack and load it.
    pub(in crate::codegen) fn build_string_struct(
        &self,
        ptr: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let alloca = self.build_alloca(struct_ty, "string_ret")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 0, "str_ptr_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(ptr_gep, ptr)?;
        let len_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 1, "str_len_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(len_gep, len)?;
        self.build_load(struct_ty, alloca, "string_ret_val")
    }

    /// Check if a struct type is the Mimi string struct {ptr, i64}.
    fn is_string_struct_type(&self, ty: inkwell::types::StructType<'ctx>) -> bool {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let expected = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        ty == expected
    }

    /// Deserialize a JSON object into a Record type's fields.
    fn compile_from_json_record(
        &mut self,
        type_name: &str,
        fields: &[crate::ast::Field],
        raw_ptr: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let llvm_ty = *self
            .type_llvm
            .get(type_name)
            .ok_or_else(|| CompileError::Generic(format!("type '{}' not registered", type_name)))?;
        let sty = match llvm_ty {
            BasicTypeEnum::StructType(s) => s,
            _ => {
                return Err(CompileError::Generic(format!(
                    "type '{}' is not a struct",
                    type_name
                )))
            }
        };
        let alloca = self.build_alloca(sty, type_name)?;
        let json_get_fn = self.get_runtime_fn("json_get_string")?;
        let json_as_i64_fn = self.module.get_function("mimi_json_as_i64");
        let json_as_f64_fn = self.module.get_function("mimi_json_as_f64");
        let json_as_bool_fn = self.module.get_function("mimi_json_as_bool");

        for (i, field) in fields.iter().enumerate() {
            let key_global = self
                .builder
                .build_global_string_ptr(&field.name, "field_key")
                .map_err(|e| CompileError::LlvmError(format!("global str: {}", e)))?;
            let raw_val = self
                .build_call(
                    json_get_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(raw_ptr),
                        BasicMetadataValueEnum::PointerValue(key_global.as_pointer_value()),
                    ],
                    "json_field",
                )?
                .try_as_basic_value_opt()
                .ok_or("json_get_string returned void")?
                .into_pointer_value();
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            let field_val = self.compile_json_scalar_field(
                type_name,
                field,
                raw_val,
                json_as_i64_fn,
                json_as_f64_fn,
                json_as_bool_fn,
            )?;
            self.build_store(gep, field_val)?;
        }
        Ok(alloca.into())
    }

    /// Convert a raw JSON value pointer into a scalar or string field value.
    fn compile_json_scalar_field(
        &mut self,
        type_name: &str,
        field: &crate::ast::Field,
        raw_val: PointerValue<'ctx>,
        json_as_i64_fn: Option<inkwell::values::FunctionValue<'ctx>>,
        json_as_f64_fn: Option<inkwell::values::FunctionValue<'ctx>>,
        json_as_bool_fn: Option<inkwell::values::FunctionValue<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match &field.ty {
            crate::ast::Type::Name(n, _) if n == "i32" || n == "i64" => {
                let f = json_as_i64_fn.ok_or("mimi_json_as_i64 not declared")?;
                let r = self.build_call(
                    f,
                    &[BasicMetadataValueEnum::PointerValue(raw_val)],
                    "as_i64",
                )?;
                Ok(BasicValueEnum::IntValue(
                    self.expect_basic_value(&r, "json_as_i64")?.into_int_value(),
                ))
            }
            crate::ast::Type::Name(n, _) if n == "f64" || n == "f32" => {
                let f = json_as_f64_fn.ok_or("mimi_json_as_f64 not declared")?;
                let r = self.build_call(
                    f,
                    &[BasicMetadataValueEnum::PointerValue(raw_val)],
                    "as_f64",
                )?;
                Ok(BasicValueEnum::FloatValue(
                    self.expect_basic_value(&r, "json_as_f64")?
                        .into_float_value(),
                ))
            }
            crate::ast::Type::Name(n, _) if n == "bool" => {
                let f = json_as_bool_fn.ok_or("mimi_json_as_bool not declared")?;
                let r = self.build_call(
                    f,
                    &[BasicMetadataValueEnum::PointerValue(raw_val)],
                    "as_bool",
                )?;
                let iv = self
                    .expect_basic_value(&r, "json_as_bool")?
                    .into_int_value();
                let one = self.context.i64_type().const_int(1, false);
                let bv = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, iv, one, "to_bool")
                    .map_err(|e| CompileError::LlvmError(format!("to_bool: {}", e)))?;
                Ok(BasicValueEnum::IntValue(
                    self.builder
                        .build_int_z_extend(bv, self.context.i64_type(), "bool_ext")
                        .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))?,
                ))
            }
            crate::ast::Type::Name(n, _) if n == "string" => {
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let null_ptr = i8_ptr_ty.const_null();
                let is_null = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, raw_val, null_ptr, "is_null")
                    .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                let strlen_fn = self.get_runtime_fn("strlen")?;
                let len = self
                    .build_call(
                        strlen_fn,
                        &[BasicMetadataValueEnum::PointerValue(raw_val)],
                        "strlen",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?
                    .into_int_value();
                let safe_len = self
                    .builder
                    .build_select(
                        is_null,
                        self.context.i64_type().const_int(0, false),
                        len,
                        "safe_len",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("select: {}", e)))?
                    .into_int_value();
                let safe_ptr = self
                    .builder
                    .build_select(is_null, null_ptr, raw_val, "safe_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("select: {}", e)))?
                    .into_pointer_value();
                let str_ty = self.context.struct_type(
                    &[
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                let str_alloca = self.build_alloca(str_ty, "str_val")?;
                let ptr_gep = self
                    .gep()
                    .build_struct_gep(str_ty, str_alloca, 0, "s_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.build_store(ptr_gep, safe_ptr)?;
                let len_gep = self
                    .gep()
                    .build_struct_gep(str_ty, str_alloca, 1, "s_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                self.build_store(len_gep, safe_len)?;
                self.build_load(str_ty, str_alloca, "str_val")
            }
            // Option<T> via Name form (common after resolve_type).
            crate::ast::Type::Name(n, args) if n == "Option" && args.len() == 1 => {
                self.compile_json_option_field(
                    type_name,
                    &args[0],
                    raw_val,
                    json_as_i64_fn,
                    json_as_f64_fn,
                    json_as_bool_fn,
                )
            }
            crate::ast::Type::Option(inner) => self.compile_json_option_field(
                type_name,
                inner,
                raw_val,
                json_as_i64_fn,
                json_as_f64_fn,
                json_as_bool_fn,
            ),
            // Nested List (e.g. Option<List<i32>>).
            crate::ast::Type::Name(n, args) if n == "List" => {
                let nested_ty = crate::ast::Type::Name("List".into(), args.clone());
                let nested =
                    self.compile_from_json_turbofish_with_ptr(&[nested_ty], raw_val)?;
                match nested {
                    BasicValueEnum::StructValue(sv) => {
                        let list_ty = self.list_struct_type();
                        let i64_ty = self.context.i64_type();
                        let size =
                            self.llvm_type_size_bytes(BasicTypeEnum::StructType(list_ty));
                        let heap = self.malloc_or_abort(
                            i64_ty.const_int(size, false),
                            "opt_list_heap",
                        )?;
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let typed = self
                            .build_bit_cast(
                                heap.into(),
                                BasicTypeEnum::PointerType(i8_ptr),
                                "opt_list_ptr",
                            )?
                            .into_pointer_value();
                        self.build_store(typed, sv)?;
                        Ok(BasicValueEnum::PointerValue(typed))
                    }
                    other => Ok(other),
                }
            }
            // Nested Map (e.g. Option<Map<string,i32>>).
            crate::ast::Type::Name(n, args) if n == "Map" => {
                let val_is_string = args
                    .get(1)
                    .map(|t| matches!(t, crate::ast::Type::Name(tn, _) if tn == "string"))
                    .unwrap_or(false);
                let val_is_float = args.get(1).map(|t| {
                    matches!(t, crate::ast::Type::Name(tn, _) if tn == "f32" || tn == "f64")
                }).unwrap_or(false);
                let fn_name = if val_is_string {
                    "mimi_map_from_json_string"
                } else if val_is_float {
                    "mimi_map_from_json_f64"
                } else {
                    "mimi_map_from_json_i64"
                };
                let func = self.get_runtime_fn(fn_name)?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_val)],
                    "map_from_json_field",
                )?;
                Ok(self.expect_basic_value(&result, fn_name)?.into())
            }
            // Nested Set.
            crate::ast::Type::Name(n, args) if n == "Set" => {
                let elem_is_string = args
                    .first()
                    .map(|t| matches!(t, crate::ast::Type::Name(tn, _) if tn == "string"))
                    .unwrap_or(false);
                let elem_is_float = args.first().map(|t| {
                    matches!(t, crate::ast::Type::Name(tn, _) if tn == "f32" || tn == "f64")
                }).unwrap_or(false);
                let fn_name = if elem_is_string {
                    "mimi_set_from_json_string"
                } else if elem_is_float {
                    "mimi_set_from_json_f64"
                } else {
                    "mimi_set_from_json_i64"
                };
                let func = self.get_runtime_fn(fn_name)?;
                let result = self.build_call(
                    func,
                    &[BasicMetadataValueEnum::PointerValue(raw_val)],
                    "set_from_json_field",
                )?;
                Ok(self.expect_basic_value(&result, fn_name)?.into())
            }
            // Nested Record: json_get_string returns the nested object as a
            // JSON substring; recurse into compile_from_json_record.
            crate::ast::Type::Name(nested_name, _) => {
                let fields_opt =
                    self.type_defs
                        .get(nested_name.as_str())
                        .and_then(|td| match &td.kind {
                            crate::ast::TypeDefKind::Record(fields) => Some(fields.clone()),
                            _ => None,
                        });
                if let Some(nested_fields) = fields_opt {
                    let nested = self.compile_from_json_record(
                        nested_name,
                        &nested_fields,
                        raw_val,
                    )?;
                    // compile_from_json_record returns an alloca pointer; store
                    // by-value into the parent field slot.
                    match nested {
                        BasicValueEnum::PointerValue(pv) => {
                            let llvm_ty = *self.type_llvm.get(nested_name.as_str()).ok_or_else(
                                || {
                                    CompileError::Generic(format!(
                                        "type '{}' not registered",
                                        nested_name
                                    ))
                                },
                            )?;
                            self.build_load(llvm_ty, pv, nested_name)
                        }
                        other => Ok(other),
                    }
                } else {
                    Err(CompileError::Generic(format!(
                        "from_json::<{}>: unsupported field type {:?}",
                        type_name, field.ty
                    )))
                }
            }
            _ => Err(CompileError::Generic(format!(
                "from_json::<{}>: unsupported field type {:?}",
                type_name, field.ty
            ))),
        }
    }

    /// Deserialize a JSON value into `Option<T>` (`{i1, i64}` canonical).
    fn compile_json_option_field(
        &mut self,
        parent_type: &str,
        inner: &crate::ast::Type,
        raw_val: PointerValue<'ctx>,
        json_as_i64_fn: Option<inkwell::values::FunctionValue<'ctx>>,
        json_as_f64_fn: Option<inkwell::values::FunctionValue<'ctx>>,
        json_as_bool_fn: Option<inkwell::values::FunctionValue<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let bool_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let option_sty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no function for Option field".to_string())?;
        let some_bb = self.context.append_basic_block(function, "json_opt_some");
        let none_bb = self.context.append_basic_block(function, "json_opt_none");
        let merge_bb = self.context.append_basic_block(function, "json_opt_merge");

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let is_null = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                raw_val,
                i8_ptr.const_null(),
                "json_opt_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
        // Also treat the literal "null" as None.
        let null_lit = self
            .builder
            .build_global_string_ptr("null", "json_null_lit")
            .map_err(|e| CompileError::LlvmError(format!("gstr: {}", e)))?;
        let strcmp_fn = self.get_runtime_fn("strcmp")?;
        let cmp_null = self
            .build_call(
                strcmp_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(raw_val),
                    BasicMetadataValueEnum::PointerValue(null_lit.as_pointer_value()),
                ],
                "strcmp_null",
            )?
            .try_as_basic_value_opt()
            .ok_or("strcmp returned void")?
            .into_int_value();
        let is_null_lit = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                cmp_null,
                self.context.i32_type().const_int(0, false),
                "is_null_lit",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
        let is_none = self
            .builder
            .build_or(is_null, is_null_lit, "json_opt_is_none")
            .map_err(|e| CompileError::LlvmError(format!("or: {}", e)))?;
        self.builder
            .build_conditional_branch(is_none, none_bb, some_bb)
            .map_err(|e| CompileError::LlvmError(format!("br: {}", e)))?;

        // Some path: parse inner as a temporary Field of type T.
        self.builder.position_at_end(some_bb);
        let fake_field = crate::ast::Field {
            name: "opt_inner".into(),
            ty: inner.clone(),
        };
        let inner_val = self.compile_json_scalar_field(
            parent_type,
            &fake_field,
            raw_val,
            json_as_i64_fn,
            json_as_f64_fn,
            json_as_bool_fn,
        )?;
        let pay_i64 = match inner_val {
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "opt_sext")
                        .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))?
                } else if bw > 64 {
                    self.builder
                        .build_int_truncate(iv, i64_ty, "opt_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("trunc: {}", e)))?
                } else {
                    iv
                }
            }
            BasicValueEnum::FloatValue(fv) => {
                let f64_ty = self.context.f64_type();
                let as_f64 = if fv.get_type().get_bit_width() == 64 {
                    fv
                } else {
                    self.builder
                        .build_float_ext(fv, f64_ty, "opt_fpext")
                        .map_err(|e| CompileError::LlvmError(format!("fpext: {}", e)))?
                };
                self.builder
                    .build_bit_cast(as_f64, i64_ty, "opt_fbits")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?
                    .into_int_value()
            }
            BasicValueEnum::PointerValue(pv) => self
                .builder
                .build_ptr_to_int(pv, i64_ty, "opt_ptr")
                .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?,
            BasicValueEnum::StructValue(sv) => {
                let tmp = self.build_alloca(BasicTypeEnum::StructType(sv.get_type()), "opt_s")?;
                self.build_store(tmp, sv)?;
                self.builder
                    .build_ptr_to_int(tmp, i64_ty, "opt_sptr")
                    .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?
            }
            other => {
                return Err(CompileError::Generic(format!(
                    "from_json Option: unsupported inner value {:?}",
                    other.get_type()
                )));
            }
        };
        let some_slot = self.build_alloca(BasicTypeEnum::StructType(option_sty), "opt_some")?;
        self.build_store(
            self.gep()
                .build_struct_gep(BasicTypeEnum::StructType(option_sty), some_slot, 0, "d")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            bool_ty.const_int(1, false),
        )?;
        self.build_store(
            self.gep()
                .build_struct_gep(BasicTypeEnum::StructType(option_sty), some_slot, 1, "p")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            pay_i64,
        )?;
        let some_val = self.build_load(BasicTypeEnum::StructType(option_sty), some_slot, "s")?;
        self.build_br(merge_bb)?;
        let some_end = self.builder.get_insert_block().unwrap_or(some_bb);

        self.builder.position_at_end(none_bb);
        let none_slot = self.build_alloca(BasicTypeEnum::StructType(option_sty), "opt_none")?;
        self.build_store(
            self.gep()
                .build_struct_gep(BasicTypeEnum::StructType(option_sty), none_slot, 0, "nd")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            bool_ty.const_int(0, false),
        )?;
        self.build_store(
            self.gep()
                .build_struct_gep(BasicTypeEnum::StructType(option_sty), none_slot, 1, "np")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            i64_ty.const_int(0, false),
        )?;
        let none_val = self.build_load(BasicTypeEnum::StructType(option_sty), none_slot, "n")?;
        self.build_br(merge_bb)?;
        let none_end = self.builder.get_insert_block().unwrap_or(none_bb);

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(BasicTypeEnum::StructType(option_sty), "opt_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi: {}", e)))?;
        phi.add_incoming(&[(&some_val, some_end), (&none_val, none_end)]);
        Ok(phi.as_basic_value())
    }

    pub(in crate::codegen) fn compile_expr_or_func_ref(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match self.compile_expr(expr, vars) {
            Ok(val) => Ok(val),
            Err(_) => {
                // Try to resolve as a module function
                if let Expr::Ident(name) = expr {
                    if let Some(func) = self.module.get_function(name) {
                        let fn_ptr = func.as_global_value().as_pointer_value();
                        return Ok(BasicValueEnum::PointerValue(fn_ptr));
                    }
                }
                // Re-compile to get the original error
                self.compile_expr(expr, vars)
            }
        }
    }
}

/// Strip generic arguments from a type string so that trait impl lookups use
/// the base type name (e.g. "List<T>" and "List<i32>" both map to "List").
fn base_type_name(type_str: &str) -> &str {
    match type_str.find('<') {
        Some(idx) => &type_str[..idx],
        None => type_str,
    }
}

/// Map a string method name to the corresponding `str_*` builtin function name.
/// Used by `compile_method_call` as a fallback when no trait provides the method.
fn string_method_to_builtin(method: &str) -> Option<&'static str> {
    match method {
        "trim" => Some("str_trim"),
        "to_upper" => Some("str_to_upper"),
        "to_lower" => Some("str_to_lower"),
        "contains" => Some("str_contains"),
        "starts_with" => Some("str_starts_with"),
        "ends_with" => Some("str_ends_with"),
        "split" => Some("str_split"),
        "replace" => Some("str_replace"),
        "char_at" => Some("str_char_at"),
        "substring" => Some("str_substring"),
        "parse_int" => Some("str_parse_int"),
        "parse_float" => Some("str_parse_float"),
        "repeat" => Some("str_repeat"),
        "index_of" => Some("str_index_of"),
        _ => None,
    }
}
