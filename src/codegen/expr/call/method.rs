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

        Err(CompileError::Generic(format!(
            "method '{}' not compiled for type '{}' (missing crate?)",
            method_name, obj_type
        )))
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
        for arg in args {
            compiled_args.push(self.compile_expr(arg, vars)?);
        }
        let metadata_args = self.values_to_metadata(&compiled_args);
        let call = self.build_call(function, &metadata_args, call_name)?;
        Ok(call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into()))
    }

    /// Ensure a `self` argument is passed as a pointer when it is a struct value.
    fn ensure_self_pointer(
        &mut self,
        obj_val: BasicValueEnum<'ctx>,
        obj: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match obj_val {
            BasicValueEnum::StructValue(sv) => {
                let struct_ty = sv.get_type();
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
                let malloc_fn = self.get_runtime_fn("malloc")?;
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
                let data_ptr = self
                    .build_call(
                        malloc_fn,
                        &[BasicMetadataValueEnum::IntValue(alloc_size)],
                        "malloc_data",
                    )?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();

                // Cast to i64* for element access
                let data_i64_ptr = self.build_pointer_cast(
                    data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "data_i64",
                )?;

                // Build list struct {i64 len, i8* data}
                let list_alloca = self.alloc_list_result(len_val, data_ptr)?;

                // Determine if list elements are heap-allocated pointers (string or record)
                // that need individual freeing at scope exit via FreeList mechanism.
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
                            let malloc_fn = self.get_runtime_fn("malloc")?;
                            let size_val = i64_ty.const_int(struct_size, false);
                            let heap_ptr = self
                                .build_call(
                                    malloc_fn,
                                    &[BasicMetadataValueEnum::IntValue(size_val)],
                                    "malloc_record",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("malloc returned void")?
                                .into_pointer_value();
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

                // Register FreeList cleanup if elements need individual freeing
                if needs_element_free {
                    self.register_heap_list_elements(list_alloca);
                }

                // Load and return list struct
                let list_ty = self.list_struct_type();
                self.build_load(BasicTypeEnum::StructType(list_ty), list_alloca, "list")
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
                    "from_json::<{}>: unsupported type (only Record types with scalar/string fields are supported)",
                    type_name
                )))
            }
            _ => Err(CompileError::Generic(format!(
                "from_json::<{:?}> codegen not yet implemented",
                type_args[0]
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
            _ => Err(CompileError::Generic(format!(
                "from_json::<{}>: unsupported field type {:?}",
                type_name, field.ty
            ))),
        }
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
        _ => None,
    }
}
