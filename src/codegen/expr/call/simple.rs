use crate::ast::*;
use crate::codegen::expr::call::helpers::infer_generic_args;
use crate::codegen::types;
use crate::codegen::{call_try_basic_value, CallSiteValueExt, CodeGenerator, VarEntry};
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
                        let ret_ty = self
                            .var_types
                            .get(name.as_str())
                            .and_then(|ty| Self::closure_return_llvm_type(self, ty));
                        return self.compile_closure_call(closure_val, &compiled_args, ret_ty);
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
            BasicValueEnum::StructValue(_) => self.compile_closure_call(fn_ref, &[payload], None),
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
                self.compile_closure_call(fn_ref, &[payload], None)
            }
            _ => Err("function reference must be a closure or function pointer".into()),
        }
    }

    /// Extract the LLVM return type of a closure-typed variable so that indirect
    /// calls use the correct ABI (especially for tuple/struct/float returns).
    fn closure_return_llvm_type(&self, ty: &Type) -> Option<BasicTypeEnum<'ctx>> {
        match ty {
            Type::Func(_, ret) | Type::ExternFunc(_, ret) => self.llvm_type_for(ret),
            Type::Ref(_, inner) | Type::RefMut(_, inner) => self.closure_return_llvm_type(inner),
            _ => None,
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
        self.coerce_args_to_param_types(name, &mut compiled_args)?;

        let mut metadata_args: Vec<_> = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if name == "len" && args.len() == 1 {
            self.pending_len_is_string = self.expr_is_string(&args[0]);
        }
        if name == "to_string" && args.len() == 1 {
            let arg_type = self.infer_object_type(&args[0], vars);
            self.pending_to_string_is_any = arg_type == "Any" || arg_type == "any";
        }
        if name == "push" && args.len() == 2 {
            let list_type = self.infer_object_type(&args[0], vars);
            if let Some(elem_type) = Self::strip_list_element_type(&list_type) {
                self.pending_push_elem_type = Some(elem_type);
            }
        }
        let builtin_available = crate::codegen::builtins::is_builtin(name);
        let user_func_matches = self.user_func_signature_matches(name, args);
        if builtin_available && !user_func_matches {
            // Special case: `to_json(obj)` where obj is a List<T> — dispatch
            // to the appropriate mimi_list_*_to_json runtime helper.
            if name == "to_json" && !args.is_empty() && !metadata_args.is_empty() {
                let obj_type = self.infer_object_type(&args[0], vars);
                if let Some(inner) = obj_type
                    .strip_prefix("List<")
                    .and_then(|s| s.strip_suffix('>'))
                {
                    let list_struct_ty = self.list_struct_type();
                    let alloca = self.build_alloca(list_struct_ty, "to_json_list_alloca")?;
                    match &metadata_args[0] {
                        BasicMetadataValueEnum::StructValue(sv) => {
                            self.build_store(alloca, *sv)?;
                        }
                        BasicMetadataValueEnum::PointerValue(pv) => {
                            let loaded = self
                                .builder
                                .build_load(
                                    BasicTypeEnum::StructType(list_struct_ty),
                                    *pv,
                                    "to_json_list_load",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                .into_struct_value();
                            self.build_store(alloca, loaded)?;
                        }
                        _ => {
                            return Err(CompileError::Generic(format!(
                                "to_json: unexpected List argument kind for {}",
                                obj_type
                            )))
                        }
                    }
                    // Check for record element type — needs callback-based serialization
                    let is_record = self
                        .type_defs
                        .get(inner)
                        .map(|td| matches!(td.kind, TypeDefKind::Record(_)))
                        .unwrap_or(false);
                    if is_record {
                        if let Some(td) = self.type_defs.get(inner) {
                            if let TypeDefKind::Record(fields) = &td.kind {
                                let fields_clone = fields.clone();
                                return self.compile_record_list_to_json(
                                    inner,
                                    &fields_clone,
                                    &alloca,
                                );
                            }
                        }
                    }
                    let rt_fn_name = match inner {
                        "string" => "mimi_list_str_to_json",
                        "f64" => "mimi_list_f64_to_json",
                        "bool" => "mimi_list_bool_to_json",
                        _ => "mimi_list_i64_to_json",
                    };
                    let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let fn_ty =
                        i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                    let callee = self.module.get_function(rt_fn_name).unwrap_or_else(|| {
                        self.module.add_function(
                            rt_fn_name,
                            fn_ty,
                            Some(inkwell::module::Linkage::External),
                        )
                    });
                    let raw = self
                        .build_call(
                            callee,
                            &[BasicMetadataValueEnum::PointerValue(alloca)],
                            "to_json_list",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("to_json list helper returned void")?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return self.wrap_c_string(raw);
                }
                // Check for Record type — serialize to JSON object via sprintf
                if let Some(type_def) = self.type_defs.get(&obj_type) {
                    if let TypeDefKind::Record(fields) = &type_def.kind {
                        let struct_ptr = match &metadata_args[0] {
                            BasicMetadataValueEnum::PointerValue(pv) => *pv,
                            _ => {
                                return Err(CompileError::Generic(
                                    "to_json: record value must be a pointer".into(),
                                ))
                            }
                        };
                        let llvm_ty = *self.type_llvm.get(&obj_type).ok_or_else(|| {
                            CompileError::LlvmError(format!("no LLVM type for record {}", obj_type))
                        })?;
                        let BasicTypeEnum::StructType(sty) = llvm_ty else {
                            return Err(CompileError::LlvmError(format!(
                                "record type {} is not a struct",
                                obj_type
                            )));
                        };
                        let i64_ty = self.context.i64_type();
                        // Sort fields alphabetically to match interpreter
                        let mut idx_map: Vec<(usize, &Field)> = fields.iter().enumerate().collect();
                        idx_map.sort_by(|a, b| a.1.name.cmp(&b.1.name));
                        let mut fmt = String::from("{");
                        let mut sprintf_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
                        for (pos, (i, field)) in idx_map.iter().enumerate() {
                            if pos > 0 {
                                fmt.push(',');
                            }
                            let gep = self
                                .gep()
                                .build_struct_gep(sty, struct_ptr, *i as u32, &field.name)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let ft = sty.get_field_type_at_index(*i as u32).ok_or_else(|| {
                                CompileError::LlvmError("missing field type".into())
                            })?;
                            let field_val =
                                self.build_load(ft, gep, &format!("load_{}", field.name))?;
                            match &field.ty {
                                Type::Name(n, _) if n == "string" => {
                                    fmt.push_str(&format!("\"{}\":\"%s\"", field.name));
                                    let sv = field_val.into_struct_value();
                                    let dp = self
                                        .builder
                                        .build_extract_value(
                                            inkwell::values::AggregateValueEnum::StructValue(sv),
                                            0,
                                            &format!("{}_data", field.name),
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                        .into_pointer_value();
                                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(dp));
                                }
                                Type::Name(n, _) if matches!(n.as_str(), "i32" | "i64") => {
                                    let is32 = n == "i32";
                                    if is32 {
                                        fmt.push_str(&format!("\"{}\":%d", field.name));
                                    } else {
                                        fmt.push_str(&format!("\"{}\":%ld", field.name));
                                    }
                                    sprintf_args.push(BasicMetadataValueEnum::IntValue(
                                        field_val.into_int_value(),
                                    ));
                                }
                                Type::Name(n, _) if n == "bool" => {
                                    fmt.push_str(&format!("\"{}\":%s", field.name));
                                    let iv = field_val.into_int_value();
                                    let true_global = self
                                        .builder
                                        .build_global_string_ptr(
                                            "true",
                                            &format!("{}_true", field.name),
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                    let false_global = self
                                        .builder
                                        .build_global_string_ptr(
                                            "false",
                                            &format!("{}_false", field.name),
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                    let zero = self.context.bool_type().const_int(0, false);
                                    let is_true = self
                                        .builder
                                        .build_int_compare(
                                            inkwell::IntPredicate::NE,
                                            iv,
                                            zero,
                                            &format!("{}_is_true", field.name),
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                    let true_ptr = true_global.as_pointer_value();
                                    let false_ptr = false_global.as_pointer_value();
                                    let selected = self
                                        .builder
                                        .build_select(
                                            is_true,
                                            true_ptr,
                                            false_ptr,
                                            &format!("{}_json", field.name),
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                    sprintf_args.push(BasicMetadataValueEnum::PointerValue(
                                        selected.into_pointer_value(),
                                    ));
                                }
                                Type::Name(n, _) if n == "f64" => {
                                    fmt.push_str(&format!("\"{}\":%g", field.name));
                                    sprintf_args.push(BasicMetadataValueEnum::FloatValue(
                                        field_val.into_float_value(),
                                    ));
                                }
                                _ => {
                                    return Err(CompileError::Generic(format!(
                                        "to_json: unsupported record field type for '{}' in {}",
                                        field.name, obj_type
                                    )))
                                }
                            }
                        }
                        fmt.push('}');
                        let malloc_fn = self
                            .module
                            .get_function("malloc")
                            .ok_or_else(|| CompileError::LlvmError("malloc not declared".into()))?;
                        let buf_size = i64_ty.const_int(4096, false);
                        let buf = self
                            .build_call(
                                malloc_fn,
                                &[BasicMetadataValueEnum::IntValue(buf_size)],
                                "record_json_malloc",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let fmt_ptr = self
                            .builder
                            .build_global_string_ptr(&fmt, "record_json_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let mut all_args = vec![BasicMetadataValueEnum::PointerValue(buf)];
                        all_args.push(BasicMetadataValueEnum::PointerValue(
                            fmt_ptr.as_pointer_value(),
                        ));
                        all_args.extend(sprintf_args);
                        let sprintf_fn = self.module.get_function("sprintf").ok_or_else(|| {
                            CompileError::LlvmError("sprintf not declared".into())
                        })?;
                        self.build_call(sprintf_fn, &all_args, "record_json_sprintf")?;
                        self.register_heap_alloc(buf);
                        return self.wrap_c_string(buf);
                    }
                }
            }
            // P0-3: for the print/println/eprintln family only, convert
            // boolean args to "true"/"false" string pointers before
            // handing them to the builtin dispatch. Other builtins
            // (e.g. atomic_bool_new) legitimately expect an i64, so the
            // conversion must stay scoped to print sinks.
            if matches!(name, "println" | "print" | "eprintln" | "format") {
                self.pending_print_arg_types = args
                    .iter()
                    .map(|a| self.infer_object_type(a, vars))
                    .collect();
                for (i, src) in args.iter().enumerate() {
                    if i >= metadata_args.len() {
                        break;
                    }
                    if let Some(replaced) = self.maybe_bool_to_string(
                        src,
                        match metadata_args[i] {
                            BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                            BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                            BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
                            BasicMetadataValueEnum::StructValue(sv) => sv.into(),
                            _ => continue,
                        },
                    ) {
                        metadata_args[i] = match replaced {
                            BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
                            BasicValueEnum::FloatValue(fv) => {
                                BasicMetadataValueEnum::FloatValue(fv)
                            }
                            BasicValueEnum::PointerValue(pv) => {
                                BasicMetadataValueEnum::PointerValue(pv)
                            }
                            BasicValueEnum::StructValue(sv) => {
                                BasicMetadataValueEnum::StructValue(sv)
                            }
                            _ => continue,
                        };
                    }
                }
            }
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

        self.maybe_wrap_string_args_for_call(name, args, &mut compiled_args)?;
        self.maybe_convert_list_args_to_values(name, &mut compiled_args)?;
        self.maybe_wrap_named_fn_args_to_closures(name, args, &mut compiled_args)?;

        metadata_args = compiled_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();

        if self.extern_func_defs.contains_key(name) {
            self.generate_extern_fn(name)?;
        }
        self.emit_named_call(name, args, &metadata_args, vars)
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
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // For non-generic functions, use the symbol as-is if it already exists.
        // Generic functions must go through compile_generic_func for monomorphization.
        let is_generic = self
            .func_defs
            .get(name)
            .is_some_and(|f| !f.generics.is_empty());
        if !is_generic {
            if let Some(function) = self.module.get_function(name) {
                return self.emit_function_call(function, name, metadata_args);
            }
        }

        let (mangled, callee_map) = if let Some(fdef) = self.func_defs.get(name) {
            if !fdef.generics.is_empty() {
                let mut callee_map: HashMap<String, Type> = HashMap::new();
                let generic_names: Vec<String> =
                    fdef.generics.iter().map(|gp| gp.name.clone()).collect();
                for (i, param) in fdef.params.iter().enumerate() {
                    if i >= args.len() {
                        break;
                    }
                    if let Some(arg_type) = self.expr_type_of(&args[i], vars) {
                        infer_generic_args(&param.ty, &arg_type, &generic_names, &mut callee_map);
                    }
                }
                // Fallback for simple direct generic params (e.g. `identity<T>(x: T)`)
                // when expr_type_of couldn't produce a type.
                for gp in &fdef.generics {
                    if !callee_map.contains_key(&gp.name) {
                        for (i, param) in fdef.params.iter().enumerate() {
                            if i >= args.len() {
                                break;
                            }
                            if Self::type_references_generic(&param.ty, &gp.name) {
                                if let Some(arg_type) = self.expr_type_of(&args[i], vars) {
                                    callee_map.insert(gp.name.clone(), arg_type);
                                    break;
                                }
                            }
                        }
                    }
                }
                (Self::mangle_name(name, &callee_map), callee_map)
            } else {
                (
                    Self::mangle_name(name, &self.type_map),
                    self.type_map.clone(),
                )
            }
        } else {
            (
                Self::mangle_name(name, &self.type_map),
                self.type_map.clone(),
            )
        };

        // Compile the specialized generic function on demand if it doesn't exist yet.
        if !callee_map.is_empty() {
            self.type_map = callee_map.clone();
        }
        if self.module.get_function(&mangled).is_none() {
            if let Some(fdef) = self.func_defs.get(name).cloned() {
                if !fdef.generics.is_empty() {
                    self.compile_generic_func(&fdef, &callee_map).map_err(|e| {
                        CompileError::Generic(format!(
                            "failed to monomorphize function '{}': {}",
                            name, e
                        ))
                    })?;
                }
            }
        }

        if let Some(function) = self.module.get_function(&mangled) {
            let call = self.build_call(function, metadata_args, "call")?;
            Ok(call_try_basic_value(&call)
                .unwrap_or(self.context.i64_type().const_int(0, false).into()))
        } else if let Some(value) = self.comptime_values.get(name).cloned() {
            // v0.28.21 — `comptime func` items are folded at codegen-start
            // and intentionally not compiled to LLVM IR. Look up the
            // pre-computed value here and emit a constant in its place.
            // No-arg `comptime func` is the only supported shape.
            if !metadata_args.is_empty() {
                return Err(format!(
                    "comptime function '{}' is no-arg only in v0.28.21; got {} args",
                    name,
                    metadata_args.len()
                )
                .into());
            }
            self.value_to_llvm_const(&value)
        } else {
            let msg = if self.comptime_func_names.contains(name) {
                format!("comptime function '{}' is compile-time only; its body could not be folded (missing from comptime_values cache)", name)
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
        // Allocate a slot for the struct in the entry block, store into it,
        // register the data slot so the loader sees the latest value at free
        // time. Return the loaded struct to the caller.
        let slot = self.build_entry_alloca(sty, "call_str_slot")?;
        self.build_store(slot, sv)?;
        if self
            .gep()
            .build_struct_gep(sty, slot, 0, "call_str_data_gep")
            .is_ok()
        {
            self.register_heap_slot(slot, sty, 0);
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

    /// Convert compiled arguments to the declared parameter types of a user-defined
    /// function. This mirrors the interpreter's implicit numeric coercion, so
    /// calls like `power(2, 10)` (where `power` expects `f64`) pass `2.0` and
    /// `10.0` to the generated function.
    fn coerce_args_to_param_types(
        &self,
        name: &str,
        args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let fdef = if let Some(f) = self.func_defs.get(name) {
            f.clone()
        } else {
            return Ok(());
        };
        for (i, param) in fdef.params.iter().enumerate() {
            if i >= args.len() {
                break;
            }
            if let Some(target) = self.llvm_type_for(&param.ty) {
                args[i] = self.adjust_int_val(args[i], target)?;
            }
        }
        Ok(())
    }

    /// Decide whether a user-defined function with the given name can plausibly
    /// accept these argument expressions. This is used when a builtin and a user
    /// function share a name (e.g. `contains`) to resolve the ambiguity created
    /// by flattening imported modules into a single namespace.
    fn user_func_signature_matches(&self, name: &str, args: &[Expr]) -> bool {
        let fdef = match self.func_defs.get(name) {
            Some(f) => f,
            None => return false,
        };
        for (i, param) in fdef.params.iter().enumerate() {
            if i >= args.len() {
                break;
            }
            let arg_ty = match self.expr_type_of(&args[i], &HashMap::new()) {
                Some(t) => t,
                None => continue,
            };
            // For concrete scalar parameter types, require an exact match.
            // Generic or complex parameter types are assumed compatible.
            let is_concrete_scalar = matches!(
                &param.ty,
                crate::ast::Type::Name(n, _)
                    if n == "string" || n == "i32" || n == "i64" || n == "f64" || n == "bool"
            );
            if is_concrete_scalar && arg_ty != param.ty {
                return false;
            }
        }
        true
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

    /// Serialize a `List<RecordType>` to JSON by generating a per-type element
    /// serializer function and calling `mimi_list_record_to_json` with a callback.
    pub(in crate::codegen) fn compile_record_list_to_json(
        &mut self,
        type_name: &str,
        fields: &[crate::ast::Field],
        list_alloca: &inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Create or reuse the element serializer function: i8*(i8*)
        let fn_name = format!("{}_to_json_elem", type_name);
        let elem_fn = if let Some(f) = self.module.get_function(&fn_name) {
            f
        } else {
            let fn_ty = i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
            let func =
                self.module
                    .add_function(&fn_name, fn_ty, Some(inkwell::module::Linkage::Internal));
            // Set up the function body
            let entry_bb = self.context.append_basic_block(func, "entry");
            // Save the current position
            let saved_block = self.builder.get_insert_block();
            self.builder.position_at_end(entry_bb);

            // Cast the input pointer to the struct type
            let llvm_ty = self.type_llvm[type_name];
            let BasicTypeEnum::StructType(sty) = llvm_ty else {
                panic!("type '{}' is not a struct", type_name)
            };
            let typed_ptr = self
                .builder
                .build_bit_cast(
                    func.get_nth_param(0)
                        .ok_or_else(|| CompileError::Generic("elem fn missing param 0".into()))?
                        .into_pointer_value(),
                    i8_ptr_ty,
                    "typed_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?
                .into_pointer_value();

            // Load the struct value
            let struct_val = self
                .builder
                .build_load(BasicTypeEnum::StructType(sty), typed_ptr, "elem_val")
                .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                .into_struct_value();

            // Get runtime functions
            let malloc_fn = self.get_runtime_fn("malloc")?;
            let sprintf_fn = self.get_runtime_fn("sprintf")?;

            // Sort fields alphabetically
            let mut idx_map: Vec<(usize, &crate::ast::Field)> = fields.iter().enumerate().collect();
            idx_map.sort_by(|a, b| a.1.name.cmp(&b.1.name));

            // Build format string and args
            let mut fmt = String::from("{");
            let mut sprintf_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
            for (pos, (i, field)) in idx_map.iter().enumerate() {
                if pos > 0 {
                    fmt.push(',');
                }
                let field_val = self
                    .builder
                    .build_extract_value(
                        inkwell::values::AggregateValueEnum::StructValue(struct_val),
                        *i as u32,
                        &field.name,
                    )
                    .map_err(|e| {
                        CompileError::LlvmError(format!("extract field {}: {}", field.name, e))
                    })?;
                match &field.ty {
                    Type::Name(n, _) if n == "string" => {
                        fmt.push_str(&format!("\"{}\":\"%s\"", field.name));
                        let sv = field_val.into_struct_value();
                        let dp = self
                            .builder
                            .build_extract_value(
                                inkwell::values::AggregateValueEnum::StructValue(sv),
                                0,
                                &format!("{}_data", field.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("extract str: {}", e)))?
                            .into_pointer_value();
                        sprintf_args.push(BasicMetadataValueEnum::PointerValue(dp));
                    }
                    Type::Name(n, _) if matches!(n.as_str(), "i32" | "i64") => {
                        fmt.push_str(&format!("\"{}\":%ld", field.name));
                        let iv = field_val.into_int_value();
                        if n == "i32" {
                            let ext = self
                                .builder
                                .build_int_z_extend(iv, i64_ty, &format!("{}_ext", field.name))
                                .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))?;
                            sprintf_args.push(BasicMetadataValueEnum::IntValue(ext));
                        } else {
                            sprintf_args.push(BasicMetadataValueEnum::IntValue(iv));
                        }
                    }
                    Type::Name(n, _) if n == "bool" => {
                        fmt.push_str(&format!("\"{}\":%s", field.name));
                        let iv = field_val.into_int_value();
                        let true_global = self
                            .builder
                            .build_global_string_ptr("true", &format!("{}_true", field.name))
                            .map_err(|e| CompileError::LlvmError(format!("true: {}", e)))?;
                        let false_global = self
                            .builder
                            .build_global_string_ptr("false", &format!("{}_false", field.name))
                            .map_err(|e| CompileError::LlvmError(format!("false: {}", e)))?;
                        let zero = self.context.bool_type().const_int(0, false);
                        let is_true = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                iv,
                                zero,
                                &format!("{}_is_true", field.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                        let selected = self
                            .builder
                            .build_select(
                                is_true,
                                true_global.as_pointer_value(),
                                false_global.as_pointer_value(),
                                &format!("{}_json", field.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("select: {}", e)))?;
                        sprintf_args.push(BasicMetadataValueEnum::PointerValue(
                            selected.into_pointer_value(),
                        ));
                    }
                    Type::Name(n, _) if n == "f64" => {
                        fmt.push_str(&format!("\"{}\":%g", field.name));
                        sprintf_args.push(BasicMetadataValueEnum::FloatValue(
                            field_val.into_float_value(),
                        ));
                    }
                    _ => {
                        return Err(CompileError::Generic(format!(
                            "unsupported field type {:?} for to_json",
                            field.ty
                        )));
                    }
                }
            }
            fmt.push('}');

            // Allocate buffer and sprintf
            let buf_size = i64_ty.const_int(4096, false);
            let buf = self
                .build_call(
                    malloc_fn,
                    &[BasicMetadataValueEnum::IntValue(buf_size)],
                    "elem_json_malloc",
                )?
                .try_as_basic_value_opt()
                .ok_or("malloc returned void")?
                .into_pointer_value();
            let fmt_ptr = self
                .builder
                .build_global_string_ptr(&fmt, "elem_json_fmt")
                .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
            let mut all_args = vec![BasicMetadataValueEnum::PointerValue(buf)];
            all_args.push(BasicMetadataValueEnum::PointerValue(
                fmt_ptr.as_pointer_value(),
            ));
            all_args.extend(sprintf_args);
            self.build_call(sprintf_fn, &all_args, "elem_json_sprintf")?;
            // Return the buffer pointer
            let ret_val: BasicValueEnum<'ctx> = buf.into();
            self.builder
                .build_return(Some(&ret_val))
                .map_err(|e| CompileError::LlvmError(format!("ret: {}", e)))?;

            // Restore the saved position
            if let Some(bb) = saved_block {
                self.builder.position_at_end(bb);
            }

            func
        };

        // Call mimi_list_record_to_json(list_alloca, elem_fn)
        let helper_name = "mimi_list_record_to_json";
        let helper_fn = self.module.get_function(helper_name).unwrap_or_else(|| {
            let fn_ty = i8_ptr_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                ],
                false,
            );
            self.module
                .add_function(helper_name, fn_ty, Some(inkwell::module::Linkage::External))
        });
        let elem_fn_ptr = elem_fn.as_global_value().as_pointer_value();
        let raw = self
            .build_call(
                helper_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(*list_alloca),
                    BasicMetadataValueEnum::PointerValue(elem_fn_ptr),
                ],
                "to_json_record_list",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_list_record_to_json returned void")?
            .into_pointer_value();
        self.register_heap_alloc(raw);
        self.wrap_c_string(raw)
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
