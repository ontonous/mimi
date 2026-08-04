use crate::ast::*;
use crate::codegen::{types, CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_record_expr(
        &mut self,
        ty: &Option<String>,
        fields: &[RecordFieldExpr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let type_name = ty.as_deref().unwrap_or("unknown");
        let llvm_ty = *self
            .type_llvm
            .get(type_name)
            .ok_or_else(|| format!("unknown type '{}'", type_name))?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(format!("type '{}' is not a struct", type_name).into());
        };

        let alloca = self.build_alloca(sty, type_name)?;
        // N-2 (0.34.35): declared field AST types, needed to reconstruct the
        // trampoline signature when a runtime fn pointer enters a func field.
        let declared_fields: Option<Vec<(String, Type)>> =
            self.type_defs.get(type_name).and_then(|td| match &td.kind {
                TypeDefKind::Record(fields) => Some(
                    fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                ),
                _ => None,
            });
        for (i, field) in fields.iter().enumerate() {
            let val = self.compile_expr(&field.value, vars)?;
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let field_ty = sty
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("field {} type", i)))?;
            let store_val = self.maybe_load_compound_field_value(val, field_ty, field, vars)?;
            // CG-C4: truncate/extend the stored value to match the field type.
            // #[repr(C)] records use extern field types (i32 for i32 fields), so
            // an i64 value from the expression must be truncated to i32 before storing.
            let store_val = self.adjust_int_val(store_val, field_ty)?;
            // N-2 (0.34.35): a plain function reference stored into a
            // closure-typed field must be wrapped as {wrapper, null-env} —
            // see wrap_fn_ref_as_closure_if_needed.
            let declared_ty = declared_fields
                .as_ref()
                .and_then(|fs| fs.iter().find(|(n, _)| n == &field.name).map(|(_, t)| t));
            let store_val =
                self.wrap_fn_ref_as_closure_if_needed(field, store_val, field_ty, declared_ty)?;
            self.build_store(gep, store_val)?;
        }
        Ok(alloca.into())
    }

    /// N-2 (0.34.35, L1): a plain function reference stored into a
    /// closure-typed record field (`func(...) -> ...`) must be wrapped as a
    /// proper `{fn_ptr, env_ptr}` closure. Without this, the raw 8-byte fn
    /// pointer is stored into the 16-byte slot leaving env uninitialized; the
    /// call site then invokes `fn_ptr(env, args...)`, injecting the garbage
    /// env as the first argument — silent miscompilation (VM executes this
    /// correctly). Two shapes:
    /// - statically known callee → `{static_wrapper(callee), null}` (the
    ///   wrapper drops env and forwards to the callee);
    /// - runtime fn pointer (e.g. held in a variable) →
    ///   `{sig_trampoline, callee_ptr}` (the trampoline indirect-calls the
    ///   callee carried in the env slot with the declared field signature).
    /// Closure values that are already structs (lambdas, captured or not)
    /// pass through untouched.
    fn wrap_fn_ref_as_closure_if_needed(
        &mut self,
        field: &RecordFieldExpr,
        val: BasicValueEnum<'ctx>,
        field_ty: BasicTypeEnum<'ctx>,
        declared_ty: Option<&Type>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let BasicTypeEnum::StructType(sty) = field_ty else {
            return Ok(val);
        };
        if sty != types::closure_struct_type(self.context) {
            return Ok(val);
        }
        let BasicValueEnum::PointerValue(pv) = val else {
            return Ok(val);
        };
        // Shape 1: statically known callee. The literal names the function,
        // or the pointer IS the function global itself.
        let static_name = match field.value.unlocated() {
            Expr::Ident(name) => {
                if self.module.get_function(name).is_some() {
                    Some(name.clone())
                } else {
                    None
                }
            }
            _ => {
                let n = pv.get_name().to_string_lossy().into_owned();
                if !n.is_empty() && self.module.get_function(&n).is_some() {
                    Some(n)
                } else {
                    None
                }
            }
        };
        let (fn_slot, env_slot) = if let Some(fn_name) = static_name {
            // Same mechanism as func-typed call arguments
            // (maybe_wrap_named_fn_args_to_closures).
            let wrapper = self.get_or_create_closure_wrapper(&fn_name)?;
            let null_ptr = self
                .context
                .ptr_type(inkwell::AddressSpace::default())
                .const_null();
            (wrapper, null_ptr)
        } else {
            // Shape 2: runtime fn pointer. The callee rides in the env slot;
            // the trampoline is keyed by the declared field signature.
            let Some(Type::Func(params, ret)) = declared_ty.map(|t| t.unlocated()) else {
                return Ok(val);
            };
            let trampoline = self.get_or_create_fnptr_trampoline(params, ret)?;
            (trampoline, pv)
        };
        let closure_ty = types::closure_struct_type(self.context);
        let closure_alloca =
            self.build_alloca(BasicTypeEnum::StructType(closure_ty), "fnref_closure")?;
        let fn_gep = self
            .gep()
            .build_struct_gep(closure_ty, closure_alloca, 0, "fn_gep")
            .map_err(|e| CompileError::LlvmError(format!("fn gep: {}", e)))?;
        self.build_store(fn_gep, BasicValueEnum::PointerValue(fn_slot))?;
        let env_gep = self
            .gep()
            .build_struct_gep(closure_ty, closure_alloca, 1, "env_gep")
            .map_err(|e| CompileError::LlvmError(format!("env gep: {}", e)))?;
        self.build_store(env_gep, BasicValueEnum::PointerValue(env_slot))?;
        self.build_load(
            BasicTypeEnum::StructType(closure_ty),
            closure_alloca,
            "fnref_closure_val",
        )
    }

    /// N-2 (0.34.35): signature-keyed trampoline for RUNTIME function
    /// pointers stored into closure-typed slots. Layout at the call site:
    /// `closure.fn_ptr(env=actual_callee, args...)`. The trampoline
    /// indirect-calls the callee held in its first (env) parameter with the
    /// declared field signature, so no env is injected into the callee.
    fn get_or_create_fnptr_trampoline(
        &mut self,
        params: &[Type],
        ret: &Type,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        use inkwell::types::BasicMetadataTypeEnum;

        let fingerprint = format!(
            "{}->{}",
            params
                .iter()
                .map(crate::core::fmt_type)
                .collect::<Vec<_>>()
                .join("_"),
            crate::core::fmt_type(ret)
        );
        if let Some(cached) = self.fnptr_trampolines.get(&fingerprint) {
            return Ok(*cached);
        }

        let mut param_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(params.len());
        for p in params {
            let pt = self.llvm_type_for(p).ok_or_else(|| {
                CompileError::Generic(format!(
                    "fnptr trampoline: unsupported param type '{}'",
                    crate::core::fmt_type(p)
                ))
            })?;
            param_tys.push(pt);
        }
        let ret_ty = self.llvm_type_for(ret).ok_or_else(|| {
            CompileError::Generic(format!(
                "fnptr trampoline: unsupported return type '{}'",
                crate::core::fmt_type(ret)
            ))
        })?;

        // Callee signature (no env): what the plain function expects.
        let callee_meta: Vec<BasicMetadataTypeEnum<'ctx>> =
            param_tys.iter().map(|t| (*t).into()).collect();
        let callee_fn_ty = match ret_ty {
            BasicTypeEnum::IntType(t) => t.fn_type(&callee_meta, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&callee_meta, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&callee_meta, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&callee_meta, false),
            _ => {
                return Err(CompileError::Generic(
                    "fnptr trampoline: unsupported return type class".to_string(),
                ))
            }
        };

        // Trampoline signature: (env=callee_ptr, params...).
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut tramp_meta: Vec<BasicMetadataTypeEnum<'ctx>> =
            vec![BasicMetadataTypeEnum::PointerType(i8_ptr)];
        tramp_meta.extend(param_tys.iter().map(|t| BasicMetadataTypeEnum::from(*t)));
        let tramp_fn_ty = match ret_ty {
            BasicTypeEnum::IntType(t) => t.fn_type(&tramp_meta, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&tramp_meta, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&tramp_meta, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&tramp_meta, false),
            _ => unreachable!(),
        };

        let safe_fp = fingerprint.replace([' ', '(', ')', ',', '[', ']', '<', '>'], "_");
        let tramp_name = format!("__mimi_fnptr_tramp_{}", safe_fp);
        let tramp_fn = self.module.add_function(
            &tramp_name,
            tramp_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );

        let saved_block = self.builder.get_insert_block();
        let entry_bb = self.context.append_basic_block(tramp_fn, "entry");
        self.builder.position_at_end(entry_bb);

        let callee_ptr = tramp_fn
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("fnptr trampoline: env param".into()))?
            .into_pointer_value();
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for i in 0..param_tys.len() {
            let p = tramp_fn.get_nth_param((i + 1) as u32).ok_or_else(|| {
                CompileError::LlvmError(format!("fnptr trampoline: param {}", i + 1))
            })?;
            call_args.push(types::basic_value_to_metadata_value(
                &p,
                self.context.i64_type(),
            ));
        }
        let call = self
            .builder
            .build_indirect_call(callee_fn_ty, callee_ptr, &call_args, "fnptr_call")
            .map_err(|e| CompileError::LlvmError(format!("fnptr indirect call: {}", e)))?;
        let ret_val = crate::codegen::call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("fnptr trampoline: void call".to_string()))?;
        self.build_return(Some(&ret_val))?;

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let ptr = tramp_fn.as_global_value().as_pointer_value();
        self.fnptr_trampolines.insert(fingerprint, ptr);
        Ok(ptr)
    }

    /// When a PointerValue is stored into a struct-typed field, check if the
    /// expression produces a compound value that needs loading.
    fn maybe_load_compound_field_value(
        &self,
        val: BasicValueEnum<'ctx>,
        field_ty: BasicTypeEnum<'ctx>,
        field: &RecordFieldExpr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(_)) = (&val, field_ty)
        else {
            return Ok(val);
        };
        let needs_load = matches!(
            &field.value,
            Expr::List(_)
                | Expr::Tuple(_)
                | Expr::Comprehension { .. }
                | Expr::SetLiteral(_)
                | Expr::Block(_)
        ) || {
            let val_type = self.infer_object_type(&field.value, vars);
            val_type.starts_with("List")
                || val_type.starts_with("Set")
                || val_type.starts_with("Option")
                || val_type.starts_with("Result")
                || self.type_defs.contains_key(&val_type)
        };
        if needs_load {
            self.build_load(field_ty, *pv, &format!("load_{}", field.name))
        } else {
            Ok(val)
        }
    }

    pub(in crate::codegen) fn compile_list_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let count = elems.len() as u64;
        let len_val = self.context.i64_type().const_int(count, false);
        let (data_ptr, data_ptr_i64) = self.allocate_list_data(count)?;
        self.store_list_elements(data_ptr_i64, elems, vars)?;
        self.build_list_struct(len_val, data_ptr)
    }

    fn allocate_list_data(
        &self,
        count: u64,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let len_val = self.context.i64_type().const_int(count, false);
        let sizeof_i64 = self.context.i64_type().const_int(8, false);
        let alloc_size = self
            .builder
            .build_int_mul(len_val, sizeof_i64, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let data_ptr = self.malloc_or_abort(alloc_size, "malloc_call")?;
        let data_ptr_i64 = self
            .build_bit_cast(
                data_ptr.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "data_ptr_i64",
            )?
            .into_pointer_value();
        Ok((data_ptr, data_ptr_i64))
    }

    fn store_list_elements(
        &mut self,
        data_ptr_i64: inkwell::values::PointerValue<'ctx>,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        for (i, elem) in elems.iter().enumerate() {
            let val = self.compile_expr(elem, vars)?;
            let iv = self.coerce_to_list_storage(val, elem, vars)?;
            let idx = self.context.i64_type().const_int(i as u64, false);
            let elem_ptr =
                self.build_in_bounds_gep(self.context.i64_type(), data_ptr_i64, &[idx], "elem")?;
            self.build_store(elem_ptr, iv)?;
        }
        Ok(())
    }

    fn coerce_to_list_storage(
        &mut self,
        val: BasicValueEnum<'ctx>,
        _elem_expr: &Expr,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        // When packing into a typed List<Result/Option/...>, inflate so Ok and
        // Err share one layout (Err is often {i1,i64,i64} while Ok is wider).
        // Critical: never memcpy a narrow Result into a wide buffer without
        // inflate — field offsets diverge ({i1,i64,i64} vs {i1,{i64,i64},i64}).
        let val = if let Some(elem_ty) = self.pending_list_elem_type.clone() {
            let needs_inflate = match elem_ty.unlocated() {
                Type::Result(_, _) | Type::Option(_) => true,
                Type::Name(n, _) if n == "Result" || n == "Option" => true,
                _ => false,
            };
            if needs_inflate {
                self.inflate_variant_struct(val, &elem_ty)?
            } else {
                val
            }
        } else {
            val
        };
        match val {
            BasicValueEnum::IntValue(iv) => {
                // List slots are always i64 — extend i32 (or narrower) values before storing.
                let i64_ty = self.context.i64_type();
                if iv.get_type().get_bit_width() < 64 {
                    Ok(self
                        .builder
                        .build_int_s_extend(iv, i64_ty, "list_elem_sext")
                        .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?)
                } else {
                    Ok(iv)
                }
            }
            BasicValueEnum::FloatValue(fv) => Ok(self
                .build_bit_cast(fv.into(), self.context.i64_type().into(), "f64_to_i64")?
                .into_int_value()),
            BasicValueEnum::PointerValue(pv) => {
                self.build_ptr_to_int(pv, self.context.i64_type(), "ptr_to_i64")
            }
            BasicValueEnum::StructValue(sv) => {
                // Mimi string struct {ptr, i64}: extract the raw C string
                // pointer and store it directly (no malloc).
                let sv_fields = sv.get_type().get_field_types();
                if sv_fields.len() == 2
                    && matches!(&sv_fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(&sv_fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
                {
                    let raw_ptr = self
                        .build_extract_value(sv.into(), 0, "str_ptr")?
                        .into_pointer_value();
                    return self.build_ptr_to_int(raw_ptr, self.context.i64_type(), "str_to_i64");
                }
                // Always pack using the (possibly inflated) struct's own type —
                // inflate already rewrote Err to the full Result layout.
                let struct_ty = sv.get_type();
                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(struct_ty));
                let size_val = self.context.i64_type().const_int(size, false);
                // B4: OOM-safe heap copy when packing structs into i64 slots.
                let ptr = self.malloc_or_abort(size_val, "struct_to_i64")?;
                let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let typed_ptr = self
                    .build_bit_cast(
                        ptr.into(),
                        BasicTypeEnum::PointerType(i8_ptr_ty),
                        "struct_ptr",
                    )?
                    .into_pointer_value();
                self.build_store(typed_ptr, sv)?;
                self.build_ptr_to_int(typed_ptr, self.context.i64_type(), "ptr_to_i64")
            }
            _ => Err("list elements must be scalar or struct types for now".into()),
        }
    }

    pub(in crate::codegen) fn build_list_struct(
        &self,
        len_val: inkwell::values::IntValue<'ctx>,
        data_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let list_ty = self.list_struct_type();
        let list_alloca = self.build_entry_alloca(list_ty, "list")?;
        let len_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 0, "list_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(len_gep, len_val)?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "list_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_void_ptr = self.build_bit_cast(
            data_ptr.into(),
            self.context
                .ptr_type(inkwell::AddressSpace::default())
                .into(),
            "data_void",
        )?;
        self.build_store(data_gep, data_void_ptr)?;
        self.register_heap_slot(list_alloca, list_ty, 1);
        Ok(list_alloca.into())
    }

    pub(in crate::codegen) fn compile_tuple_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let string_struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let mut field_vals = Vec::new();
        let mut field_tys = Vec::new();
        for e in elems.iter() {
            let val = self.compile_expr(e, vars)?;
            if let BasicValueEnum::PointerValue(pv) = val {
                if self.expr_is_string(e) {
                    let loaded = self.wrap_tuple_string_field(pv, string_struct_ty)?;
                    field_vals.push(loaded);
                    field_tys.push(BasicTypeEnum::StructType(string_struct_ty));
                    continue;
                }
                // List<T> variables are stored as pointers to the list struct
                // ({len, data}); a tuple of lists must hold the struct value.
                let tname = self.infer_object_type(e, vars);
                if self.is_list_type_name(&tname) {
                    let list_ty = self.list_struct_basic_type();
                    let loaded = self.build_load(list_ty, pv, "tuple_list")?;
                    field_vals.push(loaded);
                    field_tys.push(loaded.get_type());
                    continue;
                }
            }
            field_tys.push(val.get_type());
            field_vals.push(val);
        }
        let struct_ty = self.context.struct_type(&field_tys, false);
        self.tuple_type_stack.push(struct_ty);
        let alloca = self.build_alloca(struct_ty, "tuple")?;
        for (i, val) in field_vals.iter().enumerate() {
            let gep = self
                .gep()
                .build_struct_gep(struct_ty, alloca, i as u32, &format!("tuple_{}", i))
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(gep, *val)?;
        }
        // Return by value so println/match see a struct (not a pointer mistaken for C string).
        self.build_load(struct_ty, alloca, "tuple_val")
    }

    fn wrap_tuple_string_field(
        &self,
        pv: inkwell::values::PointerValue<'ctx>,
        string_struct_ty: inkwell::types::StructType<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let str_alloca = self.build_alloca(string_struct_ty, "tuple_str")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_struct_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ptr_gep, pv)?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_struct_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let s_len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(pv)],
                "strlen_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?
            .into_int_value();
        self.build_store(len_gep, s_len)?;
        self.build_load(string_struct_ty, str_alloca, "tuple_str_val")
    }

    pub(in crate::codegen) fn compile_comprehension_expr(
        &mut self,
        expr: &Expr,
        var: &str,
        iter: &Expr,
        guard: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (list_ptr, list_len, data_ptr) = self.load_comprehension_input(iter, vars)?;
        let (out_i64, out_ptr) = self.allocate_comprehension_output(list_len)?;
        let (_idx_alloca, wi_alloca) = self.emit_comprehension_loop(
            expr, var, guard, list_ptr, list_len, data_ptr, out_i64, vars,
        )?;
        let result_len = self.build_load(
            BasicTypeEnum::IntType(self.context.i64_type()),
            wi_alloca,
            "result_len",
        )?;
        self.build_comprehension_result(result_len.into_int_value(), out_ptr)
    }

    fn load_comprehension_input(
        &mut self,
        iter: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let iter_val = self.compile_expr(iter, vars)?;
        let list_ptr = match iter_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("comprehension iter must be a list pointer".into()),
        };
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let list_struct_ty = self.list_struct_type();
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "comp_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let list_len = self
            .build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")?
            .into_int_value();
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "comp_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_i8 = self
            .build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")?
            .into_pointer_value();
        let data_ptr = self
            .build_bit_cast(
                data_i8.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "data_i64",
            )?
            .into_pointer_value();
        Ok((list_ptr, list_len, data_ptr))
    }

    fn allocate_comprehension_output(
        &mut self,
        list_len: inkwell::values::IntValue<'ctx>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let i64_ty = self.context.i64_type();
        let elem_size = i64_ty.const_int(8, false);
        let alloc_size = self
            .builder
            .build_int_mul(list_len, elem_size, "comp_alloc")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let out_ptr = self.malloc_or_abort(alloc_size, "comp_malloc")?;
        let out_i64 = self
            .build_bit_cast(
                out_ptr.into(),
                self.context
                    .ptr_type(inkwell::AddressSpace::default())
                    .into(),
                "out_i64",
            )?
            .into_pointer_value();
        Ok((out_i64, out_ptr))
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_comprehension_loop(
        &mut self,
        expr: &Expr,
        var: &str,
        guard: &Option<Box<Expr>>,
        _list_ptr: inkwell::values::PointerValue<'ctx>,
        list_len: inkwell::values::IntValue<'ctx>,
        data_ptr: inkwell::values::PointerValue<'ctx>,
        out_i64: inkwell::values::PointerValue<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<
        (
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::PointerValue<'ctx>,
        ),
        CompileError,
    > {
        let i64_ty = self.context.i64_type();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for comprehension".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "comp_loop");
        let body_bb = self.context.append_basic_block(function, "comp_body");
        let done_bb = self.context.append_basic_block(function, "comp_done");
        let idx_alloca = self.build_alloca(i64_ty, "ci")?;
        let wi_alloca = self.build_alloca(i64_ty, "cw")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        self.build_store(wi_alloca, i64_ty.const_int(0, false))?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(loop_bb);
        let idx = self
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cmp, body_bb, done_bb)?;

        self.builder.position_at_end(body_bb);
        let elem_ptr = self.build_in_bounds_gep(i64_ty, data_ptr, &[idx], "elem")?;
        let elem = self.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")?;
        let mut comp_vars = vars.clone();
        let elem_alloca = self.build_alloca(i64_ty, var)?;
        self.build_store(elem_alloca, elem)?;
        comp_vars.insert(
            var.to_string(),
            (elem_alloca, BasicTypeEnum::IntType(i64_ty)),
        );

        let include = self.eval_guard(guard, &comp_vars, i64_ty)?;
        let store_bb = self.context.append_basic_block(function, "comp_store");
        let next_bb = self.context.append_basic_block(function, "comp_next");
        self.build_cond_br(include, store_bb, next_bb)?;

        self.builder.position_at_end(store_bb);
        self.emit_comprehension_store(expr, &comp_vars, out_i64, wi_alloca, i64_ty)?;
        self.build_br(next_bb)?;

        self.builder.position_at_end(next_bb);
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(idx_alloca, next)?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(done_bb);
        Ok((idx_alloca, wi_alloca))
    }

    fn eval_guard(
        &mut self,
        guard: &Option<Box<Expr>>,
        comp_vars: &HashMap<String, VarEntry<'ctx>>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let Some(g) = guard else {
            return Ok(self.context.bool_type().const_int(1, false));
        };
        let g_val = self.compile_expr(g, comp_vars)?;
        let BasicValueEnum::IntValue(iv) = g_val else {
            return Err("guard must be boolean".into());
        };
        let g_bool = self
            .builder
            .build_int_z_extend(iv, i64_ty, "g_ext")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        self.builder
            .build_int_compare(
                inkwell::IntPredicate::NE,
                g_bool,
                i64_ty.const_int(0, false),
                "g_truthy",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))
    }

    fn emit_comprehension_store(
        &mut self,
        expr: &Expr,
        comp_vars: &HashMap<String, VarEntry<'ctx>>,
        out_i64: inkwell::values::PointerValue<'ctx>,
        wi_alloca: inkwell::values::PointerValue<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> Result<(), CompileError> {
        let result = self.compile_expr(expr, comp_vars)?;
        let wi = self
            .build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "wi")?
            .into_int_value();
        let out_elem_ptr = self.build_in_bounds_gep(i64_ty, out_i64, &[wi], "out_elem")?;
        let result_i64 = match result {
            BasicValueEnum::IntValue(iv) => {
                // List slots are always i64 — extend i32 values before storing.
                if iv.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "comp_elem_sext")
                        .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
                } else {
                    iv
                }
            }
            BasicValueEnum::FloatValue(fv) => self
                .builder
                .build_float_to_signed_int(fv, i64_ty, "f_to_i")
                .map_err(|e| CompileError::LlvmError(format!("fptosi error: {}", e)))?,
            BasicValueEnum::PointerValue(pv) => self.build_ptr_to_int(pv, i64_ty, "p_to_i")?,
            _ => return Err("comprehension expression must produce i64-compatible value".into()),
        };
        self.build_store(out_elem_ptr, result_i64)?;
        let next_wi = self
            .builder
            .build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(wi_alloca, next_wi)?;
        Ok(())
    }

    fn build_comprehension_result(
        &self,
        result_len: inkwell::values::IntValue<'ctx>,
        out_ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let list_struct_ty = self.list_struct_type();
        let result_alloca = self.build_alloca(list_struct_ty, "comp_result")?;
        let rlen_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 0, "rlen")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(rlen_gep, result_len)?;
        let rdata_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, result_alloca, 1, "rdata")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let out_void = self.build_pointer_cast(out_ptr, i8_ptr, "out_void")?;
        self.build_store(rdata_gep, out_void)?;
        Ok(result_alloca.into())
    }
}
