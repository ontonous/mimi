use crate::ast::*;
use crate::codegen::types;
use crate::codegen::{call_try_basic_value, CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn build_string_list(
        &self,
        strings: &[String],
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let count = strings.len() as u64;

        // Allocate array of string structs: [ { i8*, i64 } x N ]
        let str_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let arr_type = str_ty.array_type(count as u32);
        let arr_alloca = self.build_alloca(BasicTypeEnum::ArrayType(arr_type), "str_arr")?;

        for (i, s) in strings.iter().enumerate() {
            let global = self
                .builder
                .build_global_string_ptr(s, &format!("str_{}", i))
                .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
            let elem_ptr = self
                .gep()
                .build_struct_gep(
                    BasicTypeEnum::StructType(str_ty),
                    arr_alloca,
                    i as u32,
                    &format!("elem_{}", i),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let ptr_gep = self
                .gep()
                .build_struct_gep(str_ty, elem_ptr, 0, "ptr")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(ptr_gep, global.as_pointer_value())?;
            let len_gep = self
                .gep()
                .build_struct_gep(str_ty, elem_ptr, 1, "len")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(len_gep, i64_ty.const_int(s.len() as u64, false))?;
        }

        // Build list struct: { i64 len, i8* data }
        let list_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );
        let list_alloca = self.build_alloca(list_ty, "str_list")?;
        let len_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 0, "len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(len_gep, i64_ty.const_int(count, false))?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, list_alloca, 1, "data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let arr_void_ptr = self.build_bit_cast(
            BasicValueEnum::PointerValue(arr_alloca),
            BasicTypeEnum::PointerType(i8_ptr),
            "arr_void",
        )?;
        self.build_store(data_gep, arr_void_ptr)?;
        Ok(list_alloca.into())
    }
    /// Determine if an expression evaluates to a string type (for len() dispatch).
    pub(in crate::codegen) fn expr_is_string(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Literal(Lit::String(_)) | Expr::Literal(Lit::FString(_)) => true,
            Expr::Ident(name) => self
                .var_type_names
                .get(name)
                .map(|t| t == "string")
                .unwrap_or(false),
            Expr::Call(callee, _) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    matches!(
                        name.as_str(),
                        "to_string"
                            | "int_to_string"
                            | "float_to_string"
                            | "chr"
                            | "str_char_at"
                            | "str_substring"
                            | "str_trim"
                            | "str_to_upper"
                            | "str_to_lower"
                            | "str_repeat"
                            | "str_replace"
                            | "str_join"
                            | "type_name"
                            | "from_json"
                            | "c_str_to_string"
                    )
                } else if let Expr::Field(_, method) = callee.as_ref() {
                    matches!(
                        method.as_str(),
                        "to_string"
                            | "trim"
                            | "to_upper"
                            | "to_lower"
                            | "repeat"
                            | "replace"
                            | "char_at"
                            | "substring"
                    )
                } else {
                    false
                }
            }
            Expr::Field(_, method) => {
                matches!(
                    method.as_str(),
                    "to_string"
                        | "trim"
                        | "to_upper"
                        | "to_lower"
                        | "repeat"
                        | "replace"
                        | "char_at"
                        | "substring"
                )
            }
            Expr::Turbofish(name, _, _) => matches!(name.as_str(), "to_string"),
            Expr::Binary(BinOp::Add, lhs, _) => self.expr_is_string(lhs),
            Expr::If { then_, else_, .. } => {
                if let Some(Stmt::Expr(e)) = then_.last() {
                    if self.expr_is_string(e) {
                        return true;
                    }
                }
                if let Some(else_block) = else_ {
                    if let Some(Stmt::Expr(e)) = else_block.last() {
                        if self.expr_is_string(e) {
                            return true;
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// P0-3: convert a bool source expression to a C string pointer
    /// suitable for `%s` printf formatting. Returns `Some(ptr)` for
    /// known bool literals (compile-time string globals) and for
    /// variables whose tracked Mimi type is `bool` (runtime
    /// `select` between "true"/"false" globals). Returns `None` for
    /// other expression kinds; the caller leaves the original
    /// compiled value alone.
    pub(in crate::codegen) fn maybe_bool_to_string(
        &self,
        expr: &Expr,
        value: BasicValueEnum<'ctx>,
    ) -> Option<BasicValueEnum<'ctx>> {
        let build_global = |s: &str, name: &str| -> Option<inkwell::values::PointerValue<'ctx>> {
            Some(
                self.builder
                    .build_global_string_ptr(s, name)
                    .ok()?
                    .as_pointer_value(),
            )
        };
        let make_string_value =
            |pv: inkwell::values::PointerValue<'ctx>| -> BasicValueEnum<'ctx> { pv.into() };
        match expr {
            Expr::Literal(Lit::Bool(true)) => {
                Some(make_string_value(build_global("true", "bool_true_lit")?))
            }
            Expr::Literal(Lit::Bool(false)) => {
                Some(make_string_value(build_global("false", "bool_false_lit")?))
            }
            Expr::Ident(name) => {
                let is_bool = self
                    .var_type_names
                    .get(name)
                    .map(|t| t == "bool")
                    .unwrap_or(false);
                if !is_bool {
                    return None;
                }
                let true_global = build_global("true", "bool_true_var")?;
                let false_global = build_global("false", "bool_false_var")?;
                let cond = match value {
                    BasicValueEnum::IntValue(iv) => self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            iv,
                            self.context.i64_type().const_int(0, false),
                            "bool_ne_zero",
                        )
                        .ok()?,
                    _ => return None,
                };
                let selected = self
                    .builder
                    .build_select(
                        cond,
                        BasicValueEnum::PointerValue(true_global),
                        BasicValueEnum::PointerValue(false_global),
                        "bool_str",
                    )
                    .ok()?;
                Some(selected)
            }
            _ => None,
        }
    }
    /// Determine the Mimi Type of an expression by resolving through the
    /// caller's type_map. Used to infer callee generic bindings at call sites.
    pub(in crate::codegen) fn expr_type_of(
        &self,
        expr: &Expr,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Option<Type> {
        match expr {
            Expr::Ident(name) => {
                // Prefer var_types (full AST type with generic args) for generic
                // inference. Fall back to var_type_names (string-based) for types
                // like "List<i32>" that can be parsed from a string.
                if let Some(ty) = self.var_types.get(name) {
                    Some(self.resolve_type(ty))
                } else if let Some(tn) = self.var_type_names.get(name) {
                    if let Some(parsed) = crate::codegen::expr::call::helpers::parse_type_str(tn) {
                        Some(self.resolve_type(&parsed))
                    } else {
                        let raw = Type::Name(tn.clone(), vec![]);
                        Some(self.resolve_type(&raw))
                    }
                } else {
                    None
                }
            }
            Expr::Literal(lit) => match lit {
                Lit::Int(_) => Some(Type::Name("i32".to_string(), vec![])),
                Lit::Float(_) => Some(Type::Name("f64".to_string(), vec![])),
                Lit::Bool(_) => Some(Type::Name("bool".to_string(), vec![])),
                Lit::String(_) | Lit::FString(_) => Some(Type::Name("string".to_string(), vec![])),
                _ => None,
            },
            Expr::Call(callee, _args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    self.func_defs
                        .get(name)
                        .and_then(|f| f.ret.clone())
                        .map(|t| self.resolve_type(&t))
                } else {
                    None
                }
            }
            Expr::Lambda { params, ret, .. } => {
                let ret_ty = ret.clone().unwrap_or(Type::Infer);
                let param_tys: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                Some(Type::Func(param_tys, Box::new(self.resolve_type(&ret_ty))))
            }
            _ => None,
        }
    }
    /// Check whether a Type contains a reference to a generic parameter name.
    pub(in crate::codegen) fn type_references_generic(ty: &Type, generic_name: &str) -> bool {
        match ty {
            Type::Name(name, args) => {
                if name == generic_name {
                    return true;
                }
                args.iter()
                    .any(|a| Self::type_references_generic(a, generic_name))
            }
            Type::Ref(_, inner) | Type::RefMut(_, inner) => {
                Self::type_references_generic(inner, generic_name)
            }
            Type::Option(inner) => Self::type_references_generic(inner, generic_name),
            Type::Result(ok, err) => {
                Self::type_references_generic(ok, generic_name)
                    || Self::type_references_generic(err, generic_name)
            }
            Type::Tuple(elems) => elems
                .iter()
                .any(|e| Self::type_references_generic(e, generic_name)),
            Type::Func(args, ret) => {
                args.iter()
                    .any(|a| Self::type_references_generic(a, generic_name))
                    || Self::type_references_generic(ret, generic_name)
            }
            Type::Shared(inner)
            | Type::LocalShared(inner)
            | Type::Weak(inner)
            | Type::WeakLocal(inner)
            | Type::RawPtr(inner)
            | Type::RawPtrMut(inner)
            | Type::CShared(inner)
            | Type::CBorrow(inner)
            | Type::CBorrowMut(inner)
            | Type::Slice(inner)
            | Type::CBuffer(inner)
            | Type::Array(inner, _) => Self::type_references_generic(inner, generic_name),
            Type::Newtype(_, inner) => Self::type_references_generic(inner, generic_name),
            Type::ExternFunc(args, ret) => {
                args.iter()
                    .any(|a| Self::type_references_generic(a, generic_name))
                    || Self::type_references_generic(ret, generic_name)
            }
            Type::Cap(_)
            | Type::Nothing
            | Type::Allocator
            | Type::Infer
            | Type::ImplTrait(_)
            | Type::DynTrait(_)
            | Type::RawString
            | Type::TypeVar(_)
            | Type::ForAll(_, _) => false,
        }
    }

    /// Compile a builtin intrinsic that requires compile-time knowledge (e.g.
    /// type introspection, higher-order list operations).
    pub(in crate::codegen) fn compile_builtin_intrinsic(
        &mut self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match name {
            "type_name" if args.len() == 1 => self.compile_type_name_intrinsic(args),
            "type_fields" if args.len() == 1 => self.compile_type_fields_intrinsic(args),
            "type_variants" if args.len() == 1 => self.compile_type_variants_intrinsic(args),
            "keys" | "values" if args.len() == 1 => {
                self.compile_keys_values_intrinsic(name, args, vars)
            }
            "map" | "filter" if args.len() == 2 => {
                self.compile_map_filter_intrinsic(name, args, vars)
            }
            "reduce" if args.len() == 3 => self.compile_reduce_intrinsic(args, vars),
            _ => Err(format!("unknown compile-time builtin '{}'", name).into()),
        }
    }

    // -------------------------------------------------------------------------
    // Intrinsic-specific private helpers
    // -------------------------------------------------------------------------

    /// `type_name(x)` -> string literal of the inferred type name.
    fn compile_type_name_intrinsic(
        &self,
        args: &[Expr],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let type_str = match &args[0] {
            Expr::Ident(var_name) => self
                .var_type_names
                .get(var_name)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            Expr::Literal(Lit::String(s)) => s.clone(),
            _ => "unknown".to_string(),
        };
        // Build string literal: { i8*, i64 }
        let global = self
            .builder
            .build_global_string_ptr(&type_str, "type_name")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let alloca = self.build_alloca(string_ty, "type_str")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, alloca, 0, "ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ptr_gep, global.as_pointer_value())?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, alloca, 1, "len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let len = self
            .context
            .i64_type()
            .const_int(type_str.len() as u64, false);
        self.build_store(len_gep, len)?;
        Ok(alloca.into())
    }

    /// `type_fields(t)` -> List<string> of record field names or enum variant names.
    fn compile_type_fields_intrinsic(
        &self,
        args: &[Expr],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let type_name_str = match &args[0] {
            Expr::Literal(Lit::String(s)) => s.clone(),
            Expr::Ident(var) => self
                .var_type_names
                .get(var)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            _ => return Err("type_fields: argument must be a type name string".into()),
        };
        let field_names: Vec<String> = self
            .type_defs
            .get(&type_name_str)
            .map(|td| match &td.kind {
                TypeDefKind::Record(fields) => fields.iter().map(|f| f.name.clone()).collect(),
                TypeDefKind::Enum(variants) => variants.iter().map(|v| v.name.clone()).collect(),
                _ => vec![],
            })
            .unwrap_or_default();
        // Build a List of field names
        self.build_string_list(&field_names, &HashMap::new())
    }

    /// `type_variants(t)` -> List<string> of enum variant names.
    fn compile_type_variants_intrinsic(
        &self,
        args: &[Expr],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let type_name_str = match &args[0] {
            Expr::Literal(Lit::String(s)) => s.clone(),
            Expr::Ident(var) => self
                .var_type_names
                .get(var)
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            _ => return Err("type_variants: argument must be a type name string".into()),
        };
        let variant_names: Vec<String> = self
            .type_defs
            .get(&type_name_str)
            .map(|td| match &td.kind {
                TypeDefKind::Enum(variants) => variants.iter().map(|v| v.name.clone()).collect(),
                _ => vec![],
            })
            .unwrap_or_default();
        self.build_string_list(&variant_names, &HashMap::new())
    }

    /// `keys(m)` / `values(m)` -> compile-time record reflection or runtime map builtin.
    fn compile_keys_values_intrinsic(
        &mut self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let var_name = match &args[0] {
            Expr::Ident(n) => n.clone(),
            _ => return Err("keys/values: argument must be a variable name".into()),
        };
        let type_name = self
            .var_type_names
            .get(&var_name)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        // Try compile-time record type first
        let is_record = self
            .type_defs
            .get(&type_name)
            .map(|td| matches!(&td.kind, TypeDefKind::Record(_)))
            .unwrap_or(false);
        if is_record {
            let field_names: Vec<String> = self
                .type_defs
                .get(&type_name)
                .map(|td| match &td.kind {
                    TypeDefKind::Record(fields) => fields.iter().map(|f| f.name.clone()).collect(),
                    _ => vec![],
                })
                .unwrap_or_default();
            if name == "keys" {
                return self.build_string_list(&field_names, vars);
            }
            // values: extract field values from record
            return self.compile_record_values_intrinsic(&type_name, &field_names, &args[0], vars);
        }
        // Runtime map fallback: compile arg and call builtin
        let compiled_arg = self.compile_expr(&args[0], vars)?;
        let metadata_arg = match compiled_arg {
            BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
            BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(pv),
            _ => return Err("keys/values: runtime fallback expects i64 or pointer".into()),
        };
        self.compile_builtin_call(name, &[metadata_arg])
            .map_err(|e| CompileError::Generic(e.to_string()))
    }

    /// Helper for `values(record)`: build a List<i64> of field values.
    fn compile_record_values_intrinsic(
        &mut self,
        type_name: &str,
        field_names: &[String],
        arg: &Expr,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let field_count = field_names.len();
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let struct_ty = self
            .type_llvm
            .get(type_name)
            .copied()
            .and_then(|t| match t {
                BasicTypeEnum::StructType(st) => Some(st),
                _ => None,
            });
        let struct_ty = struct_ty.ok_or_else(|| {
            CompileError::Generic(format!("values: no LLVM struct type for '{}'", type_name))
        })?;

        let sizeof_i64 = i64_ty.const_int(8, false);
        let alloc_size = self
            .builder
            .build_int_mul(
                i64_ty.const_int(field_count as u64, false),
                sizeof_i64,
                "values_alloc_size",
            )
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let values_data = self
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(alloc_size)],
                "values_malloc",
            )?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        let values_data_i64 = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(values_data),
                BasicTypeEnum::PointerType(i8_ptr),
                "values_data_i64",
            )?
            .into_pointer_value();
        let record_ptr = match self.compile_expr(arg, _vars)? {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("values: expected record pointer".into()),
        };
        let type_def = self
            .type_defs
            .get(type_name)
            .ok_or_else(|| format!("values: unknown type '{}'", type_name))?;
        if let TypeDefKind::Record(fields) = &type_def.kind {
            for (i, field) in fields.iter().enumerate() {
                let gep = self
                    .gep()
                    .build_struct_gep(
                        BasicTypeEnum::StructType(struct_ty),
                        record_ptr,
                        i as u32,
                        &field.name,
                    )
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let field_ty = types::mimi_type_to_llvm(self.context, &field.ty)
                    .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                let val = self.build_load(field_ty, gep, &field.name)?;
                let val_i64 = match val {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::FloatValue(fv) => self
                        .builder
                        .build_float_to_unsigned_int(fv, i64_ty, "float_to_i64")
                        .map_err(|e| CompileError::LlvmError(format!("fptosi error: {}", e)))?,
                    BasicValueEnum::PointerValue(pv) => {
                        self.build_ptr_to_int(pv, i64_ty, "ptr_to_i64")?
                    }
                    _ => return Err("values: unsupported field type".into()),
                };
                // SAFETY: values_data_i64 is i64* from malloc; i is in-bounds (small constant index).
                let elem_ptr = {
                    self.gep().build_gep(
                        i64_ty,
                        values_data_i64,
                        &[i64_ty.const_int(i as u64, false)],
                        "values_elem",
                    )
                }
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.build_store(elem_ptr, val_i64)?;
            }
            let result_list_ty = self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            let result_alloca = self.build_alloca(result_list_ty, "values_result")?;
            let result_len_gep = self
                .gep()
                .build_struct_gep(result_list_ty, result_alloca, 0, "values_result_len")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(result_len_gep, i64_ty.const_int(field_count as u64, false))?;
            let result_data_gep = self
                .gep()
                .build_struct_gep(result_list_ty, result_alloca, 1, "values_result_data")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let values_data_void = self.build_bit_cast(
                BasicValueEnum::PointerValue(values_data),
                BasicTypeEnum::PointerType(i8_ptr),
                "values_data_void",
            )?;
            self.build_store(result_data_gep, values_data_void)?;
            return Ok(result_alloca.into());
        }
        Err("values: expected record type".into())
    }

    /// Ensure a compiled list value is available as a pointer. List values may
    /// be passed by pointer (the common case) or as a loaded struct (e.g. `self`
    /// inside a trait method). Store loaded structs into a fresh alloca so the
    /// rest of the higher-order list code can use a consistent pointer.
    fn list_value_to_ptr(
        &self,
        list_val: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        match list_val {
            BasicValueEnum::PointerValue(pv) => Ok(pv),
            BasicValueEnum::StructValue(sv) => {
                let list_ty = BasicTypeEnum::StructType(sv.get_type());
                let alloca = self
                    .build_alloca(list_ty, "list_struct_arg")
                    .map_err(|e| format!("list alloca: {}", e))?;
                self.build_store(alloca, sv)
                    .map_err(|e| format!("list store: {}", e))?;
                Ok(alloca)
            }
            _ => Err("expected a list value".to_string()),
        }
    }

    /// `map(list, fn)` / `filter(list, fn)` -> compile-time higher-order list operation.
    fn compile_map_filter_intrinsic(
        &mut self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let is_map = name == "map";
        // Compile the list expression
        let list_val = self.compile_expr(&args[0], vars)?;
        let list_ptr = self.list_value_to_ptr(list_val).map_err(|e| {
            CompileError::Generic(format!("map/filter: first arg must be a list: {}", e))
        })?;
        // Resolve function from second arg (identifier, lambda, or variable).
        // Track the declared return type for indirect calls so the LLVM call
        // type matches the actual function (avoiding i1-i64 ABI mismatches).
        enum FnRef<'ctx> {
            Named(inkwell::values::FunctionValue<'ctx>),
            Indirect {
                fn_ptr: inkwell::values::PointerValue<'ctx>,
                env_ptr: inkwell::values::PointerValue<'ctx>,
                ret_type: Option<Box<Type>>,
            },
        }
        let fn_ref = match &args[1] {
            Expr::Ident(n) => {
                let f = self
                    .module
                    .get_function(n)
                    .ok_or_else(|| format!("map/filter: function '{}' not compiled", n))?;
                FnRef::Named(f)
            }
            Expr::Lambda { params, ret, body } => {
                let closure_val = self.compile_lambda_expr(params, ret, body, vars)?;
                let (fn_ptr, env_ptr) = self.extract_closure_ptrs(closure_val)?;
                FnRef::Indirect {
                    fn_ptr,
                    env_ptr,
                    ret_type: ret.clone().map(Box::new),
                }
            }
            _ => {
                let val = self.compile_expr(&args[1], vars)?;
                match val {
                    BasicValueEnum::PointerValue(fp) => {
                        let null_env = self
                            .context
                            .ptr_type(inkwell::AddressSpace::default())
                            .const_null();
                        FnRef::Indirect {
                            fn_ptr: fp,
                            env_ptr: null_env,
                            ret_type: None,
                        }
                    }
                    _ => {
                        return Err(
                            "map/filter: second arg must be a function name, lambda, or function pointer"
                                .into(),
                        )
                    }
                }
            }
        };
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ));
        // Read list length and data pointer
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let list_len = self.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_i8 = self
            .build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")?
            .into_pointer_value();
        let data_ptr = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(data_i8),
                BasicTypeEnum::PointerType(i8_ptr),
                "data_i64",
            )?
            .into_pointer_value();
        // Build result list: allocate {i64 len, i8* data}
        let result_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );
        let result_alloca = self.build_alloca(result_ty, "map_result")?;
        // Allocate output data array (same len)
        let elem_size = i64_ty.const_int(8, false);
        let alloc_size = self
            .builder
            .build_int_mul(list_len.into_int_value(), elem_size, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?;
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let out_ptr = self
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(alloc_size)],
                "out_malloc",
            )?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        let out_i64 = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(out_ptr),
                BasicTypeEnum::PointerType(i8_ptr),
                "out_i64",
            )?
            .into_pointer_value();
        // Loop: for i in 0..len
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for hof loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "hof_loop");
        let body_bb = self.context.append_basic_block(function, "hof_body");
        let done_bb = self.context.append_basic_block(function, "hof_done");
        let idx_alloca = self.build_alloca(i64_ty, "hi")?;
        let write_idx = self.build_alloca(i64_ty, "wi")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        self.build_store(write_idx, i64_ty.const_int(0, false))?;
        self.build_br(loop_bb)?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")?
            .into_int_value();
        let loop_cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                list_len.into_int_value(),
                "cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(loop_cmp, body_bb, done_bb)?;
        self.builder.position_at_end(body_bb);
        // Load element (as i64 from the data array)
        let elem_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem_i64 = self.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")?;
        let elem_i64_int = elem_i64.into_int_value();
        // Try to convert i64 element to struct type for user-defined record elements.
        // Keep the original i64 handle (elem_i64_int) for storage back to the output
        // array; the converted struct is used only for passing to the closure.
        let (elem_for_call, is_converted) =
            if let Some(converted) = self.try_convert_list_element(elem_i64_int, &args[0], vars)? {
                (converted, true)
            } else {
                (elem_i64, false)
            };
        // Build metadata type from the actual call argument type
        let elem_meta = match &elem_for_call {
            BasicValueEnum::IntValue(iv) => BasicMetadataTypeEnum::IntType(iv.get_type()),
            BasicValueEnum::StructValue(sv) => BasicMetadataTypeEnum::StructType(sv.get_type()),
            BasicValueEnum::PointerValue(pv) => BasicMetadataTypeEnum::PointerType(pv.get_type()),
            BasicValueEnum::FloatValue(fv) => BasicMetadataTypeEnum::FloatType(fv.get_type()),
            _ => BasicMetadataTypeEnum::IntType(i64_ty),
        };
        // Call the function: fn(elem) or fn(env_ptr, elem)
        let result = match &fn_ref {
            FnRef::Named(fn_llvm) => {
                // Adjust element width to match the function's first parameter type.
                // After A1 restoration, i32 params expect i32 values, but list
                // elements are stored as i64 and must be truncated.
                let adjusted_elem = if let Some(param) = fn_llvm.get_nth_param(0) {
                    match (elem_for_call, param) {
                        (BasicValueEnum::IntValue(iv), BasicValueEnum::IntValue(param_iv)) => {
                            let arg_bw = iv.get_type().get_bit_width();
                            let param_bw = param_iv.get_type().get_bit_width();
                            if arg_bw == param_bw {
                                elem_for_call
                            } else if arg_bw > param_bw {
                                self.builder
                                    .build_int_truncate(iv, param_iv.get_type(), "map_elem_trunc")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("map elem trunc: {}", e))
                                    })?
                                    .into()
                            } else {
                                self.builder
                                    .build_int_s_extend(iv, param_iv.get_type(), "map_elem_sext")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("map elem sext: {}", e))
                                    })?
                                    .into()
                            }
                        }
                        _ => elem_for_call,
                    }
                } else {
                    elem_for_call
                };
                let adjusted_meta = match &adjusted_elem {
                    BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                    BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                    BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                    BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                    _ => BasicMetadataValueEnum::IntValue(elem_i64_int),
                };
                let fn_call = self.build_call(*fn_llvm, &[adjusted_meta], "fn_call")?;
                call_try_basic_value(&fn_call).ok_or("function returned void")?
            }
            FnRef::Indirect {
                fn_ptr,
                env_ptr,
                ret_type,
            } => {
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                // Use the declared return type for the indirect call so LLVM
                // reads the value with the correct ABI width. Fall back to i64
                // when the return type is not known.
                let ret_llvm = ret_type
                    .as_ref()
                    .and_then(|rt| types::mimi_type_to_llvm(self.context, rt))
                    .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                // Determine the lambda's first parameter type to adjust the
                // element width. After A1 restoration, i32 params expect i32
                // values, but list elements are stored as i64.
                let lambda_param_ty = if let Expr::Lambda { params, .. } = &args[1] {
                    params.first().and_then(|p| {
                        self.llvm_type_for(&p.ty)
                            .or_else(|| types::mimi_type_to_llvm(self.context, &p.ty))
                    })
                } else {
                    None
                };
                let (call_elem, call_meta) = if let Some(param_llvm) = lambda_param_ty {
                    let adjusted = match (elem_for_call, param_llvm) {
                        (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(param_it)) => {
                            let arg_bw = iv.get_type().get_bit_width();
                            let param_bw = param_it.get_bit_width();
                            if arg_bw == param_bw {
                                elem_for_call
                            } else if arg_bw > param_bw {
                                self.builder
                                    .build_int_truncate(iv, param_it, "map_indirect_trunc")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!(
                                            "map indirect trunc: {}",
                                            e
                                        ))
                                    })?
                                    .into()
                            } else {
                                self.builder
                                    .build_int_s_extend(iv, param_it, "map_indirect_sext")
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("map indirect sext: {}", e))
                                    })?
                                    .into()
                            }
                        }
                        _ => elem_for_call,
                    };
                    let m = match &adjusted {
                        BasicValueEnum::IntValue(iv) => {
                            BasicMetadataTypeEnum::IntType(iv.get_type())
                        }
                        BasicValueEnum::StructValue(sv) => {
                            BasicMetadataTypeEnum::StructType(sv.get_type())
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            BasicMetadataTypeEnum::PointerType(pv.get_type())
                        }
                        BasicValueEnum::FloatValue(fv) => {
                            BasicMetadataTypeEnum::FloatType(fv.get_type())
                        }
                        _ => elem_meta,
                    };
                    (adjusted, m)
                } else {
                    (elem_for_call, elem_meta)
                };
                let metadata_params = [BasicMetadataTypeEnum::PointerType(i8_ptr), call_meta];
                let indirect_fn_type =
                    types::build_fn_type_for(self.context, ret_llvm, &metadata_params);
                let fn_ptr_typed = self
                    .build_bit_cast(
                        BasicValueEnum::PointerValue(*fn_ptr),
                        BasicTypeEnum::PointerType(i8_ptr),
                        "fn_typed",
                    )?
                    .into_pointer_value();
                let call_args = vec![
                    BasicMetadataValueEnum::PointerValue(*env_ptr),
                    match &call_elem {
                        BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                        BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                        BasicValueEnum::PointerValue(pv) => {
                            BasicMetadataValueEnum::PointerValue(*pv)
                        }
                        BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                        _ => BasicMetadataValueEnum::IntValue(elem_i64_int),
                    },
                ];
                let fn_call = self
                    .builder
                    .build_indirect_call(indirect_fn_type, fn_ptr_typed, &call_args, "fn_call")
                    .map_err(|e| CompileError::LlvmError(format!("indirect call: {}", e)))?;
                call_try_basic_value(&fn_call).ok_or("function returned void")?
            }
        };
        if is_map {
            // For map: store result to output array (widen to i64 if needed)
            let out_elem_ptr = {
                self.gep()
                    .build_in_bounds_gep(i64_ty, out_i64, &[idx], "out_elem")
            }
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            // Widen integer result to i64 for uniform list storage.
            let store_val = match result {
                BasicValueEnum::IntValue(iv) => {
                    let bw = iv.get_type().get_bit_width();
                    if bw < 64 {
                        self.builder
                            .build_int_s_extend(iv, i64_ty, "map_result_sext")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("map result sext: {}", e))
                            })?
                            .into()
                    } else if bw > 64 {
                        self.builder
                            .build_int_truncate(iv, i64_ty, "map_result_trunc")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("map result trunc: {}", e))
                            })?
                            .into()
                    } else {
                        result
                    }
                }
                _ => result,
            };
            self.build_store(out_elem_ptr, store_val)?;
        } else {
            // For filter: if result is truthy (non-zero), store to output array
            let zero = i64_ty.const_int(0, false);
            // Zero-extend result to i64 for comparison (result may be i1 bool)
            let result_i64 = self
                .builder
                .build_int_z_extend(result.into_int_value(), i64_ty, "result_ext")
                .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
            let truthy = self
                .builder
                .build_int_compare(inkwell::IntPredicate::NE, result_i64, zero, "truthy")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
            let store_bb = self.context.append_basic_block(function, "filter_store");
            let next_bb = self.context.append_basic_block(function, "filter_next");
            self.build_cond_br(truthy, store_bb, next_bb)?;
            self.builder.position_at_end(store_bb);
            let wi = self
                .build_load(BasicTypeEnum::IntType(i64_ty), write_idx, "wi")?
                .into_int_value();
            let out_elem_ptr = {
                self.gep()
                    .build_in_bounds_gep(i64_ty, out_i64, &[wi], "out_elem")
            }
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            // Store original i64 handle (not the converted struct) because the
            // output array stores i64 values (ptr-to-int handles).
            let stored_val: BasicValueEnum<'ctx> = if is_converted {
                BasicValueEnum::IntValue(elem_i64_int)
            } else {
                elem_for_call
            };
            self.build_store(out_elem_ptr, stored_val)?;
            let next_wi = self
                .builder
                .build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
                .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
            self.build_store(write_idx, next_wi)?;
            self.build_br(next_bb)?;
            self.builder.position_at_end(next_bb);
        }
        // idx++
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(idx_alloca, next)?;
        self.build_br(loop_bb)?;
        self.builder.position_at_end(done_bb);
        // Store result list: len and data ptr
        let out_len = if is_map {
            list_len
        } else {
            self.build_load(BasicTypeEnum::IntType(i64_ty), write_idx, "out_len")?
        };
        let out_len_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 0, "out_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(out_len_gep, out_len)?;
        let out_data_gep = self
            .gep()
            .build_struct_gep(result_ty, result_alloca, 1, "out_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let out_void = self.build_bit_cast(
            BasicValueEnum::PointerValue(out_i64),
            BasicTypeEnum::PointerType(i8_ptr),
            "out_void",
        )?;
        self.build_store(out_data_gep, out_void)?;
        Ok(result_alloca.into())
    }

    /// `reduce(list, fn, init)` -> compile-time left fold over a list.
    fn compile_reduce_intrinsic(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // reduce(list, fn, init) - function reference version
        let list_val = self.compile_expr(&args[0], vars)?;
        let list_ptr = self.list_value_to_ptr(list_val).map_err(|e| {
            CompileError::Generic(format!("reduce: first arg must be a list: {}", e))
        })?;
        let init_val = self.compile_expr(&args[2], vars)?;

        // Reduce takes a 2-arg closure (acc, elem) -> acc. The codegen
        // path can either call a named user function directly, or invoke a
        // lambda value by extracting its {fn_ptr, env_ptr} pair and
        // dispatching via an indirect call inside the loop body.
        enum ReduceCallee<'ctx> {
            Direct(inkwell::values::FunctionValue<'ctx>),
            Indirect(
                inkwell::values::PointerValue<'ctx>,
                inkwell::values::PointerValue<'ctx>,
            ),
        }
        let callee = match &args[1] {
            Expr::Ident(n) => ReduceCallee::Direct(
                self.module
                    .get_function(n)
                    .ok_or_else(|| format!("reduce: function '{}' not compiled", n))?,
            ),
            Expr::Lambda { params, ret, body } => {
                let closure_val = self.compile_lambda_expr(params, ret, body, vars)?;
                let (fn_ptr, env_ptr) = self.extract_closure_ptrs(closure_val)?;
                ReduceCallee::Indirect(fn_ptr, env_ptr)
            }
            _ => {
                return Err(
                    "reduce: second arg must be a function name, lambda, or function pointer"
                        .into(),
                )
            }
        };

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        ));
        let len_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 0, "len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let list_len = self.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")?;
        let data_gep = self
            .gep()
            .build_struct_gep(list_struct_ty, list_ptr, 1, "data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_i8 = self
            .build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")?
            .into_pointer_value();
        let data_ptr = self
            .build_bit_cast(
                BasicValueEnum::PointerValue(data_i8),
                BasicTypeEnum::PointerType(i8_ptr),
                "data_i64",
            )?
            .into_pointer_value();
        let acc_alloca = self.build_alloca(i64_ty, "acc")?;
        self.build_store(acc_alloca, init_val)?;
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for reduce loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "reduce_loop");
        let body_bb = self.context.append_basic_block(function, "reduce_body");
        let done_bb = self.context.append_basic_block(function, "reduce_done");
        let idx_alloca = self.build_alloca(i64_ty, "ri")?;
        self.build_store(idx_alloca, i64_ty.const_int(0, false))?;
        self.build_br(loop_bb)?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")?
            .into_int_value();
        let loop_cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                idx,
                list_len.into_int_value(),
                "cmp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(loop_cmp, body_bb, done_bb)?;
        self.builder.position_at_end(body_bb);
        let elem_ptr = {
            self.gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "elem")
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let elem = self.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")?;
        let acc = self.build_load(BasicTypeEnum::IntType(i64_ty), acc_alloca, "acc")?;

        let fn_result = match callee {
            ReduceCallee::Direct(func) => self
                .build_call(
                    func,
                    &[
                        BasicMetadataValueEnum::IntValue(acc.into_int_value()),
                        BasicMetadataValueEnum::IntValue(elem.into_int_value()),
                    ],
                    "reduce_call",
                )?
                .try_as_basic_value_opt()
                .ok_or("function returned void")?,
            ReduceCallee::Indirect(fn_ptr, env_ptr) => {
                // Closure ABI: fn(env_ptr: i8*, acc: i64, elem: i64) -> i64
                let closure_fn_type = i64_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(i64_ty),
                        BasicMetadataTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                let fn_ptr_typed = self.build_pointer_cast(fn_ptr, i8_ptr, "reduce_fn_ptr")?;
                let call = self
                    .builder
                    .build_indirect_call(
                        closure_fn_type,
                        fn_ptr_typed,
                        &[
                            BasicMetadataValueEnum::PointerValue(env_ptr),
                            BasicMetadataValueEnum::IntValue(acc.into_int_value()),
                            BasicMetadataValueEnum::IntValue(elem.into_int_value()),
                        ],
                        "reduce_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("indirect call error: {}", e)))?;
                call_try_basic_value(&call)
                    .ok_or_else(|| CompileError::LlvmError("closure returned void".into()))?
            }
        };

        self.build_store(acc_alloca, fn_result)?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(idx_alloca, next)?;
        self.build_br(loop_bb)?;
        self.builder.position_at_end(done_bb);
        let result = self.build_load(BasicTypeEnum::IntType(i64_ty), acc_alloca, "result")?;
        Ok(result)
    }

    /// Wrap a raw C string pointer into the Mimi `{ ptr, len }` string struct.
    pub(in crate::codegen) fn wrap_raw_string_ptr(
        &self,
        ptr: inkwell::values::PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(ptr)],
                "strlen_call",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
            .into_int_value();
        self.build_string_struct(ptr, len)
    }

    /// Normalize a string value to its canonical {i8*, i64} struct form.
    /// If the value is already a struct, return as-is. If it's a raw pointer
    /// and the expression is typed as string, wrap it into the canonical struct.
    /// This ensures string values use a consistent LLVM representation in
    /// variable allocas, avoiding type mismatch between literal initializers
    /// (raw i8*) and subsequent stores (e.g. from `+` which returns {i8*, i64}).
    pub(in crate::codegen) fn normalize_string_value(
        &self,
        val: BasicValueEnum<'ctx>,
        expr: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match val {
            BasicValueEnum::PointerValue(pv) => {
                if self.expr_is_string(expr) {
                    self.wrap_raw_string_ptr(pv)
                } else {
                    Ok(val)
                }
            }
            _ => Ok(val),
        }
    }

    /// For direct calls to user-defined functions whose parameter is typed as
    /// `string`, wrap raw string-pointer arguments (string literals, format
    /// strings, etc.) into the Mimi string struct so the callee ABI matches.
    /// Extern functions keep the raw C-string pointer ABI and are not wrapped.
    pub(in crate::codegen) fn maybe_wrap_string_args_for_call(
        &mut self,
        name: &str,
        arg_exprs: &[Expr],
        compiled_args: &mut [BasicValueEnum<'ctx>],
    ) -> Result<(), CompileError> {
        let Some(fdef) = self.func_defs.get(name) else {
            return Ok(());
        };
        let param_types: Vec<Type> = fdef.params.iter().map(|p| p.ty.clone()).collect();
        for (i, (_arg_expr, compiled)) in arg_exprs.iter().zip(compiled_args.iter_mut()).enumerate()
        {
            if i >= param_types.len() {
                break;
            }
            if !Self::is_string_type(&param_types[i]) {
                continue;
            }
            // Any raw pointer argument passed to a `string` parameter must be
            // wrapped into the canonical {i8*, i64} struct. This covers string
            // literals, format strings, and list-element indexing like
            // `row[i]`, which returns a raw C-string pointer.
            if let BasicValueEnum::PointerValue(pv) = *compiled {
                *compiled = self.wrap_raw_string_ptr(pv)?;
            }
        }
        Ok(())
    }

    fn is_string_type(ty: &Type) -> bool {
        match ty {
            Type::Name(name, _) if name == "string" => true,
            Type::Ref(_, inner) | Type::RefMut(_, inner) => Self::is_string_type(inner),
            _ => false,
        }
    }
}

// ============================================================
// Generic type argument inference helpers (codegen monomorphization)
// ============================================================

/// Parse a type string produced by `fmt_type` back into an AST `Type`.
///
/// This is intentionally narrower than the full parser: it handles the shapes
/// that appear in `var_type_names` (`List<i32>`, `Result<T, string>`, etc.)
/// so that codegen can extract generic arguments during monomorphization.
pub(crate) fn parse_type_str(s: &str) -> Option<Type> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Option<T>
    if let Some(inner) = strip_balanced_suffix(s, "Option<") {
        return Some(Type::Option(Box::new(parse_type_str(inner)?)));
    }
    // Result<T, E>
    if let Some(inner) = strip_balanced_suffix(s, "Result<") {
        let parts = split_top_level(inner, ',');
        if parts.len() == 2 {
            return Some(Type::Result(
                Box::new(parse_type_str(parts[0])?),
                Box::new(parse_type_str(parts[1])?),
            ));
        }
    }
    // Generic named type: Name<args>
    if let Some(lt) = find_top_level(s, '<') {
        let base = s[..lt].trim();
        let rest = &s[lt + 1..];
        let rest = strip_balanced_suffix(rest, "")?; // strip trailing '>'
        let args = split_top_level(rest, ',')
            .into_iter()
            .map(parse_type_str)
            .collect::<Option<Vec<_>>>()?;
        return Some(Type::Name(base.to_string(), args));
    }
    // Tuple types: (A, B, C)
    if s.starts_with('(') && s.ends_with(')') {
        let inner = &s[1..s.len() - 1];
        let parts = split_top_level(inner, ',');
        let elems = parts
            .into_iter()
            .map(parse_type_str)
            .collect::<Option<Vec<_>>>()?;
        return Some(Type::Tuple(elems));
    }

    Some(Type::Name(s.to_string(), vec![]))
}

/// Find the index of `c` at the top level of a type string (not nested in
/// generic brackets or parentheses).
fn find_top_level(s: &str, c: char) -> Option<usize> {
    let mut depth = 0i32;
    for (i, ch) in s.char_indices() {
        if ch == c && depth == 0 {
            return Some(i);
        }
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// If `s` is wrapped in `prefix`...`>` with balanced brackets, strip the
/// wrapper and return the inner text. The empty prefix is used for generic
/// named types like `List<...>` after the base name has been consumed.
fn strip_balanced_suffix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if !prefix.is_empty() && !s.starts_with(prefix) {
        return None;
    }
    if !s.ends_with('>') {
        return None;
    }
    let inner_start = if prefix.is_empty() { 0 } else { prefix.len() };
    let inner = &s[inner_start..s.len() - 1];
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    Some(inner)
}

/// Split a string at top-level occurrences of `delim`.
fn split_top_level(s: &str, delim: char) -> Vec<&str> {
    let mut depth = 0i32;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, ch) in s.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            _ if ch == delim && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
        .into_iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Extract generic argument substitutions from a param/arg type pair.
///
/// `generics` is the list of generic parameter names for the callee. This
/// mirrors the inference performed by the type checker so that codegen can
/// pick the correct monomorphization for non-turbofish generic calls.
pub(crate) fn infer_generic_args(
    param: &Type,
    arg: &Type,
    generics: &[String],
    map: &mut HashMap<String, Type>,
) {
    let is_gen = |name: &str| generics.iter().any(|g| g == name);

    match param {
        Type::Name(p_name, p_args) if is_gen(p_name) => {
            if !occurs_in(name_of_type(arg), p_name) {
                map.entry(p_name.clone()).or_insert_with(|| arg.clone());
            }
        }
        Type::Name(p_name, p_args) => {
            if let Type::Name(a_name, a_args) = arg {
                if p_name == a_name && p_args.len() == a_args.len() {
                    for (pa, aa) in p_args.iter().zip(a_args.iter()) {
                        infer_generic_args(pa, aa, generics, map);
                    }
                }
            }
        }
        Type::Ref(_, p_inner) | Type::RefMut(_, p_inner) => {
            if let Type::Ref(_, a_inner) | Type::RefMut(_, a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::Option(p_inner) => {
            if let Type::Option(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::Result(p_ok, p_err) => {
            if let Type::Result(a_ok, a_err) = arg {
                infer_generic_args(p_ok, a_ok, generics, map);
                infer_generic_args(p_err, a_err, generics, map);
            }
        }
        Type::Tuple(p_elems) => {
            if let Type::Tuple(a_elems) = arg {
                if p_elems.len() == a_elems.len() {
                    for (pe, ae) in p_elems.iter().zip(a_elems.iter()) {
                        infer_generic_args(pe, ae, generics, map);
                    }
                }
            }
        }
        Type::Func(p_args, p_ret) => {
            if let Type::Func(a_args, a_ret) = arg {
                if p_args.len() == a_args.len() {
                    for (pa, aa) in p_args.iter().zip(a_args.iter()) {
                        infer_generic_args(pa, aa, generics, map);
                    }
                    infer_generic_args(p_ret, a_ret, generics, map);
                }
            }
        }
        Type::Shared(p_inner) => {
            if let Type::Shared(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::LocalShared(p_inner) => {
            if let Type::LocalShared(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::Weak(p_inner) => {
            if let Type::Weak(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::WeakLocal(p_inner) => {
            if let Type::WeakLocal(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::Slice(p_inner) => {
            if let Type::Slice(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::RawPtr(p_inner) => {
            if let Type::RawPtr(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        Type::RawPtrMut(p_inner) => {
            if let Type::RawPtrMut(a_inner) = arg {
                infer_generic_args(p_inner, a_inner, generics, map);
            }
        }
        _ => {}
    }
}

fn name_of_type(ty: &Type) -> Option<&str> {
    match ty {
        Type::Name(name, _) => Some(name),
        _ => None,
    }
}

fn occurs_in(arg_name: Option<&str>, generic_name: &str) -> bool {
    arg_name == Some(generic_name)
}
