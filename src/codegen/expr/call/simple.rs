use crate::ast::*;
use crate::codegen::types;
use crate::codegen::{call_try_basic_value, CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_call_expr(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match callee {
            Expr::Ident(name) => {
                match name.as_str() {
                    "type_name" | "type_fields" | "type_variants" | "keys" | "values" | "map"
                    | "filter" | "reduce" => {
                        return self.compile_builtin_intrinsic(name, args, vars);
                    }
                    _ => {}
                }

                if self.fn_ptr_var_names.contains(name.as_str()) {
                    if let Some(&(alloca, ty)) = vars.get(name.as_str()) {
                        return self.compile_fn_ptr_var_call(name, alloca, ty, args, vars);
                    }
                }

                if let Some(&(alloca, BasicTypeEnum::StructType(st))) = vars.get(name.as_str()) {
                    if st.get_field_types().len() == 2 {
                        let closure_val = self.build_load(
                            BasicTypeEnum::StructType(st),
                            alloca,
                            &format!("{}_closure", name),
                        )?;
                        let compiled_args = self.compile_arg_values(args, vars)?;
                        return self.compile_closure_call(closure_val, &compiled_args);
                    }
                }

                self.compile_call(name, args, vars)
            }
            Expr::Field(obj, method_name) => {
                if let Expr::Ident(type_name) = obj.as_ref() {
                    let is_builtin_enum = type_name == "Result" || type_name == "Option";
                    let is_custom_enum = self
                        .type_defs
                        .get(type_name)
                        .map(|td| matches!(td.kind, crate::ast::TypeDefKind::Enum(_)))
                        .unwrap_or(false);
                    if is_builtin_enum {
                        return self.compile_call(method_name, args, vars);
                    }
                    if is_custom_enum {
                        return self.compile_custom_enum_constructor_call(
                            type_name,
                            method_name,
                            args,
                            vars,
                        );
                    }
                }
                self.compile_method_call(obj, method_name, args, vars)
            }
            _ => Err("only direct function calls and method calls supported in codegen".into()),
        }
    }

    /// Call a variable that holds a first-class function pointer.
    fn compile_fn_ptr_var_call(
        &mut self,
        name: &str,
        alloca: inkwell::values::PointerValue<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let fn_ptr = self
            .build_load(ty, alloca, &format!("{}_fn", name))?
            .into_pointer_value();
        let compiled_args = self.compile_arg_values(args, vars)?;
        let i64_ty = self.context.i64_type();
        let all_meta: Vec<_> = compiled_args
            .iter()
            .map(|arg| basic_value_to_metadata_type(arg))
            .collect();
        let ret_type = i64_ty;
        let indirect_fn_type = ret_type.fn_type(&all_meta, false);
        let fn_ptr_typed = self.build_pointer_cast(
            fn_ptr,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "fn_typed",
        )?;
        let call_args: Vec<_> = compiled_args
            .iter()
            .map(|arg| types::basic_value_to_metadata_value(arg, i64_ty))
            .collect();
        let call = self
            .builder
            .build_indirect_call(indirect_fn_type, fn_ptr_typed, &call_args, "fn_ptr_call")
            .map_err(|e| CompileError::LlvmError(format!("fn ptr call error: {}", e)))?;
        Ok(call_try_basic_value(&call).unwrap_or(i64_ty.const_int(0, false).into()))
    }

    /// Call a user-defined enum variant constructor.
    fn compile_custom_enum_constructor_call(
        &mut self,
        type_name: &str,
        method_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ctor_name = format!("{}_{}", type_name, method_name);
        let function = self
            .module
            .get_function(&ctor_name)
            .ok_or_else(|| format!("enum constructor '{}' not registered", ctor_name))?;
        let compiled_args = self.compile_arg_values(args, vars)?;
        self.emit_direct_call(function, &compiled_args, "enum_ctor")
    }

    pub(in crate::codegen) fn compile_call_fn_ref(
        &mut self,
        fn_ref: BasicValueEnum<'ctx>,
        arg_expr: &Expr,
        payload: BasicValueEnum<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match fn_ref {
            BasicValueEnum::StructValue(_) => self.compile_closure_call(fn_ref, &[payload]),
            BasicValueEnum::PointerValue(_) => {
                if let Expr::Ident(fn_name) = arg_expr {
                    if let Some(func) = self.module.get_function(fn_name) {
                        let call = self.build_call(
                            func,
                            &[BasicMetadataValueEnum::IntValue(payload.into_int_value())],
                            "fn_call",
                        )?;
                        return Ok(call_try_basic_value(&call)
                            .unwrap_or(BasicValueEnum::IntValue(i64_ty.const_int(0, false))));
                    }
                }
                self.compile_closure_call(fn_ref, &[payload])
            }
            _ => Err("function reference must be a closure or function pointer".into()),
        }
    }

    pub(in crate::codegen) fn compile_call(
        &mut self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let mut compiled_args = self.compile_arg_values(args, vars)?;

        self.maybe_convert_callback_args(name, &mut compiled_args)?;
        self.maybe_load_reprc_struct_args_for_extern(name, &mut compiled_args)?;

        let mut metadata_args: Vec<_> = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if name == "len" && args.len() == 1 {
            self.pending_len_is_string = self.expr_is_string(&args[0]);
        }
        if crate::codegen::builtins::is_builtin(name) {
            return self
                .compile_builtin_call(name, &metadata_args)
                .map_err(|e| CompileError::Generic(e.to_string()));
        }

        if let Some((type_name, _ordinal)) = self.find_variant_owner(name) {
            let ctor_name = format!("{}_{}", type_name, name);
            if let Some(function) = self.module.get_function(&ctor_name) {
                let call = self.build_call(function, &metadata_args, "call")?;
                return Ok(call_try_basic_value(&call)
                    .unwrap_or(self.context.i64_type().const_int(0, false).into()));
            }
            return Err(format!("enum constructor '{}' not registered", ctor_name).into());
        }

        match name {
            "Ok" | "Some" | "Err" | "None" => return self.compile_constructor(name, compiled_args),
            _ => {}
        }

        self.maybe_convert_list_args_to_values(name, &mut compiled_args)?;
        self.maybe_wrap_named_fn_args_to_closures(name, args, &mut compiled_args)?;

        metadata_args = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if self.extern_func_defs.contains_key(name) {
            self.generate_extern_fn(name)?;
        }
        self.emit_named_call(name, args, &metadata_args)
    }

    /// G1b: Convert closure struct args to thunk pointers for extern callback params.
    fn maybe_convert_callback_args(
        &mut self,
        name: &str,
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(param_types) = self.extern_param_types.get(name).cloned() else {
            return Ok(());
        };
        for (i, compiled) in compiled_args.iter_mut().enumerate() {
            if i >= param_types.len() {
                break;
            }
            let (cb_params, cb_ret) = match &param_types[i] {
                crate::ast::Type::ExternFunc(p, r) => (p.as_slice(), r.as_ref()),
                crate::ast::Type::Func(p, r) => (p.as_slice(), r.as_ref()),
                _ => continue,
            };
            if let BasicValueEnum::StructValue(sv) = compiled {
                let struct_ty = sv.get_type();
                if struct_ty.get_field_types().len() == 2 {
                    let fn_ptr = self
                        .build_extract_value((*sv).into(), 0, "cb_fn_ptr")?
                        .into_pointer_value();
                    let env_ptr = self
                        .build_extract_value((*sv).into(), 1, "cb_env_ptr")?
                        .into_pointer_value();
                    let thunk_entry = self
                        .get_or_create_callback_thunk(cb_params, cb_ret)
                        .map_err(|e| CompileError::LlvmError(format!("callback thunk: {}", e)))?;
                    self.build_store(thunk_entry.fn_ptr_global.as_pointer_value(), fn_ptr)?;
                    self.build_store(thunk_entry.env_ptr_global.as_pointer_value(), env_ptr)?;
                    self.pending_callback_tls
                        .push(thunk_entry.fn_ptr_global.as_pointer_value());
                    self.pending_callback_tls
                        .push(thunk_entry.env_ptr_global.as_pointer_value());
                    let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let thunk_ptr = thunk_entry.thunk_fn.as_global_value().as_pointer_value();
                    let casted = self.build_pointer_cast(thunk_ptr, i8_ptr_ty, "thunk_i8")?;
                    *compiled = casted.into();
                }
            }
        }
        Ok(())
    }

    /// For extern functions: load struct values from pointers for repr(C) struct-by-value params.
    fn maybe_load_reprc_struct_args_for_extern(
        &self,
        name: &str,
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(ef) = self.extern_func_defs.get(name) else {
            return Ok(());
        };
        for (i, arg) in compiled_args.iter_mut().enumerate() {
            if i >= ef.params.len() {
                break;
            }
            if let crate::ast::Type::Name(n, _) = &ef.params[i].ty {
                if self.repr_c_record_names.contains(n.as_str()) {
                    if let BasicValueEnum::PointerValue(pv) = arg {
                        if let Some(&BasicTypeEnum::StructType(sty)) =
                            self.type_llvm.get(n.as_str())
                        {
                            let loaded = self.build_load(
                                BasicTypeEnum::StructType(sty),
                                *pv,
                                &format!("{}_extern_val", n),
                            )?;
                            *arg = loaded;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Convert pointer-valued list arguments to struct values when the function
    /// parameter expects List<T> (passed by value).
    fn maybe_convert_list_args_to_values(
        &self,
        name: &str,
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name) else {
            return Ok(());
        };
        for (i, arg) in compiled_args.iter_mut().enumerate() {
            if i < fdef.params.len() {
                if let Type::Name(tn, _) = &fdef.params[i].ty {
                    if tn == "List" {
                        if let Some(param_llvm) = self.llvm_type_for(&fdef.params[i].ty) {
                            if let BasicValueEnum::PointerValue(pv) = arg {
                                let loaded = self.build_load(
                                    param_llvm,
                                    *pv,
                                    &format!("{}_struct_arg", &fdef.params[i].name),
                                )?;
                                *arg = loaded;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Convert function pointers to closure structs when the parameter type expects func(T) -> U.
    fn maybe_wrap_named_fn_args_to_closures(
        &mut self,
        name: &str,
        args: &[Expr],
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name) else {
            return Ok(());
        };
        let wrappers: Vec<Option<String>> = args
            .iter()
            .enumerate()
            .map(|(i, arg_expr)| {
                if i < fdef.params.len() && matches!(&fdef.params[i].ty, Type::Func(_, _)) {
                    if let Expr::Ident(fn_name) = arg_expr {
                        return Some(fn_name.clone());
                    }
                }
                None
            })
            .collect();

        for (i, fn_name_opt) in wrappers.into_iter().enumerate() {
            if let Some(fn_name) = fn_name_opt {
                if let BasicValueEnum::PointerValue(_pv) = compiled_args[i] {
                    let wrapper = self.get_or_create_closure_wrapper(&fn_name)?;
                    let closure_ty = crate::codegen::types::closure_struct_type(self.context);
                    let closure_alloca =
                        self.build_alloca(BasicTypeEnum::StructType(closure_ty), "closure_arg")?;
                    let fn_gep = self
                        .gep()
                        .build_struct_gep(closure_ty, closure_alloca, 0, "fn_gep")
                        .map_err(|e| CompileError::LlvmError(format!("fn gep: {}", e)))?;
                    self.build_store(fn_gep, BasicValueEnum::PointerValue(wrapper))?;
                    let env_gep = self
                        .gep()
                        .build_struct_gep(closure_ty, closure_alloca, 1, "env_gep")
                        .map_err(|e| CompileError::LlvmError(format!("env gep: {}", e)))?;
                    let null_i8 = self
                        .context
                        .ptr_type(inkwell::AddressSpace::default())
                        .const_null();
                    self.build_store(env_gep, BasicValueEnum::PointerValue(null_i8))?;
                    let loaded = self.build_load(
                        BasicTypeEnum::StructType(closure_ty),
                        closure_alloca,
                        "closure_loaded",
                    )?;
                    compiled_args[i] = loaded;
                }
            }
        }
        Ok(())
    }

    /// Emit a call to a function looked up by name, with generic monomorphization fallback.
    fn emit_named_call(
        &mut self,
        name: &str,
        args: &[Expr],
        metadata_args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if let Some(function) = self.module.get_function(name) {
            return self.emit_function_call(function, name, metadata_args);
        }

        let mangled = if let Some(fdef) = self.func_defs.get(name) {
            if !fdef.generics.is_empty() {
                let mut callee_map: HashMap<String, Type> = HashMap::new();
                for gp in &fdef.generics {
                    for (i, param) in fdef.params.iter().enumerate() {
                        if i < args.len() && Self::type_references_generic(&param.ty, &gp.name) {
                            if let Some(arg_type) = self.expr_type_of(&args[i], &HashMap::new()) {
                                callee_map.insert(gp.name.clone(), arg_type);
                                break;
                            }
                        }
                    }
                }
                Self::mangle_name(name, &callee_map)
            } else {
                Self::mangle_name(name, &self.type_map)
            }
        } else {
            Self::mangle_name(name, &self.type_map)
        };

        if let Some(function) = self.module.get_function(&mangled) {
            let call = self.build_call(function, metadata_args, "call")?;
            Ok(call_try_basic_value(&call)
                .unwrap_or(self.context.i64_type().const_int(0, false).into()))
        } else {
            let msg = if self.comptime_func_names.contains(name) {
                format!("comptime function '{}' is compile-time only and cannot be called from runtime code", name)
            } else {
                format!("undefined function '{}' in codegen", name)
            };
            Err(msg.into())
        }
    }

    /// Emit a direct call to a known function, clear callback TLS, and record async info.
    fn emit_function_call(
        &mut self,
        function: inkwell::values::FunctionValue<'ctx>,
        name: &str,
        metadata_args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let call = self.build_call(function, metadata_args, "call")?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let null_i8 = i8_ptr_ty.const_null();
        let tls_ptrs: Vec<_> = self.pending_callback_tls.drain(..).collect();
        for tls_ptr in tls_ptrs {
            self.build_store(tls_ptr, null_i8)?;
        }
        if let Some(fdef) = self.func_defs.get(name) {
            if fdef.is_async {
                if let Some(ret_ty) = &fdef.ret {
                    if let Some(llvm_ret) = self.llvm_type_for(ret_ty) {
                        self.pending_spawn_type = Some(llvm_ret);
                    }
                }
            }
        }
        let result = call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into());
        // CLOSE-GAP-5: when the callee returns a heap-owned `string` struct,
        // store it into a fresh alloca so the caller's `free_heap_allocs`
        // can release the data at scope exit. The callee already ensures the
        // data pointer is heap-owned (via `claim_string_return_value`), so
        // the registered pointer is always safe to free.
        self.track_string_return_lifetime(name, result)
    }

    /// If `result` is a Mimi string struct returned by a function call, stash
    /// it into a fresh alloca so the heap-owned data pointer can be freed at
    /// the caller's scope exit. Non-string or non-struct results pass through.
    fn track_string_return_lifetime(
        &self,
        callee_name: &str,
        result: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ret_is_string = self
            .func_defs
            .get(callee_name)
            .and_then(|fd| fd.ret.as_ref())
            .map(|t| matches!(t, Type::Name(n, _) if n == "string"))
            .unwrap_or(false);
        if !ret_is_string {
            return Ok(result);
        }
        let sv = match result {
            BasicValueEnum::StructValue(sv) => sv,
            other => return Ok(other),
        };
        let sty = sv.get_type();
        let fields = sty.get_field_types();
        let is_mimi_string_struct = fields.len() == 2
            && matches!(fields[0], BasicTypeEnum::PointerType(_))
            && matches!(fields[1], BasicTypeEnum::IntType(_));
        if !is_mimi_string_struct {
            return Ok(sv.into());
        }
        // Allocate a slot for the struct, store into it, register the data
        // GEP so the loader sees the latest value at free time. Return the
        // loaded struct to the caller.
        let slot = self.build_alloca(sty, "call_str_slot")?;
        self.build_store(slot, sv)?;
        if let Ok(data_gep) = self
            .gep()
            .build_struct_gep(sty, slot, 0, "call_str_data_gep")
        {
            self.register_heap_gep(data_gep);
        }
        let loaded = self.build_load(sty, slot, "call_str_load")?;
        Ok(loaded.into_struct_value().into())
    }

    /// Build a call to a declared function and extract its basic value.
    fn emit_direct_call(
        &self,
        function: inkwell::values::FunctionValue<'ctx>,
        compiled_args: &[BasicValueEnum<'ctx>],
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let metadata_args: Vec<_> = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();
        let call = self.build_call(function, &metadata_args, name)?;
        Ok(call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into()))
    }

    pub(in crate::codegen) fn compile_call_mangled(
        &mut self,
        mangled: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let compiled_args = self.compile_arg_values(args, vars)?;
        let metadata_args: Vec<_> = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if let Some(function) = self.module.get_function(mangled) {
            let call = self.build_call(function, &metadata_args, "call")?;
            Ok(call_try_basic_value(&call)
                .unwrap_or(self.context.i64_type().const_int(0, false).into()))
        } else {
            let msg = if self.comptime_func_names.contains(mangled) {
                format!("comptime function '{}' is compile-time only and cannot be called from runtime code", mangled)
            } else {
                format!("undefined function '{}' in codegen", mangled)
            };
            Err(msg.into())
        }
    }

    /// Compile argument expressions into LLVM basic values.
    fn compile_arg_values(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CompileError> {
        args.iter()
            .map(|arg| self.compile_expr(arg, vars))
            .collect()
    }

    /// Get or create a closure ABI wrapper for a named function.
    pub(in crate::codegen) fn get_or_create_closure_wrapper(
        &mut self,
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        if let Some(cached) = self.closure_wrappers.get(name) {
            return Ok(*cached);
        }

        let orig_fn = self.module.get_function(name).ok_or_else(|| {
            CompileError::Generic(format!(
                "cannot create closure wrapper for unknown function '{}'",
                name
            ))
        })?;
        let fn_type = orig_fn.get_type();
        let param_tys = fn_type.get_param_types();
        let ret_ty = fn_type.get_return_type().ok_or_else(|| {
            CompileError::Generic(format!(
                "closure wrapper: function '{}' has void return type",
                name
            ))
        })?;

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut wrapper_params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        wrapper_params.push(BasicMetadataTypeEnum::PointerType(i8_ptr));
        for pt in &param_tys {
            wrapper_params.push(*pt);
        }

        let wrapper_fn_type = fn_type_for_basic_type(ret_ty, &wrapper_params)?;
        let wrapper_name = format!("__mimi_fn_wrapper_{}", name.replace('.', "_"));
        let wrapper_fn = self.module.add_function(
            &wrapper_name,
            wrapper_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );

        let saved_block = self.builder.get_insert_block();
        let entry_bb = self.context.append_basic_block(wrapper_fn, "entry");
        self.builder.position_at_end(entry_bb);

        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for i in 0..param_tys.len() {
            let param = wrapper_fn.get_nth_param((i + 1) as u32).ok_or_else(|| {
                CompileError::LlvmError(format!("wrapper: param {} not found", i + 1))
            })?;
            call_args.push(types::basic_value_to_metadata_value(
                &param,
                self.context.i64_type(),
            ));
        }

        let call = self.build_call(orig_fn, &call_args, "wrapper_call")?;
        let ret_val = crate::codegen::call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("wrapper call returned void".to_string()))?;
        self.build_return(Some(&ret_val))?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let wrapper_ptr = wrapper_fn.as_global_value().as_pointer_value();
        self.closure_wrappers.insert(name.to_string(), wrapper_ptr);
        Ok(wrapper_ptr)
    }

    /// Find a FuncDef by name from the codegen's stored func_defs
    pub(in crate::codegen) fn find_func_def(&self, name: &str) -> Result<FuncDef, CompileError> {
        self.func_defs.get(name).cloned().ok_or_else(|| {
            CompileError::Generic(format!(
                "function '{}' definition not available for monomorphization",
                name
            ))
        })
    }
}

/// Convert a BasicValueEnum to its metadata type for indirect calls.
fn basic_value_to_metadata_type<'ctx>(val: &BasicValueEnum<'ctx>) -> BasicMetadataTypeEnum<'ctx> {
    match val {
        BasicValueEnum::IntValue(iv) => BasicMetadataTypeEnum::IntType(iv.get_type()),
        BasicValueEnum::FloatValue(fv) => BasicMetadataTypeEnum::FloatType(fv.get_type()),
        BasicValueEnum::PointerValue(pv) => BasicMetadataTypeEnum::PointerType(pv.get_type()),
        BasicValueEnum::StructValue(sv) => BasicMetadataTypeEnum::StructType(sv.get_type()),
        BasicValueEnum::ArrayValue(av) => BasicMetadataTypeEnum::ArrayType(av.get_type()),
        BasicValueEnum::VectorValue(vv) => BasicMetadataTypeEnum::VectorType(vv.get_type()),
        BasicValueEnum::ScalableVectorValue(_) => {
            BasicMetadataTypeEnum::IntType(iv_type_unavailable())
        }
    }
}

/// Build an LLVM function type from a basic return type and parameter types.
fn fn_type_for_basic_type<'ctx>(
    ret_ty: BasicTypeEnum<'ctx>,
    params: &[BasicMetadataTypeEnum<'ctx>],
) -> Result<inkwell::types::FunctionType<'ctx>, CompileError> {
    match ret_ty {
        BasicTypeEnum::IntType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::FloatType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::PointerType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::StructType(t) => Ok(t.fn_type(params, false)),
        BasicTypeEnum::ArrayType(t) => Ok(t.fn_type(params, false)),
        _ => Err(CompileError::Generic(
            "closure wrapper: unsupported return type".to_string(),
        )),
    }
}

fn iv_type_unavailable<'ctx>() -> inkwell::types::IntType<'ctx> {
    unreachable!("scalable vector not supported in Mimi codegen")
}
