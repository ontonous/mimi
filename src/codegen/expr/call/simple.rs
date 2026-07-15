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
            .collect::<Result<Vec<_>, _>>()?;
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
        let call_args = self.maybe_pack_enum_ctor_args(&compiled_args, function)?;
        self.emit_direct_call(function, &call_args, "enum_ctor")
    }

    /// Coerce a compiled arg to the LLVM type expected by an enum-ctor payload
    /// field (or the sole packed param).
    ///
    /// - string field `{ptr,i64}` + raw `i8*` (string literal) → wrap via strlen
    /// - other struct field + alloca pointer → load by value
    /// - integer width mismatch → trunc/sext
    pub(in crate::codegen) fn coerce_value_to_expected_type(
        &self,
        arg: BasicValueEnum<'ctx>,
        expected: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (arg, expected) {
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                let fields = st.get_field_types();
                // Mimi string is { i8*, i64 }; List is { i64, i8* } — order differs.
                let is_string = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(
                        fields[1],
                        BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                    );
                if is_string {
                    self.wrap_raw_string_ptr(pv)
                } else {
                    self.build_load(
                        BasicTypeEnum::StructType(st),
                        pv,
                        "coerce_struct_load",
                    )
                }
            }
            (BasicValueEnum::IntValue(arg_iv), BasicTypeEnum::IntType(exp_it)) => {
                let arg_bw = arg_iv.get_type().get_bit_width();
                let exp_bw = exp_it.get_bit_width();
                if arg_bw == exp_bw {
                    Ok(arg)
                } else if arg_bw > exp_bw {
                    Ok(self
                        .builder
                        .build_int_truncate(arg_iv, exp_it, "coerce_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("arg trunc: {}", e)))?
                        .into())
                } else {
                    Ok(self
                        .builder
                        .build_int_s_extend(arg_iv, exp_it, "coerce_sext")
                        .map_err(|e| CompileError::LlvmError(format!("arg s_ext: {}", e)))?
                        .into())
                }
            }
            _ => Ok(arg),
        }
    }

    /// If an enum constructor expects a single packed struct (multi-field variant
    /// or single struct payload like `string` / `List<T>`), coerce each arg to the
    /// expected field type and pack. Single non-struct args pass through after coerce.
    pub(in crate::codegen) fn maybe_pack_enum_ctor_args(
        &mut self,
        compiled_args: &[BasicValueEnum<'ctx>],
        function: inkwell::values::FunctionValue<'ctx>,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CompileError> {
        if function.count_params() != 1 {
            return Ok(compiled_args.to_vec());
        }
        let param = function
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("expected at least one param".into()))?;
        let BasicValueEnum::StructValue(param_sv) = param else {
            // Primitive single payload (i32/f64/…): coerce width only.
            if compiled_args.len() == 1 {
                let expected = param.get_type();
                return Ok(vec![self.coerce_value_to_expected_type(compiled_args[0], expected)?]);
            }
            return Ok(compiled_args.to_vec());
        };
        let struct_ty = param_sv.get_type();
        let field_tys = struct_ty.get_field_types();

        if compiled_args.len() > 1 {
            // Multi-arg variant → one packed struct param.
            if field_tys.len() != compiled_args.len() {
                return Err(CompileError::LlvmError(format!(
                    "enum ctor pack: {} args for {}-field payload",
                    compiled_args.len(),
                    field_tys.len()
                )));
            }
            let mut struct_val = struct_ty.get_undef();
            for (i, arg) in compiled_args.iter().enumerate() {
                let coerced = self.coerce_value_to_expected_type(*arg, field_tys[i])?;
                let agg = self
                    .builder
                    .build_insert_value(struct_val, coerced, i as u32, "packed_field")
                    .map_err(|e| {
                        CompileError::LlvmError(format!("pack enum ctor arg {}: {}", i, e))
                    })?;
                struct_val = agg.into_struct_value();
            }
            return Ok(vec![BasicValueEnum::StructValue(struct_val)]);
        }

        // Single arg for struct payload (string, List, nested enum, …).
        if compiled_args.len() == 1 {
            let coerced = self.coerce_value_to_expected_type(
                compiled_args[0],
                BasicTypeEnum::StructType(struct_ty),
            )?;
            return Ok(vec![coerced]);
        }
        Ok(compiled_args.to_vec())
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
        let ordered = self.reorder_named_args(name, args)?;
        let mut compiled_args = self.compile_arg_values(&ordered, vars)?;
        // Use ordered exprs for list-mutation/borrow paths below.
        let args = ordered.as_slice();

        // v0.28.29 fix for mimichat gap #2: list-mutating builtins (`push`,
        // `pop`) take a `*List` pointer at the LLVM level. When the caller
        // passes a local `let l: List<T> = ...`, the alloca for `l` is the
        // authoritative location. Naively `compile_arg_values` would `load`
        // the struct out of the alloca, then `compile_push` would mutate a
        // freshly-allocated temporary — discarding the changes and leaving
        // `l.data` pointing at the (already-freed) pre-mutation buffer,
        // causing double free / SIGSEGV on the next push.
        //
        // For mutating list builtins whose var slot is a `{i64, ptr}` struct
        // (i.e. `let l: List<T> = from_json::<List<T>>(...)` where the codegen
        // store the list value-by-value), swap args[0] from the loaded
        // StructValue back to the original alloca pointer so the mutation
        // is visible. Skip the swap when the var is already a list pointer
        // (e.g. list literals — the loaded value is already a *List that
        // `require_list_pointer` returns as-is, which is the correct LLVM
        // pointer for gep against the list struct).
        if matches!(name, "push" | "pop") && !args.is_empty() {
            match &args[0] {
                Expr::Ident(var_name) => {
                    if self.is_list_type_name(&self.infer_object_type(&args[0], vars)) {
                        if let Some(&(alloca, var_ty)) = vars.get(var_name) {
                            if matches!(var_ty, BasicTypeEnum::StructType(_)) {
                                compiled_args[0] = BasicValueEnum::PointerValue(alloca);
                            }
                        }
                    }
                }
                // Handle self.field = push(self.field, val) — get GEP pointer to field slot
                Expr::Field(obj_expr, field_name) => {
                    if let Expr::Ident(obj_name) = obj_expr.as_ref() {
                        if obj_name == "self" {
                            if let Ok(field_gep) =
                                self.compile_field_gep(obj_expr, field_name, vars)
                            {
                                if self.is_list_type_name(&self.infer_object_type(&args[0], vars)) {
                                    compiled_args[0] = BasicValueEnum::PointerValue(field_gep);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        self.maybe_convert_callback_args(name, &mut compiled_args)?;
        self.maybe_load_reprc_struct_args_for_extern(name, &mut compiled_args)?;
        self.coerce_args_to_param_types(name, &mut compiled_args)?;

        // read_lines_each needs the closure struct (not metadata-only) to
        // build a void(char*) C thunk that re-wraps lines as Mimi strings.
        if name == "read_lines_each" {
            return self.compile_read_lines_each_call(&compiled_args);
        }

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
                // Product tuples: JSON array via recursive field serialization.
                // Only when the *source* type is a product tuple — never Option
                // `{i1,T}`, Result, enum `{i32,i64}`, string, or list layouts.
                if let BasicMetadataValueEnum::StructValue(sv) = metadata_args[0] {
                    let fields = sv.get_type().get_field_types();
                    let looks_like_option = !fields.is_empty()
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 1
                        );
                    let is_string = fields.len() == 2
                        && matches!(fields[0], BasicTypeEnum::PointerType(_))
                        && matches!(
                            fields[1],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        );
                    let is_list = fields.len() == 2
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        )
                        && matches!(fields[1], BasicTypeEnum::PointerType(_));
                    let is_enum_tag = fields.len() == 2
                        && matches!(
                            fields[0],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 32
                        )
                        && matches!(
                            fields[1],
                            BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                        );
                    let src_ty = self.infer_object_type(&args[0], vars);
                    let named_as_tuple = src_ty.starts_with('(') || src_ty.contains("Tuple");
                    // Prefer AST-ish type name when available; else multi-field
                    // product that is not option/string/list/enum.
                    if named_as_tuple
                        || (fields.len() >= 2
                            && !looks_like_option
                            && !is_string
                            && !is_list
                            && !is_enum_tag
                            && !src_ty.starts_with("Option")
                            && !src_ty.starts_with("Result")
                            && !src_ty.starts_with("List")
                            && !src_ty.starts_with("Map")
                            && !src_ty.starts_with("Set")
                            && self.type_defs.get(&src_ty).is_none())
                    {
                        let raw = self.emit_product_tuple_to_json(sv)?;
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                }
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
                    if inner.starts_with("List") {
                        // Nested List: use mimi_list_list_to_json + inner i64 list formatter.
                        // Outer list already stored in `alloca` above.
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let callback_fn_ty = i8_ptr_ty
                            .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                        let inner_fn = self
                            .module
                            .get_function("mimi_list_i64_to_json")
                            .unwrap_or_else(|| {
                                self.module.add_function(
                                    "mimi_list_i64_to_json",
                                    callback_fn_ty,
                                    Some(inkwell::module::Linkage::External),
                                )
                            });
                        let callback = inner_fn.as_global_value().as_pointer_value();
                        let fn_ty = i8_ptr_ty.fn_type(
                            &[
                                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                            ],
                            false,
                        );
                        let callee = self
                            .module
                            .get_function("mimi_list_list_to_json")
                            .unwrap_or_else(|| {
                                self.module.add_function(
                                    "mimi_list_list_to_json",
                                    fn_ty,
                                    Some(inkwell::module::Linkage::External),
                                )
                            });
                        let raw = self
                            .build_call(
                                callee,
                                &[
                                    BasicMetadataValueEnum::PointerValue(alloca),
                                    BasicMetadataValueEnum::PointerValue(callback),
                                ],
                                "to_json_list_list",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_list_list_to_json void")?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    let rt_fn_name = if inner.starts_with("Map") {
                        if inner.contains("Map<string, string>") {
                            "mimi_list_map_to_json_string"
                        } else {
                            // i32/i64/bool/f64 maps share int-style JSON objects.
                            "mimi_list_map_to_string"
                        }
                    } else if inner.starts_with("Set") {
                        "mimi_list_set_to_json"
                    } else if inner.starts_with("Option") && inner.contains("Map<") {
                        // List of Option of Map — use typed map helper.
                        let mode = if inner.contains("Map<string, string>") {
                            1i64
                        } else if inner.contains("Map<string, bool>") {
                            2
                        } else if inner.contains("Map<string, f64>")
                            || inner.contains("Map<string, f32>")
                        {
                            3
                        } else {
                            0
                        };
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let fn_ty = i8_ptr_ty.fn_type(
                            &[
                                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                                BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            ],
                            false,
                        );
                        let callee = self
                            .module
                            .get_function("mimi_list_option_map_to_json")
                            .unwrap_or_else(|| {
                                self.module.add_function(
                                    "mimi_list_option_map_to_json",
                                    fn_ty,
                                    Some(inkwell::module::Linkage::External),
                                )
                            });
                        let raw = self
                            .build_call(
                                callee,
                                &[
                                    BasicMetadataValueEnum::PointerValue(alloca),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "to_json_list_opt_map",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list option map to_json void")?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    } else if inner.starts_with("Option")
                        && (inner.contains('(')
                            || inner.contains("Tuple")
                            || self
                                .type_defs
                                .get(
                                    inner
                                        .strip_prefix("Option<")
                                        .and_then(|s| s.strip_suffix('>'))
                                        .unwrap_or(""),
                                )
                                .is_some_and(|td| {
                                    matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                }))
                    {
                        // List of Option of product-tuple / named record: full
                        // Option layout, not the scalar {i1,i64} runtime helper.
                        let raw = self.emit_list_option_product_tuple_to_json(alloca, inner)?;
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    } else if inner.starts_with("Option") {
                        "mimi_list_option_i64_to_json"
                    } else if inner.starts_with("Result") && inner.contains("Map<") {
                        // List of Result of Map — typed map Ok payload.
                        let mode = if inner.contains("Map<string, string>") {
                            1i64
                        } else if inner.contains("Map<string, bool>") {
                            2
                        } else if inner.contains("Map<string, f64>")
                            || inner.contains("Map<string, f32>")
                        {
                            3
                        } else {
                            0
                        };
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let fn_ty = i8_ptr_ty.fn_type(
                            &[
                                BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                                BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            ],
                            false,
                        );
                        let callee = self
                            .module
                            .get_function("mimi_list_result_map_to_json")
                            .unwrap_or_else(|| {
                                self.module.add_function(
                                    "mimi_list_result_map_to_json",
                                    fn_ty,
                                    Some(inkwell::module::Linkage::External),
                                )
                            });
                        let raw = self
                            .build_call(
                                callee,
                                &[
                                    BasicMetadataValueEnum::PointerValue(alloca),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "to_json_list_res_map",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list result map to_json void")?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    } else if inner.starts_with("Result")
                        && (inner.contains('(')
                            || inner.contains("Tuple")
                            || self
                                .type_defs
                                .get(
                                    inner
                                        .strip_prefix("Result<")
                                        .and_then(|s| s.split(',').next())
                                        .map(|s| s.trim())
                                        .unwrap_or(""),
                                )
                                .is_some_and(|td| {
                                    matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                }))
                    {
                        // List of Result of product-tuple / record: full layout.
                        let raw = self.emit_list_result_product_to_json(alloca, inner)?;
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    } else if inner.starts_with("Result") {
                        "mimi_list_result_i64_to_json"
                    } else if inner.starts_with('(') {
                        // List of product tuples: codegen loop → JSON array of arrays.
                        let raw = self.emit_list_product_tuple_to_json(alloca, inner)?;
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    } else {
                        match inner {
                            "string" => "mimi_list_str_to_json",
                            "f64" | "f32" => "mimi_list_f64_to_json",
                            "bool" => "mimi_list_bool_to_json",
                            _ => "mimi_list_i64_to_json",
                        }
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
                // Map / Map<string, …> → typed map JSON helpers
                if obj_type == "Map" || obj_type.starts_with("Map<") {
                    let handle = match &metadata_args[0] {
                        BasicMetadataValueEnum::IntValue(iv) => *iv,
                        BasicMetadataValueEnum::PointerValue(_) => {
                            return Err(CompileError::Generic(
                                "to_json: Map handle must be i64".into(),
                            ));
                        }
                        other => {
                            return Err(CompileError::Generic(format!(
                                "to_json: unexpected Map argument kind {:?}",
                                other
                            )))
                        }
                    };
                    let fn_name = if obj_type.contains("Map<string, string>") {
                        "mimi_map_to_json_string"
                    } else if obj_type.contains("Map<string, bool>") {
                        "mimi_map_to_json_bool"
                    } else if obj_type.contains("Map<string, f64>")
                        || obj_type.contains("Map<string, f32>")
                    {
                        "mimi_map_to_json_f64_serde"
                    } else {
                        "mimi_map_to_json_i64"
                    };
                    let func = self.get_runtime_fn(fn_name)?;
                    let raw = self
                        .build_call(
                            func,
                            &[BasicMetadataValueEnum::IntValue(handle)],
                            "to_json_map",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("map to_json returned void")?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return self.wrap_c_string(raw);
                }
                // Set / Set<…> → typed set JSON helpers
                if obj_type == "Set" || obj_type.starts_with("Set<") || obj_type == "set" {
                    let handle = match &metadata_args[0] {
                        BasicMetadataValueEnum::IntValue(iv) => *iv,
                        BasicMetadataValueEnum::PointerValue(_) => {
                            return Err(CompileError::Generic(
                                "to_json: Set handle must be i64".into(),
                            ));
                        }
                        other => {
                            return Err(CompileError::Generic(format!(
                                "to_json: unexpected Set argument kind {:?}",
                                other
                            )))
                        }
                    };
                    let fn_name = if obj_type.contains("Set<string>") {
                        "mimi_set_to_json_string"
                    } else if obj_type.contains("Set<bool>") {
                        "mimi_set_to_json_bool"
                    } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                        "mimi_set_to_json_f64"
                    } else {
                        "mimi_set_to_json_i64"
                    };
                    let func = self.get_runtime_fn(fn_name)?;
                    let raw = self
                        .build_call(
                            func,
                            &[BasicMetadataValueEnum::IntValue(handle)],
                            "to_json_set",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("set to_json returned void")?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return self.wrap_c_string(raw);
                }
                // Option / Option<T> with integer/handle payload: {i1,i64}
                // or by-value struct payload ({i1, tuple|record}).
                if obj_type == "Option" || obj_type.starts_with("Option<") {
                    let opt_load_sty = {
                        let parsed = crate::codegen::extract_list_elem_type(&format!(
                            "List<{}>",
                            obj_type
                        ));
                        // extract_list_elem_type("List<Option<P>>") → Option<P>
                        let opt_ty = parsed.unwrap_or_else(|| {
                            crate::ast::Type::Name("Option".into(), vec![crate::ast::Type::Name(
                                "i64".into(),
                                vec![],
                            )])
                        });
                        match self.llvm_type_for(&opt_ty) {
                            Some(BasicTypeEnum::StructType(s)) => s,
                            _ => self.context.struct_type(
                                &[
                                    self.context.bool_type().into(),
                                    self.context.i64_type().into(),
                                ],
                                false,
                            ),
                        }
                    };
                    let sv = match &metadata_args[0] {
                        BasicMetadataValueEnum::StructValue(s) => *s,
                        BasicMetadataValueEnum::PointerValue(pv) => {
                            let loaded = self
                                .builder
                                .build_load(
                                    BasicTypeEnum::StructType(opt_load_sty),
                                    *pv,
                                    "opt_load",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                .into_struct_value();
                            loaded
                        }
                        other => {
                            return Err(CompileError::Generic(format!(
                                "to_json: unexpected Option argument kind {:?}",
                                other
                            )))
                        }
                    };
                    let disc = self
                        .build_extract_value(sv.into(), 0, "opt_disc")?
                        .into_int_value();
                    let disc_i64 = self
                        .builder
                        .build_int_z_extend(disc, self.context.i64_type(), "opt_disc_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let payload_bv = self.build_extract_value(sv.into(), 1, "opt_payload")?;
                    // Option of Result by-value: payload is Result struct {i1,ok,err}.
                    if obj_type.contains("Result")
                        && matches!(payload_bv, BasicValueEnum::StructValue(_))
                    {
                        let res_sv = payload_bv.into_struct_value();
                        let r_disc = self
                            .build_extract_value(res_sv.into(), 0, "opt_res_disc")?
                            .into_int_value();
                        let r_disc_i64 = self
                            .builder
                            .build_int_z_extend(
                                r_disc,
                                self.context.i64_type(),
                                "opt_res_disc_i64",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let r_ok = self
                            .build_extract_value(res_sv.into(), 1, "opt_res_ok")?
                            .into_int_value();
                        let r_ok_i64 = if r_ok.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(
                                    r_ok,
                                    self.context.i64_type(),
                                    "opt_res_ok_i64",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            r_ok
                        };
                        let r_err = self
                            .build_extract_value(res_sv.into(), 2, "opt_res_err")?
                            .into_int_value();
                        let r_err_i64 = if r_err.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(
                                    r_err,
                                    self.context.i64_type(),
                                    "opt_res_err_i64",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            r_err
                        };
                        // Option of Result of Map/Set: Ok is a handle, not a plain i64.
                        let res_json = if obj_type.contains("Map<") {
                            let mode = if obj_type.contains("Map<string, string>") {
                                1i64
                            } else if obj_type.contains("Map<string, bool>") {
                                2
                            } else if obj_type.contains("Map<string, f64>")
                                || obj_type.contains("Map<string, f32>")
                            {
                                3
                            } else {
                                0
                            };
                            let res_fn = self.get_runtime_fn("mimi_result_map_to_json")?;
                            self.build_call(
                                res_fn,
                                &[
                                    BasicMetadataValueEnum::IntValue(r_disc_i64),
                                    BasicMetadataValueEnum::IntValue(r_ok_i64),
                                    BasicMetadataValueEnum::IntValue(r_err_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "opt_res_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("result map to_json void")?
                            .into_pointer_value()
                        } else if obj_type.contains("Set<") {
                            let mode = if obj_type.contains("Set<string>") {
                                1i64
                            } else if obj_type.contains("Set<bool>") {
                                2
                            } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>")
                            {
                                3
                            } else {
                                0
                            };
                            let res_fn = self.get_runtime_fn("mimi_result_set_to_json")?;
                            self.build_call(
                                res_fn,
                                &[
                                    BasicMetadataValueEnum::IntValue(r_disc_i64),
                                    BasicMetadataValueEnum::IntValue(r_ok_i64),
                                    BasicMetadataValueEnum::IntValue(r_err_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "opt_res_set_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("result set to_json void")?
                            .into_pointer_value()
                        } else {
                            let res_fn = self.get_runtime_fn("mimi_result_i64_to_json")?;
                            self.build_call(
                                res_fn,
                                &[
                                    BasicMetadataValueEnum::IntValue(r_disc_i64),
                                    BasicMetadataValueEnum::IntValue(r_ok_i64),
                                    BasicMetadataValueEnum::IntValue(r_err_i64),
                                ],
                                "opt_res_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("result to_json void")?
                            .into_pointer_value()
                        };
                        let disc_is_some = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "opt_res_is_some",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let some_bb =
                            self.context.append_basic_block(function, "toj_opt_res_some");
                        let none_bb =
                            self.context.append_basic_block(function, "toj_opt_res_none");
                        let merge_bb =
                            self.context.append_basic_block(function, "toj_opt_res_merge");
                        let i8_ptr_ty =
                            self.context.ptr_type(inkwell::AddressSpace::default());
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_opt_res_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_some, some_bb, none_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(some_bb);
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(512, false),
                            "opt_res_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr("{\"Some\":[%s]}", "opt_res_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(512, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(res_json),
                            ],
                            "opt_res_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(none_bb);
                        let none_heap = self.malloc_or_abort(
                            self.context.i64_type().const_int(8, false),
                            "opt_res_none_heap",
                        )?;
                        let none_lit = self
                            .builder
                            .build_global_string_ptr("\"None\"", "opt_res_none")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let strcpy_fn = self.get_runtime_fn("strcpy")?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(none_heap),
                                BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                            ],
                            "opt_res_none_cpy",
                        )?;
                        self.build_store(out_alloca, none_heap)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "opt_res_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    // Option of product tuple / named record: multi-field struct payload.
                    if let BasicValueEnum::StructValue(pay_sv) = payload_bv {
                        let pay_fields = pay_sv.get_type().get_field_types();
                        let pay_is_string = pay_fields.len() == 2
                            && matches!(pay_fields[0], BasicTypeEnum::PointerType(_))
                            && matches!(
                                pay_fields[1],
                                BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                            );
                        if !pay_is_string {
                            let mut inner_name = obj_type
                                .strip_prefix("Option<")
                                .and_then(|s| s.strip_suffix('>'))
                                .unwrap_or("")
                                .to_string();
                            // Bare `Option` (missing generic in var_type_names): recover
                            // named record from payload LLVM layout.
                            if inner_name.is_empty() || inner_name == "Option" {
                                let pay_sty = pay_sv.get_type();
                                for (n, ty) in &self.type_llvm {
                                    if matches!(ty, BasicTypeEnum::StructType(s) if *s == pay_sty)
                                        && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                            matches!(
                                                td.kind,
                                                crate::ast::TypeDefKind::Record(_)
                                            )
                                        })
                                    {
                                        inner_name = n.clone();
                                        break;
                                    }
                                }
                            }
                            let is_named_record = self.type_defs.get(&inner_name).is_some_and(|td| {
                                matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                            });
                            if is_named_record || pay_fields.len() >= 2 {
                                let pay_json = if is_named_record {
                                    let rec_ty = pay_sv.get_type();
                                    let rec_alloca = self.build_alloca(
                                        BasicTypeEnum::StructType(rec_ty),
                                        "opt_rec_tmp",
                                    )?;
                                    self.build_store(rec_alloca, pay_sv)?;
                                    self.compile_record_to_json_cstr(&inner_name, rec_alloca)?
                                } else {
                                    self.emit_product_tuple_to_json(pay_sv)?
                                };
                                let disc_is_some = self
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::NE,
                                        disc_i64,
                                        self.context.i64_type().const_int(0, false),
                                        "opt_tup_is_some",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let function = self.current_function().ok_or("no function")?;
                                let some_bb =
                                    self.context.append_basic_block(function, "toj_opt_tup_some");
                                let none_bb =
                                    self.context.append_basic_block(function, "toj_opt_tup_none");
                                let merge_bb =
                                    self.context.append_basic_block(function, "toj_opt_tup_merge");
                                let i8_ptr_ty =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let out_alloca = self.build_alloca(
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    "toj_opt_tup_out",
                                )?;
                                self.builder
                                    .build_conditional_branch(disc_is_some, some_bb, none_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(some_bb);
                                let buf = self.malloc_or_abort(
                                    self.context.i64_type().const_int(1024, false),
                                    "opt_tup_buf",
                                )?;
                                let fmt = self
                                    .builder
                                    .build_global_string_ptr("{\"Some\":[%s]}", "opt_tup_fmt")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                                self.build_call(
                                    snprintf_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(buf),
                                        BasicMetadataValueEnum::IntValue(
                                            self.context.i64_type().const_int(1024, false),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(
                                            fmt.as_pointer_value(),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(pay_json),
                                    ],
                                    "opt_tup_sn",
                                )?;
                                self.build_store(out_alloca, buf)?;
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(none_bb);
                                let none_heap = self.malloc_or_abort(
                                    self.context.i64_type().const_int(8, false),
                                    "opt_tup_none_heap",
                                )?;
                                let none_lit = self
                                    .builder
                                    .build_global_string_ptr("\"None\"", "opt_tup_none")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                                self.build_call(
                                    strcpy_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(none_heap),
                                        BasicMetadataValueEnum::PointerValue(
                                            none_lit.as_pointer_value(),
                                        ),
                                    ],
                                    "opt_tup_none_cpy",
                                )?;
                                self.build_store(out_alloca, none_heap)?;
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(merge_bb);
                                let raw = self
                                    .build_load(
                                        BasicTypeEnum::PointerType(i8_ptr_ty),
                                        out_alloca,
                                        "opt_tup_result",
                                    )?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return self.wrap_c_string(raw);
                            }
                        }
                    }
                    // Option of named record: pointer payload (Some stores stack
                    // alloca of record as ptr) or i64 ptrtoint.
                    if let Some(inner_name) = obj_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                    {
                        if self.type_defs.get(inner_name).is_some_and(|td| {
                            matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                        }) {
                            if let BasicValueEnum::PointerValue(rec_ptr) = payload_bv {
                                let disc_is_some = self
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::NE,
                                        disc_i64,
                                        self.context.i64_type().const_int(0, false),
                                        "opt_rec_ptr_is_some",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let function = self.current_function().ok_or("no function")?;
                                let some_bb = self
                                    .context
                                    .append_basic_block(function, "toj_opt_rec_ptr_some");
                                let none_bb = self
                                    .context
                                    .append_basic_block(function, "toj_opt_rec_ptr_none");
                                let merge_bb = self
                                    .context
                                    .append_basic_block(function, "toj_opt_rec_ptr_merge");
                                let i8_ptr_ty =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let out_alloca = self.build_alloca(
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    "toj_opt_rec_ptr_out",
                                )?;
                                self.builder
                                    .build_conditional_branch(disc_is_some, some_bb, none_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(some_bb);
                                let rec_json =
                                    self.compile_record_to_json_cstr(inner_name, rec_ptr)?;
                                let buf = self.malloc_or_abort(
                                    self.context.i64_type().const_int(1024, false),
                                    "opt_rec_ptr_buf",
                                )?;
                                let fmt = self
                                    .builder
                                    .build_global_string_ptr(
                                        "{\"Some\":[%s]}",
                                        "opt_rec_ptr_fmt",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                                self.build_call(
                                    snprintf_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(buf),
                                        BasicMetadataValueEnum::IntValue(
                                            self.context.i64_type().const_int(1024, false),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(
                                            fmt.as_pointer_value(),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(rec_json),
                                    ],
                                    "opt_rec_ptr_sn",
                                )?;
                                self.build_store(out_alloca, buf)?;
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(none_bb);
                                let none_heap = self.malloc_or_abort(
                                    self.context.i64_type().const_int(8, false),
                                    "opt_rec_ptr_none",
                                )?;
                                let none_lit = self
                                    .builder
                                    .build_global_string_ptr("\"None\"", "opt_rec_ptr_none_lit")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                                self.build_call(
                                    strcpy_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(none_heap),
                                        BasicMetadataValueEnum::PointerValue(
                                            none_lit.as_pointer_value(),
                                        ),
                                    ],
                                    "opt_rec_ptr_none_cpy",
                                )?;
                                self.build_store(out_alloca, none_heap)?;
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(merge_bb);
                                let raw = self
                                    .build_load(
                                        BasicTypeEnum::PointerType(i8_ptr_ty),
                                        out_alloca,
                                        "opt_rec_ptr_result",
                                    )?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return self.wrap_c_string(raw);
                            }
                            if let BasicValueEnum::IntValue(pay_iv) = payload_bv {
                                let i8_ptr_ty =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let pay_i64 = if pay_iv.get_type().get_bit_width() < 64 {
                                    self.builder
                                        .build_int_s_extend(
                                            pay_iv,
                                            self.context.i64_type(),
                                            "opt_rec_pay_i64",
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                } else {
                                    pay_iv
                                };
                                let disc_is_some = self
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::NE,
                                        disc_i64,
                                        self.context.i64_type().const_int(0, false),
                                        "opt_rec_is_some",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let function = self.current_function().ok_or("no function")?;
                                let some_bb = self
                                    .context
                                    .append_basic_block(function, "toj_opt_rec_some");
                                let none_bb = self
                                    .context
                                    .append_basic_block(function, "toj_opt_rec_none");
                                let merge_bb = self
                                    .context
                                    .append_basic_block(function, "toj_opt_rec_merge");
                                let out_alloca = self.build_alloca(
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    "toj_opt_rec_out",
                                )?;
                                self.builder
                                    .build_conditional_branch(disc_is_some, some_bb, none_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(some_bb);
                                let rec_ptr = self
                                    .builder
                                    .build_int_to_ptr(pay_i64, i8_ptr_ty, "opt_rec_ptr")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let rec_json =
                                    self.compile_record_to_json_cstr(inner_name, rec_ptr)?;
                                let buf = self.malloc_or_abort(
                                    self.context.i64_type().const_int(1024, false),
                                    "opt_rec_buf",
                                )?;
                                let fmt = self
                                    .builder
                                    .build_global_string_ptr("{\"Some\":[%s]}", "opt_rec_fmt")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                                self.build_call(
                                    snprintf_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(buf),
                                        BasicMetadataValueEnum::IntValue(
                                            self.context.i64_type().const_int(1024, false),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(
                                            fmt.as_pointer_value(),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(rec_json),
                                    ],
                                    "opt_rec_sn",
                                )?;
                                self.build_store(out_alloca, buf)?;
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(none_bb);
                                let none_heap = self.malloc_or_abort(
                                    self.context.i64_type().const_int(8, false),
                                    "opt_rec_none_heap",
                                )?;
                                let none_lit = self
                                    .builder
                                    .build_global_string_ptr("\"None\"", "opt_rec_none")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                                self.build_call(
                                    strcpy_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(none_heap),
                                        BasicMetadataValueEnum::PointerValue(
                                            none_lit.as_pointer_value(),
                                        ),
                                    ],
                                    "opt_rec_none_cpy",
                                )?;
                                self.build_store(out_alloca, none_heap)?;
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(merge_bb);
                                let raw = self
                                    .build_load(
                                        BasicTypeEnum::PointerType(i8_ptr_ty),
                                        out_alloca,
                                        "opt_rec_result",
                                    )?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return self.wrap_c_string(raw);
                            }
                        }
                    }
                    let payload_i64 = match payload_bv {
                        BasicValueEnum::IntValue(iv) => {
                            if iv.get_type().get_bit_width() < 64 {
                                self.builder
                                    .build_int_s_extend(
                                        iv,
                                        self.context.i64_type(),
                                        "opt_pay_i64",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            } else {
                                iv
                            }
                        }
                        BasicValueEnum::PointerValue(pv) => self
                            .builder
                            .build_ptr_to_int(pv, self.context.i64_type(), "opt_pay_ptr")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                        other => {
                            return Err(CompileError::Generic(format!(
                                "to_json Option: unexpected payload {:?}",
                                other.get_type()
                            )));
                        }
                    };
                    if obj_type
                        .strip_prefix("Option<")
                        .and_then(|s| s.strip_suffix('>'))
                        .is_some_and(|inner| inner.starts_with("Option"))
                    {
                        // Nested Option: payload is ptrtoint of heap Option {i1,i64}.
                        // mimi_option_i64_to_json only handles int payloads — rebuild.
                        let disc_is_some = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "opt_nest_some",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let some_bb =
                            self.context.append_basic_block(function, "toj_opt_nest_some");
                        let none_bb =
                            self.context.append_basic_block(function, "toj_opt_nest_none");
                        let merge_bb =
                            self.context.append_basic_block(function, "toj_opt_nest_merge");
                        let i8_ptr_ty =
                            self.context.ptr_type(inkwell::AddressSpace::default());
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_opt_nest_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_some, some_bb, none_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(some_bb);
                        let nested_ptr = self
                            .builder
                            .build_int_to_ptr(payload_i64, i8_ptr_ty, "opt_nest_ptr")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let opt_sty = self.context.struct_type(
                            &[
                                self.context.bool_type().into(),
                                self.context.i64_type().into(),
                            ],
                            false,
                        );
                        let nested_sv = self
                            .builder
                            .build_load(
                                BasicTypeEnum::StructType(opt_sty),
                                nested_ptr,
                                "opt_nest_ld",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            .into_struct_value();
                        let n_disc = self
                            .build_extract_value(nested_sv.into(), 0, "n_disc")?
                            .into_int_value();
                        let n_disc_i64 = self
                            .builder
                            .build_int_z_extend(
                                n_disc,
                                self.context.i64_type(),
                                "n_disc_i64",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let n_pay = self
                            .build_extract_value(nested_sv.into(), 1, "n_pay")?
                            .into_int_value();
                        let n_pay_i64 = if n_pay.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(
                                    n_pay,
                                    self.context.i64_type(),
                                    "n_pay_i64",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            n_pay
                        };
                        let func = self.get_runtime_fn("mimi_option_i64_to_json")?;
                        let inner_json = self
                            .build_call(
                                func,
                                &[
                                    BasicMetadataValueEnum::IntValue(n_disc_i64),
                                    BasicMetadataValueEnum::IntValue(n_pay_i64),
                                ],
                                "opt_nest_inner_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("option to_json void")?
                            .into_pointer_value();
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(512, false),
                            "opt_nest_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr(
                                "{\"Some\":[%s]}",
                                "opt_nest_fmt",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(512, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(inner_json),
                            ],
                            "opt_nest_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(none_bb);
                        // Heap-copy "None" so wrap_c_string free is always valid.
                        let none_heap = self.malloc_or_abort(
                            self.context.i64_type().const_int(8, false),
                            "opt_nest_none_heap",
                        )?;
                        let none_lit = self
                            .builder
                            .build_global_string_ptr("\"None\"", "opt_nest_none")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let strcpy_fn = self.get_runtime_fn("strcpy")?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(none_heap),
                                BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                            ],
                            "opt_nest_none_cpy",
                        )?;
                        self.build_store(out_alloca, none_heap)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "opt_nest_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    if obj_type.contains("List<") {
                        // Option of List: payload is pointer to list struct
                        // (or ptrtoint of it). Element type may be Map/Set/scalar.
                        let disc_is_some = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "opt_list_some",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let some_bb =
                            self.context.append_basic_block(function, "toj_opt_list_some");
                        let none_bb =
                            self.context.append_basic_block(function, "toj_opt_list_none");
                        let merge_bb =
                            self.context.append_basic_block(function, "toj_opt_list_merge");
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(
                                self.context.ptr_type(inkwell::AddressSpace::default()),
                            ),
                            "toj_opt_list_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_some, some_bb, none_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(some_bb);
                        let list_ptr = self
                            .builder
                            .build_int_to_ptr(
                                payload_i64,
                                self.context.ptr_type(inkwell::AddressSpace::default()),
                                "opt_list_as_ptr",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let list_fn_name = if obj_type.contains("List<Map")
                            || obj_type.contains("List<Map<")
                        {
                            if obj_type.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            }
                        } else if obj_type.contains("List<Set") {
                            "mimi_list_set_to_json"
                        } else if obj_type.contains("List<string>") {
                            "mimi_list_str_to_json"
                        } else if obj_type.contains("List<f64>") || obj_type.contains("List<f32>") {
                            "mimi_list_f64_to_json"
                        } else if obj_type.contains("List<bool>") {
                            "mimi_list_bool_to_json"
                        } else {
                            "mimi_list_i64_to_json"
                        };
                        let list_fn_ty =
                            i8_ptr_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                        let list_fn = self.module.get_function(list_fn_name).unwrap_or_else(|| {
                            self.module.add_function(
                                list_fn_name,
                                list_fn_ty,
                                Some(inkwell::module::Linkage::External),
                            )
                        });
                        let list_json = self
                            .build_call(
                                list_fn,
                                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                "opt_list_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list to_json void")?
                            .into_pointer_value();
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(4096, false),
                            "opt_list_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr(
                                "{\"Some\":[%s]}",
                                "opt_list_fmt",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(4096, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(list_json),
                            ],
                            "opt_list_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(none_bb);
                        let none_heap = self.malloc_or_abort(
                            self.context.i64_type().const_int(8, false),
                            "opt_list_none_heap",
                        )?;
                        let none_lit = self
                            .builder
                            .build_global_string_ptr("\"None\"", "opt_list_none")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let strcpy_fn = self.get_runtime_fn("strcpy")?;
                        self.build_call(
                            strcpy_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(none_heap),
                                BasicMetadataValueEnum::PointerValue(none_lit.as_pointer_value()),
                            ],
                            "opt_list_none_cpy",
                        )?;
                        self.build_store(out_alloca, none_heap)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "opt_list_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    if obj_type.contains("Map<") {
                        let mode = if obj_type.contains("Map<string, string>") {
                            1i64
                        } else if obj_type.contains("Map<string, bool>") {
                            2
                        } else if obj_type.contains("Map<string, f64>")
                            || obj_type.contains("Map<string, f32>")
                        {
                            3
                        } else {
                            0
                        };
                        let func = self.get_runtime_fn("mimi_option_map_to_json")?;
                        let raw = self
                            .build_call(
                                func,
                                &[
                                    BasicMetadataValueEnum::IntValue(disc_i64),
                                    BasicMetadataValueEnum::IntValue(payload_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "to_json_opt_map",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_option_map_to_json void")?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    if obj_type.contains("Set<") {
                        let mode = if obj_type.contains("Set<string>") {
                            1i64
                        } else if obj_type.contains("Set<bool>") {
                            2
                        } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                            3
                        } else {
                            0
                        };
                        let func = self.get_runtime_fn("mimi_option_set_to_json")?;
                        let raw = self
                            .build_call(
                                func,
                                &[
                                    BasicMetadataValueEnum::IntValue(disc_i64),
                                    BasicMetadataValueEnum::IntValue(payload_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "to_json_opt_set",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_option_set_to_json void")?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    let func = self.get_runtime_fn("mimi_option_i64_to_json")?;
                    let raw = self
                        .build_call(
                            func,
                            &[
                                BasicMetadataValueEnum::IntValue(disc_i64),
                                BasicMetadataValueEnum::IntValue(payload_i64),
                            ],
                            "to_json_opt",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_option_i64_to_json void")?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return self.wrap_c_string(raw);
                }
                // Result / Result<T,E> integer payloads: {i1, ok, err}
                if obj_type == "Result" || obj_type.starts_with("Result<") {
                    let sv = match &metadata_args[0] {
                        BasicMetadataValueEnum::StructValue(s) => *s,
                        BasicMetadataValueEnum::PointerValue(pv) => {
                            let loaded = self
                                .builder
                                .build_load(
                                    BasicTypeEnum::StructType(
                                        self.context.struct_type(
                                            &[
                                                self.context.bool_type().into(),
                                                self.context.i64_type().into(),
                                                self.context.i64_type().into(),
                                            ],
                                            false,
                                        ),
                                    ),
                                    *pv,
                                    "res_load",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                .into_struct_value();
                            loaded
                        }
                        other => {
                            return Err(CompileError::Generic(format!(
                                "to_json: unexpected Result argument kind {:?}",
                                other
                            )))
                        }
                    };
                    let disc = self
                        .build_extract_value(sv.into(), 0, "res_disc")?
                        .into_int_value();
                    let disc_i64 = self
                        .builder
                        .build_int_z_extend(disc, self.context.i64_type(), "res_disc_i64")
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let ok_bv = self.build_extract_value(sv.into(), 1, "res_ok")?;
                    // Result of Option: Ok is nested Option struct {i1, payload}.
                    if obj_type.contains("Option")
                        && matches!(ok_bv, BasicValueEnum::StructValue(_))
                    {
                        let opt_sv = ok_bv.into_struct_value();
                        let o_disc = self
                            .build_extract_value(opt_sv.into(), 0, "res_opt_disc")?
                            .into_int_value();
                        let o_disc_i64 = self
                            .builder
                            .build_int_z_extend(
                                o_disc,
                                self.context.i64_type(),
                                "res_opt_disc_i64",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let o_pay_bv = self.build_extract_value(opt_sv.into(), 1, "res_opt_pay")?;
                        // Option of product-tuple/record inside Result: rebuild
                        // {"Some":[…]} from struct payload rather than i64 helper.
                        if let BasicValueEnum::StructValue(pay_sv) = o_pay_bv {
                            let pay_fields = pay_sv.get_type().get_field_types();
                            let pay_is_string = pay_fields.len() == 2
                                && matches!(pay_fields[0], BasicTypeEnum::PointerType(_))
                                && matches!(
                                    pay_fields[1],
                                    BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                                );
                            if !pay_is_string && pay_fields.len() >= 2 {
                                let pay_json = self.emit_product_tuple_to_json(pay_sv)?;
                                let disc_is_ok = self
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::NE,
                                        disc_i64,
                                        self.context.i64_type().const_int(0, false),
                                        "res_opt_tup_is_ok",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let o_is_some = self
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::NE,
                                        o_disc_i64,
                                        self.context.i64_type().const_int(0, false),
                                        "res_opt_tup_is_some",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let function = self.current_function().ok_or("no function")?;
                                let ok_bb = self
                                    .context
                                    .append_basic_block(function, "toj_res_opt_tup_ok");
                                let err_bb = self
                                    .context
                                    .append_basic_block(function, "toj_res_opt_tup_err");
                                let merge_bb = self
                                    .context
                                    .append_basic_block(function, "toj_res_opt_tup_merge");
                                let i8_ptr_ty =
                                    self.context.ptr_type(inkwell::AddressSpace::default());
                                let out_alloca = self.build_alloca(
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    "toj_res_opt_tup_out",
                                )?;
                                self.builder
                                    .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(ok_bb);
                                let some_bb = self
                                    .context
                                    .append_basic_block(function, "toj_res_opt_tup_some");
                                let none_bb = self
                                    .context
                                    .append_basic_block(function, "toj_res_opt_tup_none");
                                let ok_merge = self
                                    .context
                                    .append_basic_block(function, "toj_res_opt_tup_ok_m");
                                self.builder
                                    .build_conditional_branch(o_is_some, some_bb, none_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(some_bb);
                                let inner_buf = self.malloc_or_abort(
                                    self.context.i64_type().const_int(1024, false),
                                    "res_opt_tup_inner",
                                )?;
                                let ifmt = self
                                    .builder
                                    .build_global_string_ptr(
                                        "{\"Some\":[%s]}",
                                        "res_opt_tup_ifmt",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let snprintf_fn = self.get_runtime_fn("snprintf")?;
                                self.build_call(
                                    snprintf_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(inner_buf),
                                        BasicMetadataValueEnum::IntValue(
                                            self.context.i64_type().const_int(1024, false),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(
                                            ifmt.as_pointer_value(),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(pay_json),
                                    ],
                                    "res_opt_tup_isn",
                                )?;
                                let outer_buf = self.malloc_or_abort(
                                    self.context.i64_type().const_int(1024, false),
                                    "res_opt_tup_outer",
                                )?;
                                let ofmt = self
                                    .builder
                                    .build_global_string_ptr("{\"Ok\":[%s]}", "res_opt_tup_ofmt")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.build_call(
                                    snprintf_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(outer_buf),
                                        BasicMetadataValueEnum::IntValue(
                                            self.context.i64_type().const_int(1024, false),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(
                                            ofmt.as_pointer_value(),
                                        ),
                                        BasicMetadataValueEnum::PointerValue(inner_buf),
                                    ],
                                    "res_opt_tup_osn",
                                )?;
                                self.build_store(out_alloca, outer_buf)?;
                                self.builder
                                    .build_unconditional_branch(ok_merge)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(none_bb);
                                let none_wrap = self.malloc_or_abort(
                                    self.context.i64_type().const_int(32, false),
                                    "res_opt_tup_none",
                                )?;
                                let nfmt = self
                                    .builder
                                    .build_global_string_ptr(
                                        "{\"Ok\":[\"None\"]}",
                                        "res_opt_tup_nfmt",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                let strcpy_fn = self.get_runtime_fn("strcpy")?;
                                self.build_call(
                                    strcpy_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(none_wrap),
                                        BasicMetadataValueEnum::PointerValue(
                                            nfmt.as_pointer_value(),
                                        ),
                                    ],
                                    "res_opt_tup_ncpy",
                                )?;
                                self.build_store(out_alloca, none_wrap)?;
                                self.builder
                                    .build_unconditional_branch(ok_merge)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(ok_merge);
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(err_bb);
                                let ebuf = self.malloc_or_abort(
                                    self.context.i64_type().const_int(32, false),
                                    "res_opt_tup_err",
                                )?;
                                let efmt = self
                                    .builder
                                    .build_global_string_ptr("{\"Err\":[0]}", "res_opt_tup_efmt")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.build_call(
                                    strcpy_fn,
                                    &[
                                        BasicMetadataValueEnum::PointerValue(ebuf),
                                        BasicMetadataValueEnum::PointerValue(
                                            efmt.as_pointer_value(),
                                        ),
                                    ],
                                    "res_opt_tup_ecpy",
                                )?;
                                self.build_store(out_alloca, ebuf)?;
                                self.builder
                                    .build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                                self.builder.position_at_end(merge_bb);
                                let raw = self
                                    .build_load(
                                        BasicTypeEnum::PointerType(i8_ptr_ty),
                                        out_alloca,
                                        "res_opt_tup_result",
                                    )?
                                    .into_pointer_value();
                                self.register_heap_alloc(raw);
                                return self.wrap_c_string(raw);
                            }
                        }
                        let o_pay = match o_pay_bv {
                            BasicValueEnum::IntValue(iv) => iv,
                            BasicValueEnum::PointerValue(pv) => self
                                .builder
                                .build_ptr_to_int(
                                    pv,
                                    self.context.i64_type(),
                                    "res_opt_pay_ptr",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                            BasicValueEnum::StructValue(_) => {
                                // Nested Option/List heap-packed as struct — already
                                // handled above for multi-field product; treat as 0.
                                self.context.i64_type().const_int(0, false)
                            }
                            other => {
                                return Err(CompileError::Generic(format!(
                                    "to_json Result Option: unexpected pay {:?}",
                                    other.get_type()
                                )));
                            }
                        };
                        let o_pay_i64 = if o_pay.get_type().get_bit_width() < 64 {
                            self.builder
                                .build_int_s_extend(
                                    o_pay,
                                    self.context.i64_type(),
                                    "res_opt_pay_i64",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?
                        } else {
                            o_pay
                        };
                        // Nested Option of Map/Set/List needs typed helpers.
                        let opt_json = if obj_type.contains("Map<") {
                            let mode = if obj_type.contains("Map<string, string>") {
                                1i64
                            } else if obj_type.contains("Map<string, bool>") {
                                2
                            } else if obj_type.contains("Map<string, f64>")
                                || obj_type.contains("Map<string, f32>")
                            {
                                3
                            } else {
                                0
                            };
                            let opt_fn = self.get_runtime_fn("mimi_option_map_to_json")?;
                            self.build_call(
                                opt_fn,
                                &[
                                    BasicMetadataValueEnum::IntValue(o_disc_i64),
                                    BasicMetadataValueEnum::IntValue(o_pay_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "res_opt_map_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("option map to_json void")?
                            .into_pointer_value()
                        } else if obj_type.contains("Set<") {
                            let mode = if obj_type.contains("Set<string>") {
                                1i64
                            } else if obj_type.contains("Set<bool>") {
                                2
                            } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>")
                            {
                                3
                            } else {
                                0
                            };
                            let opt_fn = self.get_runtime_fn("mimi_option_set_to_json")?;
                            self.build_call(
                                opt_fn,
                                &[
                                    BasicMetadataValueEnum::IntValue(o_disc_i64),
                                    BasicMetadataValueEnum::IntValue(o_pay_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "res_opt_set_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("option set to_json void")?
                            .into_pointer_value()
                        } else if obj_type.contains("List<") {
                            // Option of List: rebuild {"Some":[list_json]} / "None".
                            let disc_is_some = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    o_disc_i64,
                                    self.context.i64_type().const_int(0, false),
                                    "res_opt_list_some",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let function = self.current_function().ok_or("no function")?;
                            let some_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_list_some");
                            let none_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_list_none");
                            let merge_bb = self
                                .context
                                .append_basic_block(function, "toj_res_opt_list_merge");
                            let i8_ptr_ty =
                                self.context.ptr_type(inkwell::AddressSpace::default());
                            let out_alloca = self.build_alloca(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                "toj_res_opt_list_out",
                            )?;
                            self.builder
                                .build_conditional_branch(disc_is_some, some_bb, none_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(some_bb);
                            let list_ptr = self
                                .builder
                                .build_int_to_ptr(o_pay_i64, i8_ptr_ty, "res_opt_list_ptr")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let list_fn_name = if obj_type.contains("List<Map") {
                                if obj_type.contains("Map<string, string>") {
                                    "mimi_list_map_to_json_string"
                                } else {
                                    "mimi_list_map_to_string"
                                }
                            } else if obj_type.contains("List<string>") {
                                "mimi_list_str_to_json"
                            } else if obj_type.contains("List<f64>") || obj_type.contains("List<f32>")
                            {
                                "mimi_list_f64_to_json"
                            } else if obj_type.contains("List<bool>") {
                                "mimi_list_bool_to_json"
                            } else {
                                "mimi_list_i64_to_json"
                            };
                            let list_fn_ty = i8_ptr_ty
                                .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                            let list_fn =
                                self.module.get_function(list_fn_name).unwrap_or_else(|| {
                                    self.module.add_function(
                                        list_fn_name,
                                        list_fn_ty,
                                        Some(inkwell::module::Linkage::External),
                                    )
                                });
                            let list_json = self
                                .build_call(
                                    list_fn,
                                    &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                    "res_opt_list_json",
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("list to_json void")?
                                .into_pointer_value();
                            let buf = self.malloc_or_abort(
                                self.context.i64_type().const_int(4096, false),
                                "res_opt_list_buf",
                            )?;
                            let fmt = self
                                .builder
                                .build_global_string_ptr(
                                    "{\"Some\":[%s]}",
                                    "res_opt_list_fmt",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let snprintf_fn = self.get_runtime_fn("snprintf")?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(buf),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(4096, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(list_json),
                                ],
                                "res_opt_list_sn",
                            )?;
                            self.build_store(out_alloca, buf)?;
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(none_bb);
                            let none_heap = self.malloc_or_abort(
                                self.context.i64_type().const_int(8, false),
                                "res_opt_list_none",
                            )?;
                            let none_lit = self
                                .builder
                                .build_global_string_ptr("\"None\"", "res_opt_list_none_lit")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let strcpy_fn = self.get_runtime_fn("strcpy")?;
                            self.build_call(
                                strcpy_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(none_heap),
                                    BasicMetadataValueEnum::PointerValue(
                                        none_lit.as_pointer_value(),
                                    ),
                                ],
                                "res_opt_list_none_cpy",
                            )?;
                            self.build_store(out_alloca, none_heap)?;
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(merge_bb);
                            self.build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "res_opt_list_result",
                            )?
                            .into_pointer_value()
                        } else {
                            let opt_fn = self.get_runtime_fn("mimi_option_i64_to_json")?;
                            self.build_call(
                                opt_fn,
                                &[
                                    BasicMetadataValueEnum::IntValue(o_disc_i64),
                                    BasicMetadataValueEnum::IntValue(o_pay_i64),
                                ],
                                "res_opt_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("option to_json void")?
                            .into_pointer_value()
                        };
                        let disc_is_ok = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "res_opt_is_ok",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let ok_bb =
                            self.context.append_basic_block(function, "toj_res_opt_ok");
                        let err_bb =
                            self.context.append_basic_block(function, "toj_res_opt_err");
                        let merge_bb =
                            self.context.append_basic_block(function, "toj_res_opt_merge");
                        let i8_ptr_ty =
                            self.context.ptr_type(inkwell::AddressSpace::default());
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_res_opt_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(ok_bb);
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(512, false),
                            "res_opt_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr("{\"Ok\":[%s]}", "res_opt_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(512, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(opt_json),
                            ],
                            "res_opt_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(err_bb);
                        let err_bv = self.build_extract_value(sv.into(), 2, "res_opt_err")?;
                        let err_i64 = match err_bv {
                            BasicValueEnum::IntValue(iv) => {
                                if iv.get_type().get_bit_width() < 64 {
                                    self.builder
                                        .build_int_s_extend(
                                            iv,
                                            self.context.i64_type(),
                                            "res_opt_err_i64",
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                } else {
                                    iv
                                }
                            }
                            _ => self.context.i64_type().const_int(0, false),
                        };
                        let ebuf = self.malloc_or_abort(
                            self.context.i64_type().const_int(128, false),
                            "res_opt_ebuf",
                        )?;
                        let efmt = self
                            .builder
                            .build_global_string_ptr("{\"Err\":[%ld]}", "res_opt_efmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(ebuf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(128, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(efmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(err_i64),
                            ],
                            "res_opt_esn",
                        )?;
                        self.build_store(out_alloca, ebuf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "res_opt_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    // Result of List: Ok may be by-value list struct {i64,ptr}
                    // or a pointer/int handle — handle before scalar ok_i64 coercion.
                    if obj_type.contains("List<") {
                        let err_bv = self.build_extract_value(sv.into(), 2, "res_list_err")?;
                        let err_i64 = match err_bv {
                            BasicValueEnum::IntValue(iv) => {
                                if iv.get_type().get_bit_width() < 64 {
                                    self.builder
                                        .build_int_s_extend(
                                            iv,
                                            self.context.i64_type(),
                                            "res_list_err_i64",
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                } else {
                                    iv
                                }
                            }
                            BasicValueEnum::PointerValue(pv) => self
                                .builder
                                .build_ptr_to_int(pv, self.context.i64_type(), "res_list_err_ptr")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                            _ => self.context.i64_type().const_int(0, false),
                        };
                        let disc_is_ok = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::NE,
                                disc_i64,
                                self.context.i64_type().const_int(0, false),
                                "res_list_ok",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let function = self.current_function().ok_or("no function")?;
                        let ok_bb = self.context.append_basic_block(function, "toj_res_list_ok");
                        let err_bb =
                            self.context.append_basic_block(function, "toj_res_list_err");
                        let merge_bb =
                            self.context.append_basic_block(function, "toj_res_list_merge");
                        let i8_ptr_ty =
                            self.context.ptr_type(inkwell::AddressSpace::default());
                        let out_alloca = self.build_alloca(
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            "toj_res_list_out",
                        )?;
                        self.builder
                            .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(ok_bb);
                        // Materialize list as a pointer for runtime helpers.
                        let list_ptr = match ok_bv {
                            BasicValueEnum::StructValue(lsv) => {
                                let list_alloca = self.build_alloca(
                                    BasicTypeEnum::StructType(lsv.get_type()),
                                    "res_list_tmp",
                                )?;
                                self.build_store(list_alloca, lsv)?;
                                list_alloca
                            }
                            BasicValueEnum::PointerValue(pv) => pv,
                            BasicValueEnum::IntValue(iv) => {
                                let as_i64 = if iv.get_type().get_bit_width() < 64 {
                                    self.builder
                                        .build_int_s_extend(
                                            iv,
                                            self.context.i64_type(),
                                            "res_list_ok_i64",
                                        )
                                        .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                } else {
                                    iv
                                };
                                self.builder
                                    .build_int_to_ptr(as_i64, i8_ptr_ty, "res_list_as_ptr")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            }
                            other => {
                                return Err(CompileError::Generic(format!(
                                    "to_json Result List: unexpected Ok payload {:?}",
                                    other.get_type()
                                )));
                            }
                        };
                        // Pick list element formatter: Map / Set / i64 default.
                        let list_fn_name = if obj_type.contains("List<Map")
                            || obj_type.contains("List<Map<")
                        {
                            if obj_type.contains("Map<string, string>") {
                                "mimi_list_map_to_json_string"
                            } else {
                                "mimi_list_map_to_string"
                            }
                        } else if obj_type.contains("List<Set") {
                            "mimi_list_set_to_json"
                        } else if obj_type.contains("List<string>") {
                            "mimi_list_str_to_json"
                        } else if obj_type.contains("List<f64>") || obj_type.contains("List<f32>") {
                            "mimi_list_f64_to_json"
                        } else if obj_type.contains("List<bool>") {
                            "mimi_list_bool_to_json"
                        } else {
                            "mimi_list_i64_to_json"
                        };
                        let list_fn_ty = i8_ptr_ty
                            .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
                        let list_fn = self.module.get_function(list_fn_name).unwrap_or_else(|| {
                            self.module.add_function(
                                list_fn_name,
                                list_fn_ty,
                                Some(inkwell::module::Linkage::External),
                            )
                        });
                        let list_json = self
                            .build_call(
                                list_fn,
                                &[BasicMetadataValueEnum::PointerValue(list_ptr)],
                                "res_list_json",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("list to_json void")?
                            .into_pointer_value();
                        let buf = self.malloc_or_abort(
                            self.context.i64_type().const_int(4096, false),
                            "res_list_buf",
                        )?;
                        let fmt = self
                            .builder
                            .build_global_string_ptr("{\"Ok\":[%s]}", "res_list_fmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        let snprintf_fn = self.get_runtime_fn("snprintf")?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(4096, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(list_json),
                            ],
                            "res_list_sn",
                        )?;
                        self.build_store(out_alloca, buf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(err_bb);
                        let ebuf = self.malloc_or_abort(
                            self.context.i64_type().const_int(128, false),
                            "res_list_ebuf",
                        )?;
                        let efmt = self
                            .builder
                            .build_global_string_ptr("{\"Err\":[%ld]}", "res_list_efmt")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.build_call(
                            snprintf_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(ebuf),
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(128, false),
                                ),
                                BasicMetadataValueEnum::PointerValue(efmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(err_i64),
                            ],
                            "res_list_esn",
                        )?;
                        self.build_store(out_alloca, ebuf)?;
                        self.builder
                            .build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                        self.builder.position_at_end(merge_bb);
                        let raw = self
                            .build_load(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                out_alloca,
                                "res_list_result",
                            )?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    // Result of product tuple / named record Ok.
                    if let BasicValueEnum::StructValue(ok_sv) = ok_bv {
                        let ok_fields = ok_sv.get_type().get_field_types();
                        let ok_is_string = ok_fields.len() == 2
                            && matches!(ok_fields[0], BasicTypeEnum::PointerType(_))
                            && matches!(
                                ok_fields[1],
                                BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                            );
                        if !ok_is_string && !ok_fields.is_empty() {
                            let mut ok_inner = obj_type
                                .strip_prefix("Result<")
                                .and_then(|s| s.split(',').next())
                                .map(|s| s.trim().to_string())
                                .unwrap_or_default();
                            if ok_inner.is_empty() {
                                let pay_sty = ok_sv.get_type();
                                for (n, ty) in &self.type_llvm {
                                    if matches!(ty, BasicTypeEnum::StructType(s) if *s == pay_sty)
                                        && self.type_defs.get(n.as_str()).is_some_and(|td| {
                                            matches!(
                                                td.kind,
                                                crate::ast::TypeDefKind::Record(_)
                                            )
                                        })
                                    {
                                        ok_inner = n.clone();
                                        break;
                                    }
                                }
                            }
                            let is_named_record =
                                self.type_defs.get(&ok_inner).is_some_and(|td| {
                                    matches!(td.kind, crate::ast::TypeDefKind::Record(_))
                                });
                            if !is_named_record && ok_fields.len() < 2 {
                                // fall through
                            } else {
                            let ok_json = if is_named_record {
                                let rec_ty = ok_sv.get_type();
                                let rec_alloca = self.build_alloca(
                                    BasicTypeEnum::StructType(rec_ty),
                                    "res_rec_tmp",
                                )?;
                                self.build_store(rec_alloca, ok_sv)?;
                                self.compile_record_to_json_cstr(&ok_inner, rec_alloca)?
                            } else {
                                self.emit_product_tuple_to_json(ok_sv)?
                            };
                            let err_bv = self.build_extract_value(sv.into(), 2, "res_err_tup")?;
                            let err_i64 = match err_bv {
                                BasicValueEnum::IntValue(iv) => {
                                    if iv.get_type().get_bit_width() < 64 {
                                        self.builder
                                            .build_int_s_extend(
                                                iv,
                                                self.context.i64_type(),
                                                "res_err_tup_i64",
                                            )
                                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                                    } else {
                                        iv
                                    }
                                }
                                BasicValueEnum::PointerValue(pv) => self
                                    .builder
                                    .build_ptr_to_int(
                                        pv,
                                        self.context.i64_type(),
                                        "res_err_tup_ptr",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                                _ => self.context.i64_type().const_int(0, false),
                            };
                            // Result disc: true/1 = Ok, false/0 = Err (matches mimi_result_*_to_json).
                            let disc_is_ok = self
                                .builder
                                .build_int_compare(
                                    inkwell::IntPredicate::NE,
                                    disc_i64,
                                    self.context.i64_type().const_int(0, false),
                                    "res_tup_is_ok",
                                )
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let function = self.current_function().ok_or("no function")?;
                            let ok_bb =
                                self.context.append_basic_block(function, "toj_res_tup_ok");
                            let err_bb =
                                self.context.append_basic_block(function, "toj_res_tup_err");
                            let merge_bb =
                                self.context.append_basic_block(function, "toj_res_tup_merge");
                            let i8_ptr_ty =
                                self.context.ptr_type(inkwell::AddressSpace::default());
                            let out_alloca = self.build_alloca(
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                "toj_res_tup_out",
                            )?;
                            self.builder
                                .build_conditional_branch(disc_is_ok, ok_bb, err_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(ok_bb);
                            let buf = self.malloc_or_abort(
                                self.context.i64_type().const_int(1024, false),
                                "res_tup_ok_buf",
                            )?;
                            let ofmt = self
                                .builder
                                .build_global_string_ptr("{\"Ok\":[%s]}", "res_tup_ofmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            let snprintf_fn = self.get_runtime_fn("snprintf")?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(buf),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(1024, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(ofmt.as_pointer_value()),
                                    BasicMetadataValueEnum::PointerValue(ok_json),
                                ],
                                "res_tup_osn",
                            )?;
                            self.build_store(out_alloca, buf)?;
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(err_bb);
                            let ebuf = self.malloc_or_abort(
                                self.context.i64_type().const_int(128, false),
                                "res_tup_err_buf",
                            )?;
                            let efmt = self
                                .builder
                                .build_global_string_ptr("{\"Err\":[%ld]}", "res_tup_efmt")
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(ebuf),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(128, false),
                                    ),
                                    BasicMetadataValueEnum::PointerValue(efmt.as_pointer_value()),
                                    BasicMetadataValueEnum::IntValue(err_i64),
                                ],
                                "res_tup_esn",
                            )?;
                            self.build_store(out_alloca, ebuf)?;
                            self.builder
                                .build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                            self.builder.position_at_end(merge_bb);
                            let raw = self
                                .build_load(
                                    BasicTypeEnum::PointerType(i8_ptr_ty),
                                    out_alloca,
                                    "res_tup_result",
                                )?
                                .into_pointer_value();
                            self.register_heap_alloc(raw);
                            return self.wrap_c_string(raw);
                            } // else is_named_record || multi-field
                        }
                    }
                    let ok_i64 = match ok_bv {
                        BasicValueEnum::IntValue(iv) => {
                            if iv.get_type().get_bit_width() < 64 {
                                self.builder
                                    .build_int_s_extend(
                                        iv,
                                        self.context.i64_type(),
                                        "res_ok_i64",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            } else {
                                iv
                            }
                        }
                        BasicValueEnum::PointerValue(pv) => self
                            .builder
                            .build_ptr_to_int(pv, self.context.i64_type(), "res_ok_ptr")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                        other => {
                            return Err(CompileError::Generic(format!(
                                "to_json Result: unexpected Ok payload {:?}",
                                other.get_type()
                            )));
                        }
                    };
                    let err_bv = self.build_extract_value(sv.into(), 2, "res_err")?;
                    let err_i64 = match err_bv {
                        BasicValueEnum::IntValue(iv) => {
                            if iv.get_type().get_bit_width() < 64 {
                                self.builder
                                    .build_int_s_extend(
                                        iv,
                                        self.context.i64_type(),
                                        "res_err_i64",
                                    )
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            } else {
                                iv
                            }
                        }
                        BasicValueEnum::PointerValue(pv) => self
                            .builder
                            .build_ptr_to_int(pv, self.context.i64_type(), "res_err_ptr")
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?,
                        other => {
                            return Err(CompileError::Generic(format!(
                                "to_json Result: unexpected Err payload {:?}",
                                other.get_type()
                            )));
                        }
                    };
                    if obj_type.contains("Map<") {
                        let mode = if obj_type.contains("Map<string, string>") {
                            1i64
                        } else if obj_type.contains("Map<string, bool>") {
                            2
                        } else if obj_type.contains("Map<string, f64>")
                            || obj_type.contains("Map<string, f32>")
                        {
                            3
                        } else {
                            0
                        };
                        let func = self.get_runtime_fn("mimi_result_map_to_json")?;
                        let raw = self
                            .build_call(
                                func,
                                &[
                                    BasicMetadataValueEnum::IntValue(disc_i64),
                                    BasicMetadataValueEnum::IntValue(ok_i64),
                                    BasicMetadataValueEnum::IntValue(err_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "to_json_res_map",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_result_map_to_json void")?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    if obj_type.contains("Set<") {
                        let mode = if obj_type.contains("Set<string>") {
                            1i64
                        } else if obj_type.contains("Set<bool>") {
                            2
                        } else if obj_type.contains("Set<f64>") || obj_type.contains("Set<f32>") {
                            3
                        } else {
                            0
                        };
                        let func = self.get_runtime_fn("mimi_result_set_to_json")?;
                        let raw = self
                            .build_call(
                                func,
                                &[
                                    BasicMetadataValueEnum::IntValue(disc_i64),
                                    BasicMetadataValueEnum::IntValue(ok_i64),
                                    BasicMetadataValueEnum::IntValue(err_i64),
                                    BasicMetadataValueEnum::IntValue(
                                        self.context.i64_type().const_int(mode as u64, false),
                                    ),
                                ],
                                "to_json_res_set",
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("mimi_result_set_to_json void")?
                            .into_pointer_value();
                        self.register_heap_alloc(raw);
                        return self.wrap_c_string(raw);
                    }
                    let func = self.get_runtime_fn("mimi_result_i64_to_json")?;
                    let raw = self
                        .build_call(
                            func,
                            &[
                                BasicMetadataValueEnum::IntValue(disc_i64),
                                BasicMetadataValueEnum::IntValue(ok_i64),
                                BasicMetadataValueEnum::IntValue(err_i64),
                            ],
                            "to_json_res",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_result_i64_to_json void")?
                        .into_pointer_value();
                    self.register_heap_alloc(raw);
                    return self.wrap_c_string(raw);
                }
                // Check for Record type — serialize to JSON object via sprintf
                if self.type_defs.get(&obj_type).is_some_and(|td| {
                    matches!(td.kind, TypeDefKind::Record(_))
                }) {
                    let struct_ptr = match &metadata_args[0] {
                        BasicMetadataValueEnum::PointerValue(pv) => *pv,
                        _ => {
                            return Err(CompileError::Generic(
                                "to_json: record value must be a pointer".into(),
                            ))
                        }
                    };
                    let raw = self.compile_record_to_json_cstr(&obj_type, struct_ptr)?;
                    self.register_heap_alloc(raw);
                    return self.wrap_c_string(raw);
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
                let call_args = self.maybe_pack_enum_ctor_args(&compiled_args, function)?;
                // emit_direct_call: int width adjust + load list/record allocas
                // when the ctor takes a by-value struct payload.
                return self.emit_direct_call(function, &call_args, "enum_ctor");
            }
            return Err(format!("enum constructor '{}' not registered", ctor_name).into());
        }

        match name {
            "Ok" | "Some" | "Err" | "None" => {
                // Wrap raw string literal pointers into string structs before
                // passing to constructors (they use coerce_to_i64_slot which
                // needs the full {ptr, i64} struct, not a raw i8*).
                // Ok/Some/Err/None are not in func_defs, so maybe_wrap_string_args_for_call
                // won't find them — wrap manually based on arg expr type.
                for (i, arg_expr) in args.iter().enumerate() {
                    if i >= compiled_args.len() {
                        break;
                    }
                    if let BasicValueEnum::PointerValue(pv) = compiled_args[i] {
                        // Check if the arg is a string literal or string-producing expr
                        if matches!(arg_expr, Expr::Literal(Lit::String(_))) {
                            compiled_args[i] = self.wrap_raw_string_ptr(pv)?;
                        }
                    }
                }
                return self.compile_constructor(name, compiled_args);
            }
            _ => {}
        }

        self.maybe_wrap_string_args_for_call(name, args, &mut compiled_args)?;
        self.maybe_convert_list_args_to_values(name, &mut compiled_args)?;
        self.maybe_convert_record_args_to_values(name, &mut compiled_args)?;
        self.maybe_wrap_named_fn_args_to_closures(name, args, &mut compiled_args)?;
        // Run after value-shape conversions: borrowed parameters must be the
        // final authority and pass storage addresses, never copied structs.
        self.prepare_borrowed_user_args(name, args, vars, &mut compiled_args)?;

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

    /// Convert pointer-valued record arguments to struct values when the function
    /// parameter expects a record type (passed by value in LLVM).
    fn maybe_convert_record_args_to_values(
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
                    if tn != "List" && self.type_defs.contains_key(tn) {
                        if let BasicValueEnum::PointerValue(pv) = arg {
                            if let Some(param_llvm) = self.llvm_type_for(&fdef.params[i].ty) {
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
                let mangled = Self::mangle_name(name, &callee_map);
                (mangled, callee_map)
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
    pub(in crate::codegen) fn emit_direct_call(
        &self,
        function: inkwell::values::FunctionValue<'ctx>,
        compiled_args: &[BasicValueEnum<'ctx>],
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Coerce each arg to the declared param type: int width, string wrap
        // (raw i8* → {ptr,len}), list/record alloca load → by-value struct.
        let adjusted_args: Vec<BasicValueEnum<'ctx>> = compiled_args
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if let Some(param) = function.get_nth_param(i as u32) {
                    self.coerce_value_to_expected_type(*v, param.get_type())
                } else {
                    Ok(*v)
                }
            })
            .collect::<Result<_, CompileError>>()?;
        let metadata_args: Vec<_> = adjusted_args
            .iter()
            .map(|v| types::basic_value_to_metadata_value(v, self.context.i64_type()))
            .collect();
        let call = self.build_call(function, &metadata_args, name)?;
        Ok(call_try_basic_value(&call)
            .unwrap_or(self.context.i64_type().const_int(0, false).into()))
    }

    /// Codegen for `read_lines_each(path, callback)`.
    ///
    /// Runtime `mimi_read_lines_each` expects `void (*)(const char*)` — not a
    /// Mimi closure. Build a thin C thunk that:
    /// 1. Loads TLS-stored Mimi closure `{fn_ptr, env_ptr}`
    /// 2. Wraps the C line pointer into `{ptr, len}` via strlen
    /// 3. Calls `fn_ptr(env, string_struct)` (Mimi lambda ABI)
    pub(in crate::codegen) fn compile_read_lines_each_call(
        &mut self,
        compiled_args: &[BasicValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if compiled_args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "read_lines_each expects 2 arguments (path, callback)".into(),
            ));
        }
        let path_ptr = match compiled_args[0] {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::StructValue(sv) => {
                // Mimi string {ptr, len} — extract data pointer.
                self.builder
                    .build_extract_value(sv, 0, "rle_path_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract path: {}", e)))?
                    .into_pointer_value()
            }
            _ => {
                return Err(CompileError::Generic(
                    "read_lines_each: path must be string".into(),
                ))
            }
        };

        let closure_sv = match compiled_args[1] {
            BasicValueEnum::StructValue(sv) => sv,
            _ => {
                return Err(CompileError::Generic(
                    "read_lines_each: callback must be a closure".into(),
                ))
            }
        };
        let fn_ptr = self
            .builder
            .build_extract_value(closure_sv, 0, "rle_fn_ptr")
            .map_err(|e| CompileError::LlvmError(format!("extract fn: {}", e)))?
            .into_pointer_value();
        let env_ptr = self
            .builder
            .build_extract_value(closure_sv, 1, "rle_env_ptr")
            .map_err(|e| CompileError::LlvmError(format!("extract env: {}", e)))?
            .into_pointer_value();

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        // TLS globals for this call site (reused pattern from callback thunks).
        let id = self.callback_thunk_counter;
        self.callback_thunk_counter += 1;
        let fn_global = self.module.add_global(
            i8_ptr,
            None,
            &format!("__mimi_rle_fnptr_{}", id),
        );
        fn_global.set_initializer(&i8_ptr.const_null());
        fn_global.set_thread_local(true);
        fn_global.set_thread_local_mode(Some(inkwell::ThreadLocalMode::GeneralDynamicTLSModel));
        let env_global = self.module.add_global(
            i8_ptr,
            None,
            &format!("__mimi_rle_envptr_{}", id),
        );
        env_global.set_initializer(&i8_ptr.const_null());
        env_global.set_thread_local(true);
        env_global.set_thread_local_mode(Some(inkwell::ThreadLocalMode::GeneralDynamicTLSModel));

        self.build_store(fn_global.as_pointer_value(), fn_ptr)?;
        self.build_store(env_global.as_pointer_value(), env_ptr)?;
        self.pending_callback_tls
            .push(fn_global.as_pointer_value());
        self.pending_callback_tls
            .push(env_global.as_pointer_value());

        // Build void(i8*) thunk if not already present for this id.
        let thunk_name = format!("__mimi_rle_thunk_{}", id);
        let void_ty = self.context.void_type();
        let thunk_fn_ty = void_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
        let thunk_fn = self.module.add_function(
            &thunk_name,
            thunk_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        let saved_block = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        let line_c = thunk_fn
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("rle thunk missing line param".into()))?
            .into_pointer_value();
        let tls_fn = self
            .build_load(i8_ptr, fn_global.as_pointer_value(), "rle_tls_fn")?
            .into_pointer_value();
        let tls_env = self
            .build_load(i8_ptr, env_global.as_pointer_value(), "rle_tls_env")?
            .into_pointer_value();

        // Wrap C string as Mimi {ptr, len} without alloca (SSA only).
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(line_c)],
                "rle_strlen",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen void".into()))?
            .into_int_value();
        let str_with_ptr = self
            .builder
            .build_insert_value(string_ty.get_undef(), line_c, 0, "rle_str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("rle str ptr: {}", e)))?
            .into_struct_value();
        let str_val = self
            .builder
            .build_insert_value(str_with_ptr, len, 1, "rle_str_len")
            .map_err(|e| CompileError::LlvmError(format!("rle str len: {}", e)))?
            .into_struct_value();

        // Mimi lambda ABI: fn(env_ptr, string) -> i64 (ignore return).
        let mimi_fn_ty = i64_ty.fn_type(
            &[
                BasicMetadataTypeEnum::PointerType(i8_ptr),
                types::basic_to_metadata(self.context, BasicTypeEnum::StructType(string_ty)),
            ],
            false,
        );
        let fn_typed = self.build_pointer_cast(
            tls_fn,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "rle_fn_typed",
        )?;
        let _ = self
            .builder
            .build_indirect_call(
                mimi_fn_ty,
                fn_typed,
                &[
                    BasicMetadataValueEnum::PointerValue(tls_env),
                    BasicMetadataValueEnum::StructValue(str_val),
                ],
                "rle_cb_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("rle cb call: {}", e)))?;
        self.build_return(None)?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let runtime_fn = self.get_runtime_fn("mimi_read_lines_each")?;
        let thunk_ptr = thunk_fn.as_global_value().as_pointer_value();
        let call = self.build_call(
            runtime_fn,
            &[
                BasicMetadataValueEnum::PointerValue(path_ptr),
                BasicMetadataValueEnum::PointerValue(thunk_ptr),
            ],
            "read_lines_each",
        )?;
        // Clear TLS after the call (same as other callback builtins).
        let tls_ptrs: Vec<_> = self.pending_callback_tls.drain(..).collect();
        for p in tls_ptrs {
            self.build_store(p, i8_ptr.const_null())?;
        }
        Ok(call_try_basic_value(&call)
            .unwrap_or(i64_ty.const_int(0, false).into()))
    }

    pub(in crate::codegen) fn compile_call_mangled(
        &mut self,
        mangled: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let compiled_args = self.compile_arg_values(args, vars)?;
        // Adjust integer arg widths to match declared parameter types.
        let function = self.module.get_function(mangled);
        let adjusted_args: Vec<BasicValueEnum<'ctx>> = if let Some(f) = function {
            compiled_args
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    if let Some(param) = f.get_nth_param(i as u32) {
                        if let (
                            BasicValueEnum::IntValue(arg_iv),
                            BasicValueEnum::IntValue(param_iv),
                        ) = (*v, param)
                        {
                            let arg_bw = arg_iv.get_type().get_bit_width();
                            let param_bw = param_iv.get_type().get_bit_width();
                            if arg_bw == param_bw {
                                Ok(*v)
                            } else if arg_bw > param_bw {
                                Ok(self
                                    .builder
                                    .build_int_truncate(
                                        arg_iv,
                                        param_iv.get_type(),
                                        &format!("call_arg_trunc_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("arg trunc: {}", e))
                                    })?
                                    .into())
                            } else {
                                Ok(self
                                    .builder
                                    .build_int_s_extend(
                                        arg_iv,
                                        param_iv.get_type(),
                                        &format!("call_arg_sext_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("arg s_ext: {}", e))
                                    })?
                                    .into())
                            }
                        } else {
                            Ok(*v)
                        }
                    } else {
                        Ok(*v)
                    }
                })
                .collect::<Result<_, CompileError>>()?
        } else {
            compiled_args
        };
        let metadata_args: Vec<_> = adjusted_args
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
    /// Named args (`name = expr`) are reordered to the function's parameter
    /// order when `func_name` is known (and present in `func_defs`).
    fn compile_arg_values(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CompileError> {
        args.iter()
            .map(|arg| match arg {
                Expr::NamedArg(_, value) => self.compile_expr(value, vars),
                other => self.compile_expr(other, vars),
            })
            .collect()
    }

    /// Reorder named args to positional order for a known function definition.
    fn reorder_named_args(&self, name: &str, args: &[Expr]) -> Result<Vec<Expr>, CompileError> {
        let has_named = args.iter().any(|a| matches!(a, Expr::NamedArg(_, _)));
        if !has_named {
            return Ok(args.to_vec());
        }
        let Some(fdef) = self.func_defs.get(name) else {
            // Unknown function (builtin/method): strip NamedArg wrappers only.
            return Ok(args
                .iter()
                .map(|a| match a {
                    Expr::NamedArg(_, v) => *v.clone(),
                    other => other.clone(),
                })
                .collect());
        };
        let mut ordered: Vec<Option<Expr>> = vec![None; fdef.params.len()];
        let mut next_pos = 0usize;
        for arg in args {
            match arg {
                Expr::NamedArg(n, val) => {
                    let Some(pos) = fdef.params.iter().position(|p| p.name == *n) else {
                        return Err(CompileError::Generic(format!(
                            "unknown named argument '{}' for function '{}'",
                            n, name
                        )));
                    };
                    if pos >= ordered.len() {
                        ordered.resize(pos + 1, None);
                    }
                    ordered[pos] = Some(*val.clone());
                }
                other => {
                    while next_pos < ordered.len() && ordered[next_pos].is_some() {
                        next_pos += 1;
                    }
                    if next_pos >= ordered.len() {
                        ordered.push(Some(other.clone()));
                    } else {
                        ordered[next_pos] = Some(other.clone());
                    }
                    next_pos += 1;
                }
            }
        }
        // Fill defaults for missing slots.
        for (i, p) in fdef.params.iter().enumerate() {
            if i < ordered.len() && ordered[i].is_none() {
                if let Some(ref d) = p.default_value {
                    ordered[i] = Some(d.clone());
                }
            }
        }
        ordered
            .into_iter()
            .enumerate()
            .map(|(i, o)| {
                o.ok_or_else(|| {
                    CompileError::Generic(format!(
                        "missing argument {} for function '{}'",
                        i + 1,
                        name
                    ))
                })
            })
            .collect()
    }

    /// Lower `view`/`mutate` user-function arguments to the reference ABI.
    /// Scalar/struct locals pass their alloca; List literals already evaluate
    /// to a pointer to the authoritative List header and pass that pointer.
    fn prepare_borrowed_user_args(
        &mut self,
        name: &str,
        arg_exprs: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name).cloned() else {
            return Ok(());
        };
        for (index, param) in fdef.params.iter().enumerate() {
            if param.borrow.is_none() || index >= args.len() || index >= arg_exprs.len() {
                continue;
            }
            match &arg_exprs[index] {
                Expr::Ident(var_name) => {
                    let Some(&(slot, stored_ty)) = vars.get(var_name) else {
                        return Err(CompileError::Generic(format!(
                            "borrowed argument '{}' must refer to a local variable",
                            var_name
                        )));
                    };
                    let list_is_already_indirect = matches!(
                        (&param.ty, stored_ty),
                        (Type::Name(type_name, _), BasicTypeEnum::PointerType(_))
                            if type_name == "List"
                    );
                    if list_is_already_indirect {
                        args[index] =
                            self.build_load(stored_ty, slot, &format!("{}_borrow_ptr", var_name))?;
                    } else {
                        args[index] = BasicValueEnum::PointerValue(slot);
                    }
                }
                Expr::Field(object, field_name) => {
                    let field_slot = self.compile_field_gep(object, field_name, vars)?;
                    args[index] = BasicValueEnum::PointerValue(field_slot);
                }
                _ => {
                    let target_ty = self.llvm_type_for(&param.ty).ok_or_else(|| {
                        CompileError::Generic(format!(
                            "no LLVM type for borrowed argument {}",
                            index + 1
                        ))
                    })?;
                    // Rvalues have no caller storage. Materialize a temporary
                    // so `view 5` and `mutate 7` remain ergonomic; mutations to
                    // a temporary are intentionally observable only through the
                    // function's return value.
                    if matches!(args[index], BasicValueEnum::PointerValue(_))
                        && matches!(target_ty, BasicTypeEnum::StructType(_))
                    {
                        // Aggregate literals such as List already evaluate to
                        // a pointer to their authoritative temporary storage.
                    } else {
                        let slot = self.build_alloca(target_ty, "borrowed_temp")?;
                        let value = self.adjust_int_val(args[index], target_ty)?;
                        self.build_store(slot, value)?;
                        args[index] = BasicValueEnum::PointerValue(slot);
                    }
                }
            }
        }
        Ok(())
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
            if param.borrow.is_some() {
                continue;
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
                return Err(CompileError::Generic(format!(
                    "type '{}' is not a struct; cannot create element function for list operation",
                    type_name
                )));
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
                            // A1: use s_extend for signed integers.
                            let bw = iv.get_type().get_bit_width();
                            let ext = if bw == 1 {
                                self.builder
                                    .build_int_z_extend(iv, i64_ty, &format!("{}_ext", field.name))
                                    .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))?
                            } else {
                                self.builder
                                    .build_int_s_extend(iv, i64_ty, &format!("{}_ext", field.name))
                                    .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))?
                            };
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

            // Allocate buffer and sprintf (CG-H1: size from format + field slack).
            let est = (fmt.len() + fields.len() * 256 + 1024).max(4096) as u64;
            let buf_size = i64_ty.const_int(est, false);
            // B4: OOM-safe buffer for element to_json.
            let buf = self.malloc_or_abort(buf_size, "elem_json_malloc")?;
            let fmt_ptr = self
                .builder
                .build_global_string_ptr(&fmt, "elem_json_fmt")
                .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
            // B3/CG-C3: snprintf returns i32, not i8*.
            let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let i32_ty = self.context.i32_type();
                let ty = i32_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    true,
                );
                self.module
                    .add_function("snprintf", ty, Some(inkwell::module::Linkage::External))
            });
            let mut all_args = vec![BasicMetadataValueEnum::PointerValue(buf)];
            all_args.push(BasicMetadataValueEnum::IntValue(buf_size));
            all_args.push(BasicMetadataValueEnum::PointerValue(
                fmt_ptr.as_pointer_value(),
            ));
            all_args.extend(sprintf_args);
            self.build_call(snprintf_fn, &all_args, "elem_json_snprintf")?;
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

    /// Serialize a named Record at `struct_ptr` to a heap JSON C string.
    /// Caller owns the buffer (export) or should register_heap_alloc (to_json).
    pub(in crate::codegen) fn compile_record_to_json_cstr(
        &mut self,
        obj_type: &str,
        struct_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let type_def = self.type_defs.get(obj_type).ok_or_else(|| {
            CompileError::LlvmError(format!("no type def for record {}", obj_type))
        })?;
        let fields = match &type_def.kind {
            TypeDefKind::Record(fields) => fields.clone(),
            _ => {
                return Err(CompileError::LlvmError(format!(
                    "{} is not a record",
                    obj_type
                )))
            }
        };
        let llvm_ty = *self.type_llvm.get(obj_type).ok_or_else(|| {
            CompileError::LlvmError(format!("no LLVM type for record {}", obj_type))
        })?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(CompileError::LlvmError(format!(
                "record type {} is not a struct",
                obj_type
            )));
        };
        let i64_ty = self.context.i64_type();
        let mut idx_map: Vec<(usize, Field)> = fields.iter().cloned().enumerate().collect();
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
            let ft = sty
                .get_field_type_at_index(*i as u32)
                .ok_or_else(|| CompileError::LlvmError("missing field type".into()))?;
            let field_val = self.build_load(ft, gep, &format!("load_{}", field.name))?;
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
                    fmt.push_str(&format!("\"{}\":%ld", field.name));
                    let field_iv = field_val.into_int_value();
                    let field_i64 = if field_iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(
                                field_iv,
                                self.context.i64_type(),
                                "json_i32_ext",
                            )
                            .map_err(|e| CompileError::LlvmError(e.to_string()))?
                    } else {
                        field_iv
                    };
                    sprintf_args.push(BasicMetadataValueEnum::IntValue(field_i64));
                }
                Type::Name(n, _) if n == "bool" => {
                    fmt.push_str(&format!("\"{}\":%s", field.name));
                    let iv = field_val.into_int_value();
                    let true_global = self
                        .builder
                        .build_global_string_ptr("true", &format!("{}_true", field.name))
                        .map_err(|e| CompileError::LlvmError(e.to_string()))?;
                    let false_global = self
                        .builder
                        .build_global_string_ptr("false", &format!("{}_false", field.name))
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
                    let selected = self
                        .builder
                        .build_select(
                            is_true,
                            true_global.as_pointer_value(),
                            false_global.as_pointer_value(),
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
        let est = (fmt.len() + fields.len() * 256 + 1024).max(4096) as u64;
        let buf_size = i64_ty.const_int(est, false);
        let buf = self.malloc_or_abort(buf_size, "record_json_malloc")?;
        let fmt_ptr = self
            .builder
            .build_global_string_ptr(&fmt, "record_json_fmt")
            .map_err(|e| CompileError::LlvmError(e.to_string()))?;
        let mut all_args = vec![BasicMetadataValueEnum::PointerValue(buf)];
        all_args.push(BasicMetadataValueEnum::IntValue(buf_size));
        all_args.push(BasicMetadataValueEnum::PointerValue(
            fmt_ptr.as_pointer_value(),
        ));
        all_args.extend(sprintf_args);
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        self.build_call(snprintf_fn, &all_args, "record_json_snprintf")?;
        Ok(buf)
    }
}

/// Convert a BasicValueEnum to its metadata type for indirect calls.
fn basic_value_to_metadata_type<'ctx>(
    val: &BasicValueEnum<'ctx>,
) -> Result<BasicMetadataTypeEnum<'ctx>, CompileError> {
    Ok(match val {
        BasicValueEnum::IntValue(iv) => BasicMetadataTypeEnum::IntType(iv.get_type()),
        BasicValueEnum::FloatValue(fv) => BasicMetadataTypeEnum::FloatType(fv.get_type()),
        BasicValueEnum::PointerValue(pv) => BasicMetadataTypeEnum::PointerType(pv.get_type()),
        BasicValueEnum::StructValue(sv) => BasicMetadataTypeEnum::StructType(sv.get_type()),
        BasicValueEnum::ArrayValue(av) => BasicMetadataTypeEnum::ArrayType(av.get_type()),
        BasicValueEnum::VectorValue(vv) => BasicMetadataTypeEnum::VectorType(vv.get_type()),
        BasicValueEnum::ScalableVectorValue(_) => {
            return Err(CompileError::Generic(
                "scalable vector not supported in Mimi codegen".to_string(),
            ));
        }
    })
}
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
