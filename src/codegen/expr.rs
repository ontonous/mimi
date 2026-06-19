#![allow(dead_code, deprecated)]

use crate::ast::*;
use crate::codegen::types;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match expr {
            Expr::Literal(lit) => self.compile_literal_expr(lit, vars),
            Expr::Ident(name) => self.compile_ident_expr(name, vars),
            Expr::Binary(op, lhs, rhs) => self.compile_binary_expr(*op, lhs, rhs, vars),
            Expr::Unary(op, inner) => self.compile_unary_expr(*op, inner, vars),
            Expr::Call(callee, args) => self.compile_call_expr(callee, args, vars),
            Expr::Turbofish(name, type_args, args) => self.compile_turbofish_expr(name, type_args, args, vars),
            Expr::Match(scrutinee, arms) => self.compile_match_expr(scrutinee, arms, vars),
            Expr::Record { ty, fields } => self.compile_record_expr(ty, fields, vars),
            Expr::Field(obj, field_name) => self.compile_field_expr(obj, field_name, vars),
            Expr::List(elems) => self.compile_list_expr(elems, vars),
            Expr::Index(obj, idx_expr) => self.compile_index_expr(obj, idx_expr, vars),
            Expr::Spawn(expr) => self.compile_spawn_expr(expr, vars),
            Expr::Await(expr) => self.compile_await_expr(expr, vars),
            Expr::Try(inner) => self.compile_try_expr(inner, vars),
            Expr::TypeOf(inner) => self.compile_typeof_expr(inner, vars),
            Expr::TypeInfo(ty) => self.compile_typeinfo_expr(ty, vars),
            Expr::Old(inner) => self.compile_old_expr(inner, vars),
            Expr::Tuple(elems) => self.compile_tuple_expr(elems, vars),
            Expr::If { cond, then_, else_ } => self.compile_if_expr(cond, then_, else_, vars),
            Expr::Range { start, end } => self.compile_range_expr(start, end, vars),
            Expr::SliceExpr { target, start, end } => self.compile_slice_expr(target, start, end, vars),
            Expr::Lambda { params, ret, body } => self.compile_lambda_expr(params, ret, body, vars),
            Expr::Comprehension { expr: comp_expr, var, iter, guard } => self.compile_comprehension_expr(comp_expr, var, iter, guard, vars),
            Expr::Quote(_) | Expr::QuoteInterpolate(_) | Expr::Comptime(_) => {
                Err("quote/comptime expressions must be resolved before codegen".into())
            }
            #[allow(unreachable_patterns)]
            _ => Err(format!("unsupported expression in codegen: {:?}", expr))
        }
    }

    fn compile_literal_expr(
        &mut self,
        lit: &Lit,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match lit {
            Lit::Int(n) => Ok(self.context.i64_type().const_int(*n as u64, true).into()),
            Lit::Float(f) => Ok(self.context.f64_type().const_float(*f).into()),
            Lit::Bool(b) => Ok(self.context.bool_type().const_int(*b as u64, false).into()),
            Lit::Unit => Ok(self.context.i64_type().const_int(0, false).into()),
            Lit::String(s) => {
                let global = self.builder.build_global_string_ptr(s, "str")
                    .map_err(|e| format!("string error: {}", e))?;
                Ok(global.as_pointer_value().into())
            }
            Lit::FString(parts) => Ok(self.compile_fstring(parts, vars)?),
        }
    }

    fn compile_ident_expr(
        &mut self,
        name: &String,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if let Some(&(alloca, ty)) = vars.get(name) {
            if self.shared_var_names.contains(name.as_str()) {
                // Shared variable: the alloca stores a T* pointer to heap memory.
                // First load the pointer, then load the value from the heap.
                let ptr_ty = ty.ptr_type(inkwell::AddressSpace::default());
                let heap_ptr = self.builder.build_load(ptr_ty, alloca, name)
                    .map_err(|e| format!("shared heap ptr load error: {}", e))?;
                let heap_pointer = heap_ptr.into_pointer_value();
                self.builder.build_load(ty, heap_pointer, name)
                    .map_err(|e| format!("shared value load error: {}", e))
            } else {
                self.builder.build_load(ty, alloca, name)
                    .map_err(|e| format!("load error: {}", e))
            }
        } else if self.cap_type_names.contains(name.as_str()) {
            // Cap literal: call mimi_cap_register(name) to get handle
            if let Some(register_fn) = self.module.get_function("mimi_cap_register") {
                let name_global = self.builder.build_global_string_ptr(
                    &format!("{}\0", name), &format!("cap_name_{}", name))
                    .map_err(|e| format!("string global error: {}", e))?;
                let name_ptr = name_global.as_pointer_value();
                let handle = self.builder.build_call(register_fn, &[
                    BasicMetadataValueEnum::PointerValue(name_ptr),
                ], &format!("cap_register_{}", name))
                    .map_err(|e| format!("cap_register error: {}", e))?
                    .try_as_basic_value().left()
                    .ok_or("mimi_cap_register returned void")?;
                Ok(handle)
            } else {
                Err(format!("cap literal '{}' requires mimi_cap_register runtime", name))
            }
        } else {
            Err(format!("undefined variable '{}'", name))
        }
    }

    fn compile_binary_expr(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let l = self.compile_expr(lhs, vars)?;
        let r = self.compile_expr(rhs, vars)?;
        self.compile_binop(op, l, r)
    }

    fn compile_unary_expr(
        &mut self,
        op: UnOp,
        inner: &Box<Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let v = self.compile_expr(inner, vars)?;
        match op {
            UnOp::Neg => {
                if let BasicValueEnum::IntValue(iv) = v {
                    let zero = self.context.i64_type().const_int(0, true);
                    Ok(self.builder.build_int_sub(zero, iv, "neg")
                        .map_err(|e| format!("neg error: {}", e))?.into())
                } else if let BasicValueEnum::FloatValue(fv) = v {
                    let zero = self.context.f64_type().const_float(0.0);
                    Ok(self.builder.build_float_sub(zero, fv, "fneg")
                        .map_err(|e| format!("neg error: {}", e))?.into())
                } else {
                    let ty_desc = match v.get_type() {
                        inkwell::types::BasicTypeEnum::IntType(_) => "int",
                        inkwell::types::BasicTypeEnum::FloatType(_) => "float",
                        inkwell::types::BasicTypeEnum::PointerType(_) => "pointer",
                        inkwell::types::BasicTypeEnum::ArrayType(_) => "array",
                        inkwell::types::BasicTypeEnum::StructType(_) => "struct",
                        inkwell::types::BasicTypeEnum::VectorType(_) => "vector",
                    };
                    Err(format!("negation requires numeric type, got {}", ty_desc))
                }
            }
            UnOp::Not => {
                if let BasicValueEnum::IntValue(iv) = v {
                    Ok(self.builder.build_not(iv, "not")
                        .map_err(|e| format!("not error: {}", e))?.into())
                } else {
                    let ty_desc = match v.get_type() {
                        inkwell::types::BasicTypeEnum::IntType(_) => "int",
                        inkwell::types::BasicTypeEnum::FloatType(_) => "float",
                        inkwell::types::BasicTypeEnum::PointerType(_) => "pointer",
                        inkwell::types::BasicTypeEnum::ArrayType(_) => "array",
                        inkwell::types::BasicTypeEnum::StructType(_) => "struct",
                        inkwell::types::BasicTypeEnum::VectorType(_) => "vector",
                    };
                    Err(format!("'not' requires bool, got {}", ty_desc))
                }
            }
            UnOp::Ref | UnOp::RefMut => {
                let ty = v.get_type();
                let alloca = self.builder.build_alloca(ty, "ref")
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(alloca, v)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(alloca.into())
            }
            UnOp::Deref => {
                if let BasicValueEnum::PointerValue(ptr) = v {
                    // Try to determine the pointee type from the inner expression's variable entry
                    let pointee_ty = match inner.as_ref() {
                        Expr::Ident(name) => {
                            if let Some(&(_, ty)) = vars.get(name) {
                                ty
                            } else {
                                BasicTypeEnum::IntType(self.context.i64_type())
                            }
                        }
                        _ => BasicTypeEnum::IntType(self.context.i64_type()),
                    };
                    Ok(self.builder.build_load(pointee_ty, ptr, "deref")
                        .map_err(|e| format!("load error: {}", e))?)
                } else {
                    let ty_desc = match v.get_type() {
                        inkwell::types::BasicTypeEnum::IntType(_) => "int",
                        inkwell::types::BasicTypeEnum::FloatType(_) => "float",
                        inkwell::types::BasicTypeEnum::PointerType(_) => "pointer",
                        inkwell::types::BasicTypeEnum::ArrayType(_) => "array",
                        inkwell::types::BasicTypeEnum::StructType(_) => "struct",
                        inkwell::types::BasicTypeEnum::VectorType(_) => "vector",
                    };
                    Err(format!("dereference requires pointer type, got {}", ty_desc))
                }
            }
        }
    }
    fn compile_call_expr(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match callee {
            Expr::Ident(name) => {
                // Compile-time builtins: resolved at codegen time, not runtime
                match name.as_str() {
                    "type_name" if args.len() == 1 => {
                        let type_str = match &args[0] {
                            Expr::Ident(var_name) => self.var_type_names.get(var_name)
                                .cloned().unwrap_or_else(|| "unknown".to_string()),
                            Expr::Literal(Lit::String(s)) => s.clone(),
                            _ => "unknown".to_string(),
                        };
                        // Build string literal: { i8*, i64 }
                        let global = self.builder.build_global_string_ptr(&type_str, "type_name")
                            .map_err(|e| format!("global string error: {}", e))?;
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let string_ty = self.context.struct_type(&[
                            BasicTypeEnum::PointerType(i8_ptr),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        let alloca = self.builder.build_alloca(string_ty, "type_str")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let ptr_gep = self.builder.build_struct_gep(string_ty, alloca, 0, "ptr")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(ptr_gep, global.as_pointer_value())
                            .map_err(|e| format!("store error: {}", e))?;
                        let len_gep = self.builder.build_struct_gep(string_ty, alloca, 1, "len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let len = self.context.i64_type().const_int(type_str.len() as u64, false);
                        self.builder.build_store(len_gep, len)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(alloca.into())
                    }
                    "type_fields" if args.len() == 1 => {
                        let type_name_str = match &args[0] {
                            Expr::Literal(Lit::String(s)) => s.clone(),
                            Expr::Ident(var) => self.var_type_names.get(var)
                                .cloned().unwrap_or_else(|| "unknown".to_string()),
                            _ => return Err("[E0712] type_fields: argument must be a type name string".into()),
                        };
                        let field_names: Vec<String> = self.type_defs.get(&type_name_str)
                            .map(|td| match &td.kind {
                                TypeDefKind::Record(fields) => {
                                    fields.iter().map(|f| f.name.clone()).collect()
                                }
                                TypeDefKind::Enum(variants) => {
                                    variants.iter().map(|v| v.name.clone()).collect()
                                }
                                _ => vec![],
                            })
                            .unwrap_or_default();
                        // Build a List of field names
                        self.build_string_list(&field_names, vars)
                    }
                    "type_variants" if args.len() == 1 => {
                        let type_name_str = match &args[0] {
                            Expr::Literal(Lit::String(s)) => s.clone(),
                            Expr::Ident(var) => self.var_type_names.get(var)
                                .cloned().unwrap_or_else(|| "unknown".to_string()),
                            _ => return Err("[E0712] type_variants: argument must be a type name string".into()),
                        };
                        let variant_names: Vec<String> = self.type_defs.get(&type_name_str)
                            .map(|td| match &td.kind {
                                TypeDefKind::Enum(variants) => {
                                    variants.iter().map(|v| v.name.clone()).collect()
                                }
                                _ => vec![],
                            })
                            .unwrap_or_default();
                        self.build_string_list(&variant_names, vars)
                    }
                    "keys" | "values" if args.len() == 1 => {
                        let var_name = match &args[0] {
                            Expr::Ident(n) => n.clone(),
                            _ => return Err("keys/values: argument must be a variable name".into()),
                        };
                        let type_name = self.var_type_names.get(&var_name)
                            .cloned().unwrap_or_else(|| "unknown".to_string());
                        let field_names: Vec<String> = self.type_defs.get(&type_name)
                            .map(|td| match &td.kind {
                                TypeDefKind::Record(fields) => {
                                    fields.iter().map(|f| f.name.clone()).collect()
                                }
                                _ => vec![],
                            })
                            .unwrap_or_default();
                        if name == "keys" {
                            self.build_string_list(&field_names, vars)
                        } else {
                            let field_count = field_names.len();
                            let llvm_ty = self.type_llvm.get(&type_name).cloned();
                            if let Some(BasicTypeEnum::StructType(_struct_ty)) = llvm_ty {
                                let i64_ty = self.context.i64_type();
                                let sizeof_i64 = i64_ty.const_int(8, false);
                                let alloc_size = self.builder.build_int_mul(
                                    i64_ty.const_int(field_count as u64, false),
                                    sizeof_i64,
                                    "values_alloc_size"
                                ).map_err(|e| format!("mul error: {}", e))?;
                                let malloc_fn = self.module.get_function("malloc")
                                    .ok_or_else(|| "malloc not declared".to_string())?;
                                let values_data = self.builder.build_call(malloc_fn, &[
                                    BasicMetadataValueEnum::IntValue(alloc_size),
                                ], "values_malloc")
                                    .map_err(|e| format!("malloc error: {}", e))?
                                    .try_as_basic_value().left()
                                    .ok_or("malloc returned void")?
                                    .into_pointer_value();
                                let values_data_i64 = self.builder.build_bit_cast(values_data,
                                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "values_data_i64")
                                    .map_err(|e| format!("bitcast error: {}", e))?
                                    .into_pointer_value();
                                let record_ptr = match self.compile_expr(&args[0], vars)? {
                                    BasicValueEnum::PointerValue(pv) => pv,
                                    _ => return Err("[E0712] values: expected record pointer".into()),
                                };
                                let td = self.type_defs.get(&type_name);
                                if let Some(TypeDefKind::Record(fields)) = td.map(|t| &t.kind) {
                                    for (i, field) in fields.iter().enumerate() {
                                        let gep = self.builder.build_struct_gep(_struct_ty, record_ptr, i as u32, &field.name)
                                            .map_err(|e| format!("gep error: {}", e))?;
                                        let field_ty = types::mimi_type_to_llvm(self.context, &field.ty)
                                            .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                                        let val = self.builder.build_load(field_ty, gep, &field.name)
                                            .map_err(|e| format!("load error: {}", e))?;
                                        let val_i64 = match val {
                                            BasicValueEnum::IntValue(iv) => iv,
                                            BasicValueEnum::FloatValue(fv) => self.builder.build_float_to_unsigned_int(fv, i64_ty, "float_to_i64")
                                                .map_err(|e| format!("fptosi error: {}", e))?,
                                            BasicValueEnum::PointerValue(pv) => self.builder.build_ptr_to_int(pv, i64_ty, "ptr_to_i64")
                                                .map_err(|e| format!("ptrtoint error: {}", e))?,
                                            _ => return Err("[E0701] values: unsupported field type".into()),
                                        };
                                        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                                        let elem_ptr = unsafe { self.builder.build_gep(i64_ty, values_data_i64, &[i64_ty.const_int(i as u64, false)], "values_elem") }
                                            .map_err(|e| format!("gep error: {}", e))?;
                                        self.builder.build_store(elem_ptr, val_i64)
                                            .map_err(|e| format!("store error: {}", e))?;
                                    }
                                    let result_list_ty = self.context.struct_type(&[
                                        BasicTypeEnum::IntType(i64_ty),
                                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                                    ], false);
                                    let result_alloca = self.builder.build_alloca(result_list_ty, "values_result")
                                        .map_err(|e| format!("alloca error: {}", e))?;
                                    let result_len_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 0, "values_result_len")
                                        .map_err(|e| format!("gep error: {}", e))?;
                                    self.builder.build_store(result_len_gep, i64_ty.const_int(field_count as u64, false))
                                        .map_err(|e| format!("store error: {}", e))?;
                                    let result_data_gep = self.builder.build_struct_gep(result_list_ty, result_alloca, 1, "values_result_data")
                                        .map_err(|e| format!("gep error: {}", e))?;
                                    let values_data_void = self.builder.build_bit_cast(values_data,
                                        self.context.ptr_type(inkwell::AddressSpace::default()), "values_data_void")
                                        .map_err(|e| format!("bitcast error: {}", e))?;
                                    self.builder.build_store(result_data_gep, values_data_void)
                                        .map_err(|e| format!("store error: {}", e))?;
                                    Ok(result_alloca.into())
                                } else {
                                    Err("values: argument must be a record type".into())
                                }
                            } else {
                                Err("values: type is not a struct".into())
                            }
                        }
                    }
                    // map/list, fn_ref): compile-time list iteration + function call
                    "map" | "filter" if args.len() == 2 => {
                        let is_map = name == "map";
                        // Compile the list expression
                        let list_val = self.compile_expr(&args[0], vars)?;
                        let list_ptr = match list_val {
                            BasicValueEnum::PointerValue(pv) => pv,
                            _ => return Err("map/filter: first arg must be a list".into()),
                        };
                        // Resolve function name from second arg (must be an identifier)
                        let fn_name = match &args[1] {
                            Expr::Ident(n) => n.clone(),
                            _ => return Err("map/filter: second arg must be a function name (identifier)".into()),
                        };
                        let fn_llvm = self.module.get_function(&fn_name)
                            .ok_or_else(|| format!("map/filter: function '{}' not compiled", fn_name))?;
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let i64_ty = self.context.i64_type();
                        let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                            BasicTypeEnum::IntType(i64_ty),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false));
                        // Read list length and data pointer
                        let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                            .map_err(|e| format!("load error: {}", e))?;
                        let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")
                            .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                        let data_ptr = self.builder.build_bit_cast(data_i8,
                            i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                            .map_err(|e| format!("bitcast error: {}", e))?
                            .into_pointer_value();
                        // Build result list: allocate {i64 len, i8* data}
                        let result_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(i64_ty),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false);
                        let result_alloca = self.builder.build_alloca(result_ty, "map_result")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        // Allocate output data array (same len)
                        let elem_size = i64_ty.const_int(8, false);
                        let alloc_size = self.builder.build_int_mul(list_len.into_int_value(), elem_size, "alloc_size")
                            .map_err(|e| format!("mul error: {}", e))?;
                        let malloc_fn = self.module.get_function("malloc")
                            .ok_or_else(|| "malloc not declared".to_string())?;
                        let out_ptr = self.builder.build_call(malloc_fn, &[
                            BasicMetadataValueEnum::IntValue(alloc_size),
                        ], "out_malloc")
                            .map_err(|e| format!("malloc error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("malloc returned void")?
                            .into_pointer_value();
                        let out_i64 = self.builder.build_bit_cast(out_ptr,
                            i64_ty.ptr_type(inkwell::AddressSpace::default()), "out_i64")
                            .map_err(|e| format!("bitcast error: {}", e))?
                            .into_pointer_value();
                        // Loop: for i in 0..len
                        let function = self.current_function().ok_or_else(|| "codegen: no current function for hof loop".to_string())?;
                        let loop_bb = self.context.append_basic_block(function, "hof_loop");
                        let body_bb = self.context.append_basic_block(function, "hof_body");
                        let done_bb = self.context.append_basic_block(function, "hof_done");
                        let idx_alloca = self.builder.build_alloca(i64_ty, "hi")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        let write_idx = self.builder.build_alloca(i64_ty, "wi")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_store(write_idx, i64_ty.const_int(0, false))
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(loop_bb);
                        let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                            .map_err(|e| format!("load error: {}", e))?.into_int_value();
                        let loop_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len.into_int_value(), "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        self.builder.build_conditional_branch(loop_cmp, body_bb, done_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(body_bb);
                        // Load element
                        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                        let elem_ptr = unsafe {
                            self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                        }.map_err(|e| format!("gep error: {}", e))?;
                        let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                            .map_err(|e| format!("load error: {}", e))?;
                        // Call the function: fn(elem)
                        let fn_call = self.builder.build_call(fn_llvm, &[
                            BasicMetadataValueEnum::IntValue(elem.into_int_value()),
                        ], "fn_call")
                            .map_err(|e| format!("call error: {}", e))?;
                        let result = fn_call.try_as_basic_value().left()
                            .ok_or("function returned void")?;
                        if is_map {
                            // For map: store result to output array
                            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                            let out_elem_ptr = unsafe {
                                self.builder.build_gep(i64_ty, out_i64, &[idx], "out_elem")
                            }.map_err(|e| format!("gep error: {}", e))?;
                            self.builder.build_store(out_elem_ptr, result)
                                .map_err(|e| format!("store error: {}", e))?;
                        } else {
                            // For filter: if result is truthy (non-zero), store to output array
                            let zero = i64_ty.const_int(0, false);
                            // Zero-extend result to i64 for comparison (result may be i1 bool)
                            let result_i64 = self.builder.build_int_z_extend(result.into_int_value(), i64_ty, "result_ext")
                                .map_err(|e| format!("zext error: {}", e))?;
                            let truthy = self.builder.build_int_compare(inkwell::IntPredicate::NE, result_i64, zero, "truthy")
                                .map_err(|e| format!("cmp error: {}", e))?;
                            let store_bb = self.context.append_basic_block(function, "filter_store");
                            let next_bb = self.context.append_basic_block(function, "filter_next");
                            self.builder.build_conditional_branch(truthy, store_bb, next_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            self.builder.position_at_end(store_bb);
                            let wi = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), write_idx, "wi")
                                .map_err(|e| format!("load error: {}", e))?.into_int_value();
                            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                            let out_elem_ptr = unsafe {
                                self.builder.build_gep(i64_ty, out_i64, &[wi], "out_elem")
                            }.map_err(|e| format!("gep error: {}", e))?;
                            self.builder.build_store(out_elem_ptr, elem)
                                .map_err(|e| format!("store error: {}", e))?;
                            let next_wi = self.builder.build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
                                .map_err(|e| format!("add error: {}", e))?;
                            self.builder.build_store(write_idx, next_wi)
                                .map_err(|e| format!("store error: {}", e))?;
                            self.builder.build_unconditional_branch(next_bb)
                                .map_err(|e| format!("branch error: {}", e))?;
                            self.builder.position_at_end(next_bb);
                        }
                        // idx++
                        let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next)
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(done_bb);
                        // Store result list: len and data ptr
                        let out_len = if is_map {
                            list_len
                        } else {
                            self.builder.build_load(BasicTypeEnum::IntType(i64_ty), write_idx, "out_len")
                                .map_err(|e| format!("load error: {}", e))?
                        };
                        let out_len_gep = self.builder.build_struct_gep(result_ty, result_alloca, 0, "out_len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        self.builder.build_store(out_len_gep, out_len)
                            .map_err(|e| format!("store error: {}", e))?;
                        let out_data_gep = self.builder.build_struct_gep(result_ty, result_alloca, 1, "out_data")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let out_void = self.builder.build_pointer_cast(out_i64, i8_ptr, "out_void")
                            .map_err(|e| format!("bitcast error: {}", e))?;
                        self.builder.build_store(out_data_gep, out_void)
                            .map_err(|e| format!("store error: {}", e))?;
                        Ok(result_alloca.into())
                    }
                    "reduce" if args.len() == 3 => {
                        // reduce(list, fn, init) - function reference version
                        let list_val = self.compile_expr(&args[0], vars)?;
                        let list_ptr = match list_val {
                            BasicValueEnum::PointerValue(pv) => pv,
                            _ => return Err("reduce: first arg must be a list".into()),
                        };
                        let fn_name = match &args[1] {
                            Expr::Ident(n) => n.clone(),
                            _ => return Err("reduce: second arg must be a function name".into()),
                        };
                        let init_val = self.compile_expr(&args[2], vars)?;
                        let fn_llvm = self.module.get_function(&fn_name)
                            .ok_or_else(|| format!("reduce: function '{}' not compiled", fn_name))?;
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let i64_ty = self.context.i64_type();
                        let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
                            BasicTypeEnum::IntType(i64_ty),
                            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        ], false));
                        let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "len")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
                            .map_err(|e| format!("load error: {}", e))?;
                        let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "data")
                            .map_err(|e| format!("gep error: {}", e))?;
                        let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")
                            .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                        let data_ptr = self.builder.build_bit_cast(data_i8,
                            i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                            .map_err(|e| format!("bitcast error: {}", e))?
                            .into_pointer_value();
                        let acc_alloca = self.builder.build_alloca(i64_ty, "acc")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(acc_alloca, init_val)
                            .map_err(|e| format!("store error: {}", e))?;
                        let function = self.current_function().ok_or_else(|| "codegen: no current function for reduce loop".to_string())?;
                        let loop_bb = self.context.append_basic_block(function, "reduce_loop");
                        let body_bb = self.context.append_basic_block(function, "reduce_body");
                        let done_bb = self.context.append_basic_block(function, "reduce_done");
                        let idx_alloca = self.builder.build_alloca(i64_ty, "ri")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(loop_bb);
                        let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
                            .map_err(|e| format!("load error: {}", e))?.into_int_value();
                        let loop_cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len.into_int_value(), "cmp")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        self.builder.build_conditional_branch(loop_cmp, body_bb, done_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(body_bb);
                        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                        let elem_ptr = unsafe {
                            self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
                        }.map_err(|e| format!("gep error: {}", e))?;
                        let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                            .map_err(|e| format!("load error: {}", e))?;
                        let acc = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), acc_alloca, "acc")
                            .map_err(|e| format!("load error: {}", e))?;
                        let fn_result = self.builder.build_call(fn_llvm, &[
                            BasicMetadataValueEnum::IntValue(acc.into_int_value()),
                            BasicMetadataValueEnum::IntValue(elem.into_int_value()),
                        ], "reduce_call")
                            .map_err(|e| format!("call error: {}", e))?
                            .try_as_basic_value().left()
                            .ok_or("function returned void")?;
                        self.builder.build_store(acc_alloca, fn_result)
                            .map_err(|e| format!("store error: {}", e))?;
                        let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
                            .map_err(|e| format!("add error: {}", e))?;
                        self.builder.build_store(idx_alloca, next)
                            .map_err(|e| format!("store error: {}", e))?;
                        self.builder.build_unconditional_branch(loop_bb)
                            .map_err(|e| format!("branch error: {}", e))?;
                        self.builder.position_at_end(done_bb);
                        let result = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), acc_alloca, "result")
                            .map_err(|e| format!("load error: {}", e))?;
                        Ok(result)
                    }
                    _ => {
                        // Check if this is a closure variable call
                        if let Some(&(alloca, ty)) = vars.get(name.as_str()) {
                            if let BasicTypeEnum::StructType(st) = ty {
                                if st.get_field_types().len() == 2 {
                                    // Closure struct {fn_ptr, env_ptr} — do indirect call
                                    let closure_val = self.builder.build_load(
                                        BasicTypeEnum::StructType(st), alloca,
                                        &format!("{}_closure", name),
                                    ).map_err(|e| format!("load closure error: {}", e))?;
                                    let closure_struct = closure_val.into_struct_value();
                                    let fn_ptr = self.builder.build_extract_value(closure_struct, 0, "fn_ptr")
                                        .map_err(|e| format!("extract fn_ptr error: {}", e))?
                                        .into_pointer_value();
                                    let env_ptr = self.builder.build_extract_value(closure_struct, 1, "env_ptr")
                                        .map_err(|e| format!("extract env_ptr error: {}", e))?
                                        .into_pointer_value();
                                    let mut compiled_args = Vec::new();
                                    for arg in args {
                                        compiled_args.push(self.compile_expr(arg, vars)?);
                                    }
                                    let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                                    let env_meta = BasicMetadataTypeEnum::PointerType(i8_ptr);
                                    let mut all_meta = vec![env_meta];
                                    for arg in &compiled_args {
                                        all_meta.push(match arg {
                                            BasicValueEnum::IntValue(iv) => BasicMetadataTypeEnum::IntType(iv.get_type()),
                                            BasicValueEnum::FloatValue(fv) => BasicMetadataTypeEnum::FloatType(fv.get_type()),
                                            BasicValueEnum::PointerValue(pv) => BasicMetadataTypeEnum::PointerType(pv.get_type()),
                                            BasicValueEnum::StructValue(sv) => BasicMetadataTypeEnum::StructType(sv.get_type()),
                                            BasicValueEnum::ArrayValue(av) => BasicMetadataTypeEnum::ArrayType(av.get_type()),
                                            BasicValueEnum::VectorValue(vv) => BasicMetadataTypeEnum::VectorType(vv.get_type()),
                                        });
                                    }
                                    let ret_type = self.context.i64_type();
                                    let indirect_fn_type = ret_type.fn_type(&all_meta, false);
                                    let fn_ptr_typed = self.builder.build_pointer_cast(
                                        fn_ptr,
                                        indirect_fn_type.ptr_type(inkwell::AddressSpace::default()),
                                        "fn_typed",
                                    ).map_err(|e| format!("pointer cast error: {}", e))?;
                                    let mut call_args = vec![BasicMetadataValueEnum::PointerValue(env_ptr)];
                                    for arg in &compiled_args {
                                        call_args.push(match arg {
                                            BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                            BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                            BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                            BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                            BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                            BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                        });
                                    }
                                    let call = self.builder.build_indirect_call(
                                        indirect_fn_type, fn_ptr_typed, &call_args, "closure_call",
                                    ).map_err(|e| format!("closure call error: {}", e))?;
                                    return Ok(call.try_as_basic_value().left().unwrap_or(
                                        self.context.i64_type().const_int(0, false).into()
                                    ));
                                }
                            }
                        }
                        self.compile_call(name, args, vars)
                    }
                }
            }
            Expr::Field(obj, method_name) => {
                // Method call: obj.method(args)
                // Determine the type of the object to find the actor/trait name
                let obj_type = self.infer_object_type(obj, vars);
                let actor_method = format!("{}__{}__method", obj_type, method_name);
                
                // 1. Try actor method dispatch
                if let Some(function) = self.module.get_function(&actor_method) {
                    let mut obj_val = self.compile_expr(obj, vars)?;
                    // Actor methods take self as pointer; convert struct value to pointer if needed
                    if let BasicValueEnum::StructValue(sv) = obj_val {
                        let struct_ty = sv.get_type();
                        let alloca = self.builder.build_alloca(struct_ty, "self_tmp")
                            .map_err(|e| format!("alloca error: {}", e))?;
                        self.builder.build_store(alloca, obj_val)
                            .map_err(|e| format!("store error: {}", e))?;
                        obj_val = alloca.into();
                    }
                    let mut compiled_args = Vec::new();
                    compiled_args.push(obj_val);
                    for arg in args {
                        compiled_args.push(self.compile_expr(arg, vars)?);
                    }
                    let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                        BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                        BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                        BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                        BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                        BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                        BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                    }).collect();
                    let call = self.builder.build_call(function, &metadata_args, "method_call")
                        .map_err(|e| format!("method call error: {}", e))?;
                    return Ok(call.try_as_basic_value().left().unwrap_or(
                        self.context.i64_type().const_int(0, false).into()
                    ));
                }

                // 1.5. Special case: Type.spawn() constructor call for actors
                if method_name == "spawn" {
                    let spawn_name = format!("{}_spawn", obj_type);
                    if let Some(spawn_fn) = self.module.get_function(&spawn_name) {
                        let call = self.builder.build_call(spawn_fn, &[], "actor_spawn")
                            .map_err(|e| format!("spawn call error: {}", e))?;
                        return Ok(call.try_as_basic_value().left().unwrap_or(
                            self.context.i64_type().const_int(0, false).into()
                        ));
                    }
                }

                // 2. Try trait method dispatch: type_impls[type_name][trait_name][method_name]
                if let Some(trait_impls) = self.type_impls.get(&obj_type) {
                    for (trait_name, methods) in trait_impls {
                        if methods.iter().any(|m| m.name == *method_name) {
                            let mangled = format!("{}__{}__{}", obj_type, trait_name, method_name);
                            if let Some(function) = self.module.get_function(&mangled) {
                                let obj_val = self.compile_expr(obj, vars)?;
                                let mut compiled_args = Vec::new();
                                compiled_args.push(obj_val);
                                for arg in args {
                                    compiled_args.push(self.compile_expr(arg, vars)?);
                                }
                                let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                                    BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                    BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                    BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                    BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                    BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                    BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                }).collect();
                                let call = self.builder.build_call(function, &metadata_args, "trait_call")
                                    .map_err(|e| format!("trait method call error: {}", e))?;
                                return Ok(call.try_as_basic_value().left().unwrap_or(
                                    self.context.i64_type().const_int(0, false).into()
                                ));
                            }
                        }
                    }
                }
                // 3. True vtable indirect dispatch for dyn Trait objects
                if obj_type.starts_with("dyn ") {
                    let trait_name = obj_type.strip_prefix("dyn ").unwrap_or("");
                    if !trait_name.is_empty() && !trait_name.contains(' ') {
                        // Find method index within the trait definition
                        let method_idx = self.trait_defs.get(trait_name)
                            .and_then(|tdef| tdef.methods.iter().position(|m| m.name == *method_name));
                        if let Some(idx) = method_idx {
                            // Get the vtable struct type (clone to avoid borrow conflict)
                            let vtable_ty = self.vtable_types.get(trait_name)
                                .map(|s| *s).ok_or("no vtable type for trait")?;
                            // Fat pointer layout: { i8* data, i8* vtable }
                            let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                            let fat_ty = self.context.struct_type(&[
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                                BasicTypeEnum::PointerType(i8_ptr_ty),
                            ], false);
                            // The obj_val is a fat pointer struct { data: i8*, vtable: i8* }
                            let obj_val = self.compile_expr(obj, vars)?;
                            let fat_ptr = match obj_val {
                                    BasicValueEnum::StructValue(_) => {
                                        // Alloca the struct value so we can GEP into it
                                        let alloca = self.builder.build_alloca(
                                            BasicTypeEnum::StructType(fat_ty), "fat_tmp"
                                        ).map_err(|e| format!("alloca error: {}", e))?;
                                        self.builder.build_store(alloca, obj_val)
                                            .map_err(|e| format!("store error: {}", e))?;
                                        alloca
                                    }
                                    BasicValueEnum::PointerValue(pv) => pv,
                                    _ => return Err("dyn Trait value must be a struct or pointer".into()),
                                };
                                // Extract vtable pointer (field 1)
                                let vtable_gep = self.builder.build_struct_gep(
                                    BasicTypeEnum::StructType(fat_ty), fat_ptr, 1, "vtable_gep"
                                ).map_err(|e| format!("gep error: {}", e))?;
                                let vtable_ptr = self.builder.build_load(
                                    BasicTypeEnum::PointerType(i8_ptr_ty), vtable_gep, "vtable_ptr"
                                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                                // GEP into vtable at method index
                                let method_gep = self.builder.build_struct_gep(
                                    BasicTypeEnum::StructType(vtable_ty), vtable_ptr, idx as u32, "method_gep"
                                ).map_err(|e| format!("gep error: {}", e))?;
                                // Load function pointer from vtable slot
                                let fn_ptr = self.builder.build_load(
                                    BasicTypeEnum::PointerType(i8_ptr_ty), method_gep, "fn_ptr"
                                ).map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                                // Extract data pointer (field 0) for passing as self arg
                                let data_gep = self.builder.build_struct_gep(
                                    BasicTypeEnum::StructType(fat_ty), fat_ptr, 0, "data_gep"
                                ).map_err(|e| format!("gep error: {}", e))?;
                                let data_ptr = self.builder.build_load(
                                    BasicTypeEnum::PointerType(i8_ptr_ty), data_gep, "data_ptr"
                                ).map_err(|e| format!("load error: {}", e))?;
                                // Get the mangled function's type for the indirect call signature
                                // Find any matching mangled function to extract fn type
                                let fn_sig = (|| -> Option<(inkwell::values::AnyValueEnum<'ctx>, String)> {
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
                                })();
                                if let Some((fn_val, _)) = fn_sig {
                                    let fn_llvm = fn_val.into_function_value();
                                    let fn_type = fn_llvm.get_type();
                                    // Cast fn_ptr i8* to the right function pointer type
                                    let fn_ptr_cast = self.builder.build_pointer_cast(
                                        fn_ptr,
                                        fn_type.ptr_type(inkwell::AddressSpace::default()),
                                        "fn_cast"
                                    ).map_err(|e| format!("cast error: {}", e))?;
                                    // Compile additional args (start with data ptr as self)
                                    let mut compiled_args = Vec::new();
                                    compiled_args.push(data_ptr);
                                    for arg in args {
                                        compiled_args.push(self.compile_expr(arg, vars)?);
                                    }
                                    let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                                        BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                        BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                        BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                        BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                        BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                        BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                    }).collect();
                                    let call = self.builder.build_indirect_call(
                                        fn_type, fn_ptr_cast, &metadata_args, "dyn_call"
                                    ).map_err(|e| format!("dyn indirect call error: {}", e))?;
                                    return Ok(call.try_as_basic_value().left().unwrap_or(
                                        self.context.i64_type().const_int(0, false).into()
                                    ));
                                }
                        }
                    }
                    return Err(format!("[E0708] cannot dispatch method '{}' on {}", method_name, obj_type));
                }

                // 3b. Try impl Trait dispatch (same logic as dyn Trait)
                if obj_type.starts_with("impl ") {
                    let trait_name = obj_type.strip_prefix("impl ").unwrap_or("");
                    if !trait_name.is_empty() && !trait_name.contains(' ') {
                        for (type_name, trait_impls) in &self.type_impls {
                            if let Some(methods) = trait_impls.get(trait_name) {
                                if methods.iter().any(|m| m.name == *method_name) {
                                    let mangled = format!("{}__{}__{}", type_name, trait_name, method_name);
                                    if let Some(function) = self.module.get_function(&mangled) {
                                        let obj_val = self.compile_expr(obj, vars)?;
                                        let mut compiled_args = Vec::new();
                                        compiled_args.push(obj_val);
                                        for arg in args {
                                            compiled_args.push(self.compile_expr(arg, vars)?);
                                        }
                                        let metadata_args: Vec<_> = compiled_args.iter().map(|v| match v {
                                            BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                            BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                            BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                            BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                            BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                            BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                        }).collect();
                                        let call = self.builder.build_call(function, &metadata_args, "impl_trait_call")
                                            .map_err(|e| format!("impl trait call error: {}", e))?;
                                        return Ok(call.try_as_basic_value().left().unwrap_or(
                                            self.context.i64_type().const_int(0, false).into()
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    return Err(format!("[E0708] cannot dispatch method '{}' on {}", method_name, obj_type));
                }

                // 4. Fallback: field access or error
                if self.type_defs.contains_key(&obj_type) {
                    Err(format!("method '{}' not compiled for type '{}' (missing crate?)", method_name, obj_type))
                } else {
                    Err(format!("cannot call method '{}' on unknown type '{}'", method_name, obj_type))
                }
            }
            _ => Err("only direct function calls and method calls supported in codegen".to_string()),
        }
    }

    fn compile_turbofish_expr(
        &mut self,
        name: &str,
        type_args: &[Type],
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Monomorphized call: func::<Type>(args)
        // Build type_map from explicit type args
        let func = self.find_func_def(name)?;
        if func.generics.len() != type_args.len() {
            return Err(format!("[E0720] turbofish for '{}' expects {} type args, got {}", name, func.generics.len(), type_args.len()));
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
            self.compile_generic_func(&func, &merged_map).map_err(|e| e.to_string())?;
        }
        // Call the mangled function
        self.compile_call_mangled(&mangled, args, vars)
    }
    fn compile_match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let scrutinee_val = self.compile_expr(scrutinee, vars)?;
        let scrutinee_iv = if let BasicValueEnum::IntValue(iv) = scrutinee_val {
            iv
        } else {
            return Err("match scrutinee must be integer (enum tag)".into());
        };

        let function = self.current_function().ok_or_else(|| "codegen: no current function for match".to_string())?;
        let merge_bb = self.context.append_basic_block(function, "matchcont");
        let mut else_bb = self.context.append_basic_block(function, "matchelse");

        // Branch from current block to the dispatch (matchelse)
        self.builder.build_unconditional_branch(else_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(else_bb);

        let mut incoming_vals = Vec::new();
        let mut incoming_bbs = Vec::new();

        // Build if-else chain for each arm
        for (i, arm) in arms.iter().enumerate() {
            let arm_bb = self.context.append_basic_block(function, &format!("arm{}", i));

            match &arm.pat {
                Pattern::Wildcard | Pattern::Variable(_) => {
                    // Always matches - jump to arm body
                    self.builder.position_at_end(else_bb);
                    self.builder.build_unconditional_branch(arm_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    // Create a fresh else_bb so the after-loop code doesn't
                    // double-terminate the block we just wrote to.
                    else_bb = self.context.append_basic_block(function, &format!("wccont{}", i));
                }
                Pattern::Literal(lit) => {
                    self.builder.position_at_end(else_bb);
                    let lit_val = match lit {
                        Lit::Int(n) => self.context.i64_type().const_int(*n as u64, true),
                        Lit::Bool(b) => self.context.bool_type().const_int(*b as u64, false),
                        Lit::Unit => self.context.i64_type().const_int(0, false),
                        _ => return Err("[E0709] unsupported match literal type".into()),
                    };
                    let cmp = self.builder.build_int_compare(
                        inkwell::IntPredicate::EQ,
                        scrutinee_iv,
                        lit_val,
                        "cmp",
                    ).map_err(|e| format!("cmp error: {}", e))?;
                    // Always create an intermediate next block so the else chain
                    // never points directly at merge_bb.  This keeps the phi's
                    // predecessor set clean and avoids corrupting merge_bb.
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    else_bb = next_bb;
                }
                Pattern::Constructor(name, _) => {
                    // Constructor pattern: compare tag using ordinal index
                    self.builder.position_at_end(else_bb);
                    // Look up the variant ordinal index from type definitions
                    let ordinal = self.find_variant_ordinal(name);
                    let tag_val = self.context.i64_type().const_int(ordinal, false);
                    let cmp = self.builder.build_int_compare(
                        inkwell::IntPredicate::EQ,
                        scrutinee_iv,
                        tag_val,
                        "cmp",
                    ).map_err(|e| format!("cmp error: {}", e))?;
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    else_bb = next_bb;
                }
                Pattern::Tuple(_inner_pats) => {
                    // Tuple pattern: match each element of the tuple struct
                    // Treat as always-matching for now (full element-wise comparison is complex)
                    // but bind inner variables by loading from the struct
                    self.builder.position_at_end(else_bb);
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    self.builder.build_unconditional_branch(arm_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    else_bb = next_bb;
                }
                Pattern::Array(_inner_pats) => {
                    // Array pattern: match each element of the list
                    // Treat as always-matching for now, bind inner variables
                    self.builder.position_at_end(else_bb);
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    self.builder.build_unconditional_branch(arm_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    else_bb = next_bb;
                }
                Pattern::Slice(_inner_pats, _rest) => {
                    // Slice pattern: match prefix elements, bind rest
                    self.builder.position_at_end(else_bb);
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    self.builder.build_unconditional_branch(arm_bb)
                        .map_err(|e| format!("branch error: {}", e))?;
                    else_bb = next_bb;
                }
            }

            // Arm body
            self.builder.position_at_end(arm_bb);
            let mut local_vars = vars.clone();
            // Bind variables from pattern
            match &arm.pat {
                Pattern::Variable(name) => {
                    let alloca = self.builder.build_alloca(
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        name,
                    ).map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, scrutinee_iv)
                        .map_err(|e| format!("store error: {}", e))?;
                    local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                }
                Pattern::Constructor(_, inner_patterns) => {
                    // For constructor patterns, bind inner variables
                    // For now, assume single inner variable
                    for inner_pat in inner_patterns {
                        if let Pattern::Variable(name) = inner_pat {
                            let alloca = self.builder.build_alloca(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                name,
                            ).map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, scrutinee_iv)
                                .map_err(|e| format!("store error: {}", e))?;
                            local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                        }
                    }
                }
                Pattern::Tuple(inner_pats) => {
                    // For tuple patterns, bind inner variables by loading from struct
                    let scrutinee_ptr = match scrutinee_val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        _ => continue,
                    };
                    // Determine tuple element types from the struct
                    let _elem_count = inner_pats.len();
                    for (j, inner_pat) in inner_pats.iter().enumerate() {
                        if let Pattern::Variable(name) = inner_pat {
                            let gep = self.builder.build_struct_gep(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                scrutinee_ptr,
                                j as u32,
                                &format!("tuple_{}", j),
                            ).map_err(|e| format!("gep error: {}", e))?;
                            let val = self.builder.build_load(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                gep,
                                &format!("tup_{}", j),
                            ).map_err(|e| format!("load error: {}", e))?;
                            let alloca = self.builder.build_alloca(
                                BasicTypeEnum::IntType(self.context.i64_type()),
                                name,
                            ).map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, val)
                                .map_err(|e| format!("store error: {}", e))?;
                            local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(self.context.i64_type())));
                        }
                    }
                }
                Pattern::Array(inner_pats) => {
                    // For array patterns, bind inner variables by loading from list data
                    let scrutinee_ptr = match scrutinee_val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        _ => continue,
                    };
                    // Load data pointer from list struct
                    let list_ty = self.context.struct_type(&[
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    ], false);
                    let data_gep = self.builder.build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
                        .map_err(|e| format!("gep error: {}", e))?;
                    let data_i8 = self.builder.build_load(
                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        data_gep, "data").map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                    let i64_ty = self.context.i64_type();
                    let data_ptr = self.builder.build_bit_cast(data_i8,
                        i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                        .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
                    for (j, inner_pat) in inner_pats.iter().enumerate() {
                        if let Pattern::Variable(name) = inner_pat {
                            let idx = i64_ty.const_int(j as u64, false);
                            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                            let elem_ptr = unsafe {
                                self.builder.build_gep(i64_ty, data_ptr, &[idx], &format!("arr_{}", j))
                            }.map_err(|e| format!("gep error: {}", e))?;
                            let val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, &format!("arrv_{}", j))
                                .map_err(|e| format!("load error: {}", e))?;
                            let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                                .map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, val)
                                .map_err(|e| format!("store error: {}", e))?;
                            local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                        }
                    }
                }
                Pattern::Slice(inner_pats, rest) => {
                    // For slice patterns, bind prefix variables and rest as list
                    let scrutinee_ptr = match scrutinee_val {
                        BasicValueEnum::PointerValue(pv) => pv,
                        _ => continue,
                    };
                    let list_ty = self.context.struct_type(&[
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    ], false);
                    let data_gep = self.builder.build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
                        .map_err(|e| format!("gep error: {}", e))?;
                    let data_i8 = self.builder.build_load(
                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        data_gep, "data").map_err(|e| format!("load error: {}", e))?.into_pointer_value();
                    let i64_ty = self.context.i64_type();
                    let data_ptr = self.builder.build_bit_cast(data_i8,
                        i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                        .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
                    // Bind prefix elements
                    for (j, inner_pat) in inner_pats.iter().enumerate() {
                        if let Pattern::Variable(name) = inner_pat {
                            let idx = i64_ty.const_int(j as u64, false);
                            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                            let elem_ptr = unsafe {
                                self.builder.build_gep(i64_ty, data_ptr, &[idx], &format!("slc_{}", j))
                            }.map_err(|e| format!("gep error: {}", e))?;
                            let val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, &format!("slcv_{}", j))
                                .map_err(|e| format!("load error: {}", e))?;
                            let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                                .map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, val)
                                .map_err(|e| format!("store error: {}", e))?;
                            local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                        }
                    }
                    // Bind rest as remaining list (simplified: bind as empty list)
                    if let Some(rest_pat) = rest.as_ref() {
                        if let Pattern::Variable(name) = rest_pat.as_ref() {
                            let i64_ty = self.context.i64_type();
                            let empty_list: BasicValueEnum = i64_ty.const_int(0, false).into();
                            let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                                .map_err(|e| format!("alloca error: {}", e))?;
                            self.builder.build_store(alloca, empty_list)
                                .map_err(|e| format!("store error: {}", e))?;
                            local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                        }
                    }
                }
                Pattern::Wildcard | Pattern::Literal(_) => {
                    // Wildcard and literal patterns: no variable binding needed
                }
            }
            let arm_val = self.compile_expr(&arm.body, &local_vars)?;
            incoming_vals.push(arm_val);
            incoming_bbs.push(arm_bb);
            self.builder.build_unconditional_branch(merge_bb)
                .map_err(|e| format!("branch error: {}", e))?;
        }

        // Unreachable else block (should not be reached if match is exhaustive).
        // else_bb is a fresh next_N block (never merge_bb) thanks to the
        // unconditional intermediate-block creation above.
        self.builder.position_at_end(else_bb);
        self.builder.build_unconditional_branch(merge_bb)
            .map_err(|e| format!("branch error: {}", e))?;

        // Merge block - use phi to select the right value
        self.builder.position_at_end(merge_bb);
        if incoming_vals.is_empty() {
            return Err("empty match expression".into());
        }
        let ty = incoming_vals[0].get_type();
        let phi = self.builder.build_phi(ty, "match.result")
            .map_err(|e| format!("phi error: {}", e))?;
        let mut phi_incoming: Vec<_> = incoming_vals.iter().zip(incoming_bbs.iter())
            .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
            .collect();
        // Add the unreachable else block with a dummy value so every
        // predecessor of merge_bb has a phi entry.
        let dummy_val = self.context.i64_type().const_int(0, false);
        phi_incoming.push((&dummy_val as &dyn inkwell::values::BasicValue, else_bb));
        phi.add_incoming(&phi_incoming);
        Ok(phi.as_basic_value())
    }

    fn compile_record_expr(
        &mut self,
        ty: &Option<String>,
        fields: &[RecordFieldExpr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Create a record value
        let type_name = ty.as_deref().unwrap_or("unknown");
        let llvm_ty = *self.type_llvm.get(type_name)
            .ok_or_else(|| format!("unknown type '{}'", type_name))?;
        if let BasicTypeEnum::StructType(sty) = llvm_ty {
            let alloca = self.builder.build_alloca(sty, type_name)
                .map_err(|e| format!("alloca error: {}", e))?;
            // Store field values
            for (i, field) in fields.iter().enumerate() {
                let val = self.compile_expr(&field.value, vars)?;
                let gep = self.builder.build_struct_gep(sty, alloca, i as u32, &field.name)
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(gep, val)
                    .map_err(|e| format!("store error: {}", e))?;
            }
            Ok(alloca.into())
        } else {
            Err(format!("type '{}' is not a struct", type_name))
        }
    }
    fn compile_field_expr(
        &mut self,
        obj: &Expr,
        field_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Field access: obj.field
        let obj_val = self.compile_expr(obj, vars)?;
        let obj_type = self.infer_object_type(obj, vars);
        let field_ptr = match obj_val {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::StructValue(sv) => {
                if let Some(BasicTypeEnum::StructType(sty)) = self.type_llvm.get(&obj_type) {
                    let alloca = self.builder.build_alloca(*sty, "tmp")
                        .map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, sv)
                        .map_err(|e| format!("store error: {}", e))?;
                    alloca
                } else {
                    return Err(format!("[E0707] cannot access field on type '{}'", obj_type));
                }
            }
            _ => return Err(format!("field access requires a struct or actor type, got {}", obj_val.get_type())),
        };
        let sty = match self.type_llvm.get(&obj_type) {
            Some(BasicTypeEnum::StructType(s)) => *s,
            _ => return Err(format!("type '{}' is not a struct", obj_type)),
        };
        if let Some(td) = self.type_defs.get(&obj_type) {
            if let TypeDefKind::Record(fields) = &td.kind {
                if let Some(idx) = fields.iter().position(|f| f.name == *field_name) {
                    let gep = self.builder.build_struct_gep(sty, field_ptr, idx as u32, field_name)
                        .map_err(|e| format!("gep error: {}", e))?;
                    let field_ty = types::mimi_type_to_llvm(self.context, &fields[idx].ty)
                        .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                    return self.builder.build_load(field_ty, gep, field_name)
                        .map_err(|e| format!("load error: {}", e));
                }
            }
        }
        // Fallback: numeric field index
        if let Ok(idx) = field_name.parse::<u32>() {
            let gep = self.builder.build_struct_gep(sty, field_ptr, idx, field_name)
                .map_err(|e| format!("gep error: {}", e))?;
            return self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), gep, field_name)
                .map_err(|e| format!("load error: {}", e));
        }
        Err(format!("field '{}' not found on type '{}'", field_name, obj_type))
    }

    fn compile_list_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Create a list struct: { i64 len, i64* data }
        let count = elems.len() as u64;
        let len_val = self.context.i64_type().const_int(count, false);
        // Allocate array
        let sizeof_i64 = self.context.i64_type().const_int(8, false);
        let alloc_size = self.builder.build_int_mul(len_val, sizeof_i64, "alloc_size")
            .map_err(|e| format!("mul error: {}", e))?;
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let data_ptr = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(alloc_size),
        ], "malloc_call")
            .map_err(|e| format!("malloc error: {}", e))?
            .try_as_basic_value().left()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        self.register_heap_alloc(data_ptr);
        let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
            self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
            "data_ptr_i64")
            .map_err(|e| format!("bitcast error: {}", e))?
            .into_pointer_value();
        // Store each element (universal i64 representation)
        for (i, elem) in elems.iter().enumerate() {
            let val = self.compile_expr(elem, vars)?;
            let iv = match val {
                BasicValueEnum::IntValue(iv) => iv,
                BasicValueEnum::FloatValue(fv) => {
                    self.builder.build_bit_cast(fv, self.context.i64_type(), "f64_to_i64")
                        .map_err(|e| format!("bitcast error: {}", e))?
                        .into_int_value()
                }
                BasicValueEnum::PointerValue(pv) => {
                    self.builder.build_ptr_to_int(pv, self.context.i64_type(), "ptr_to_i64")
                        .map_err(|e| format!("ptr_to_int error: {}", e))?
                }
                _ => return Err("list elements must be scalar types (int, float, pointer) for now".into()),
            };
            let idx = self.context.i64_type().const_int(i as u64, false);
            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
            let elem_ptr = unsafe {
                self.builder.build_gep(self.context.i64_type(), data_ptr_i64, &[idx], "elem")
            }.map_err(|e| format!("gep error: {}", e))?;
            self.builder.build_store(elem_ptr, iv)
                .map_err(|e| format!("store error: {}", e))?;
        }
        // Create list struct
        let list_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(self.context.i64_type()),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let list_alloca = self.builder.build_alloca(list_ty, "list")
            .map_err(|e| format!("alloca error: {}", e))?;
        let len_gep = self.builder.build_struct_gep(list_ty, list_alloca, 0, "list_len")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(len_gep, len_val)
            .map_err(|e| format!("store error: {}", e))?;
        let data_gep = self.builder.build_struct_gep(list_ty, list_alloca, 1, "list_data")
            .map_err(|e| format!("gep error: {}", e))?;
        let data_void_ptr = self.builder.build_bit_cast(data_ptr,
            self.context.ptr_type(inkwell::AddressSpace::default()), "data_void")
            .map_err(|e| format!("bitcast error: {}", e))?;
        self.builder.build_store(data_gep, data_void_ptr)
            .map_err(|e| format!("store error: {}", e))?;
        Ok(list_alloca.into())
    }

    fn compile_index_expr(
        &mut self,
        obj: &Expr,
        idx_expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // list[i] or arr[i] - load from array/list
        let obj_val = self.compile_expr(obj, vars)?;
        let idx_val = self.compile_expr(idx_expr, vars)?;
        match obj_val {
            BasicValueEnum::PointerValue(pv) => {
                let idx_iv = match idx_val {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] index must be i64".into()),
                };
                // Try list struct first: { i64 len, i8* data }
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                // Check if this looks like a list struct by trying to GEP field 0 (len)
                if let Ok(_len_gep) = self.builder.build_struct_gep(list_ty, pv, 0, "list.len_check") {
                    // It's a list struct - load data pointer and index into it
                    let data_gep = self.builder.build_struct_gep(list_ty, pv, 1, "list.data")
                        .map_err(|e| format!("gep error: {}", e))?;
                    let data_ptr = self.builder.build_load(
                        BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                        data_gep, "data")
                        .map_err(|e| format!("load error: {}", e))?
                        .into_pointer_value();
                    let data_ptr_i64 = self.builder.build_bit_cast(data_ptr,
                        self.context.i64_type().ptr_type(inkwell::AddressSpace::default()),
                        "data_i64")
                        .map_err(|e| format!("bitcast error: {}", e))?
                        .into_pointer_value();
                    // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                    let elem_ptr = unsafe {
                        self.builder.build_gep(self.context.i64_type(), data_ptr_i64, &[idx_iv], "elem")
                    }.map_err(|e| format!("gep error: {}", e))?;
                    return self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                        .map_err(|e| format!("load error: {}", e));
                }
                // Fallback: treat as raw pointer to i64 array
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let elem_ptr = unsafe {
                    self.builder.build_gep(self.context.i64_type(), pv, &[idx_iv], "elem")
                }.map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), elem_ptr, "elem_val")
                    .map_err(|e| format!("load error: {}", e))
            }
            BasicValueEnum::ArrayValue(_av) => {
                // Direct LLVM array value: extract element by index
                let idx = match idx_val {
                    BasicValueEnum::IntValue(iv) => {
                        // Convert runtime i64 index to constant u32 for extractvalue
                        iv.get_zero_extended_constant()
                            .ok_or_else(|| "[E0712] array index must be a compile-time constant".to_string())? as u32
                    }
                    _ => return Err("[E0712] index must be i64".into()),
                };
                let elem = self.builder.build_extract_value(obj_val.into_array_value(), idx, "arr_elem")
                    .map_err(|e| format!("extractvalue error: {}", e))?;
                Ok(elem)
            }
            _ => Err("index requires a list/array pointer".into()),
        }
    }

    fn compile_spawn_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Spawn: create a thread to execute the expression
        let parent_fn = self.current_function().ok_or_else(|| "codegen: no current function for spawn".to_string())?;
        let parent_name = parent_fn.get_name().to_str().unwrap_or("unknown").to_string();
        let wrapper_name = format!("{}{}__spawn_wrapper", parent_name, self.spawn_counter).to_string();
        self.spawn_counter += 1;
        
        // Create wrapper function: i8* wrapper(i8*)
        let i8_ty = self.context.i8_type();
        let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
        let wrapper_fn_type = i8_ptr.fn_type(
            &[BasicMetadataTypeEnum::PointerType(i8_ptr)], false
        );
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_type, None);
        let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
        
        // Save current builder position and compile the spawn body into the wrapper
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(wrapper_entry);
        
        // Compile the spawn expression (the result is the return value)
        let result = self.compile_expr(expr, vars)?;
        
        // Allocate heap space for the return value using malloc (not alloca — 
        // heap memory survives the wrapper function's return)
        let i64_ty = self.context.i64_type();
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let result_llvm_ty_for_size = result.get_type();
        let byte_size_val = result_llvm_ty_for_size.size_of()
            .and_then(|v: inkwell::values::IntValue<'ctx>| v.get_zero_extended_constant())
            .unwrap_or(0) as u64;
        let byte_size = i64_ty.const_int(byte_size_val, false);
        let result_storage = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(byte_size),
        ], "malloc_result")
            .map_err(|e| format!("malloc error: {}", e))?
            .try_as_basic_value()
            .left()
            .ok_or("malloc returned void")?;
        let result_storage_ptr = if let BasicValueEnum::PointerValue(pv) = result_storage {
            pv
        } else {
            return Err("malloc should return a pointer".into());
        };
        // Store the result
                    // Cast result_storage (i8*) to the correct type pointer for storing
        let result_llvm_ty = result.get_type();
        let result_ptr_ty = match result_llvm_ty {
            BasicTypeEnum::IntType(t) => t.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::FloatType(t) => t.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::PointerType(t) => t.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::StructType(t) => t.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::ArrayType(t) => t.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::VectorType(t) => t.ptr_type(inkwell::AddressSpace::default()),
        };
        let result_typed_ptr = self.builder.build_pointer_cast(
            result_storage_ptr,
            result_ptr_ty,
            "result_typed"
        ).map_err(|e| format!("bitcast error: {}", e))?;
        self.builder.build_store(result_typed_ptr, result)
            .map_err(|e| format!("store error: {}", e))?;
        // Return the i8* pointer
        self.builder.build_return(Some(&result_storage))
            .map_err(|e| format!("return error: {}", e))?;
        
        // Restore builder position to original block
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }
        
        let wrapper_fn_ptr = self.builder.build_pointer_cast(
            wrapper_fn.as_global_value().as_pointer_value(),
            i8_ptr,
            "wrapper_i8"
        ).map_err(|e| format!("bitcast error: {}", e))?;

        if self.in_parasteps {
            // Parasteps: submit to thread pool (avoids creating N OS threads)
            self.pending_spawn_type = Some(result.get_type());
            let mimi_pool_submit_fn = self.module.get_function("mimi_pool_submit")
                .ok_or("mimi_pool_submit not declared")?;
            self.builder.build_call(mimi_pool_submit_fn, &[
                BasicMetadataValueEnum::PointerValue(wrapper_fn_ptr),
                BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
            ], "pool_submit_call")
                .map_err(|e| format!("pool_submit error: {}", e))?;
            // Return 0 as placeholder (parasteps joins all at block end)
            let placeholder = i64_ty.const_int(0, false);
            Ok(BasicValueEnum::IntValue(placeholder))
        } else {
            // Non-parasteps (single spawn+await): use raw pthread_create
            let thread_alloca = self.builder.build_alloca(i64_ty, "thread")
                .map_err(|e| format!("alloca error: {}", e))?;
            self.builder.build_store(thread_alloca, i64_ty.const_int(0, false))
                .map_err(|e| format!("store error: {}", e))?;

            let pthread_create_fn = self.module.get_function("pthread_create")
                .ok_or("pthread_create not declared")?;
            self.builder.build_call(pthread_create_fn, &[
                BasicMetadataValueEnum::PointerValue(thread_alloca),
                BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
                BasicMetadataValueEnum::PointerValue(wrapper_fn_ptr),
                BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
            ], "pthread_create_call")
                .map_err(|e| format!("pthread_create error: {}", e))?;

            let thread_id_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), thread_alloca, "thread_id")
                .map_err(|e| format!("load error: {}", e))?;
            Ok(thread_id_val)
        }
    }
    fn compile_await_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Await: join the thread and get the result
        let thread_val = self.compile_expr(expr, vars)?;
        let thread_id = match thread_val {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::PointerValue(pv) => {
                self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), pv, "thread")
                    .map_err(|e| format!("load error: {}", e))?.into_int_value()
            }
            _ => return Err("await requires a thread (i64) value".into()),
        };
        
        // Allocate space to receive the wrapper's return pointer (void**)
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let retval_storage = self.builder.build_alloca(i8_ptr, "retval_ptr")
            .map_err(|e| format!("alloca error: {}", e))?;
        self.builder.build_store(retval_storage, i8_ptr.const_null())
            .map_err(|e| format!("store error: {}", e))?;
        
        // Remove from parasteps tracking (already awaited, avoid double-join at block end)
        self.parasteps_thread_ids.retain(|&id| id != thread_id);
        
        let pthread_join_fn = self.module.get_function("pthread_join")
            .ok_or("pthread_join not declared")?;
        self.builder.build_call(pthread_join_fn, &[
            BasicMetadataValueEnum::IntValue(thread_id),
            BasicMetadataValueEnum::PointerValue(retval_storage),
        ], "pthread_join_call")
            .map_err(|e| format!("pthread_join error: {}", e))?;
        
        // Load the returned pointer from the storage (it's the wrapper's malloc'd result)
        let result_i8_ptr = self.builder.build_load(
            BasicTypeEnum::PointerType(i8_ptr),
            retval_storage,
            "result_ptr"
        ).map_err(|e| format!("load error: {}", e))?;
        let result_ptr = if let BasicValueEnum::PointerValue(pv) = result_i8_ptr {
            pv
        } else {
            return Err("expected pointer from pthread_join".into());
        };
        
        // Cast from i8* to result type pointer and load the result value
        let result_type = self.pending_spawn_type.take().unwrap_or_else(|| self.context.i64_type().into());
        let result_typed = self.builder.build_pointer_cast(
            result_ptr,
            result_type.ptr_type(inkwell::AddressSpace::default()),
            "result_typed_ptr"
        ).map_err(|e| format!("bitcast error: {}", e))?;
        let result_val = self.builder.build_load(
            result_type,
            result_typed,
            "spawn_result_val"
        ).map_err(|e| format!("load error: {}", e))?;
        
        // Free the malloc'd memory
        let free_fn = self.module.get_function("free")
            .ok_or_else(|| "free not declared".to_string())?;
        self.builder.build_call(free_fn, &[
            BasicMetadataValueEnum::PointerValue(result_ptr),
        ], "free_call")
            .map_err(|e| format!("free error: {}", e))?;
        
        Ok(result_val)
    }

    fn compile_try_expr(
        &mut self,
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // ? operator: compile inner expr as Result<T,E>{i1, T},
        // check discriminant, extract T on Ok, exit on Err
        let result_val = self.compile_expr(inner, vars)?;

        // The result should be a struct {i1, T}. Load it if it's a pointer.
        // Extract discriminant (field 0) via GEP+load if pointer, or extract_value if struct
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let function = self.current_function().ok_or_else(|| "codegen: no current function for try".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "try_ok");
        let err_bb = self.context.append_basic_block(function, "try_err");

        match result_val {
            BasicValueEnum::PointerValue(pv) => {
                // Access struct fields via GEP
                let result_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i1_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let gep0 = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_ty), pv, 0, "disc_gep"
                ).map_err(|e| format!("gep error: {}", e))?;
                let disc = self.builder.build_load(
                    BasicTypeEnum::IntType(i1_ty), gep0, "discriminant"
                ).map_err(|e| format!("load error: {}", e))?.into_int_value();
                let gep1 = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_ty), pv, 1, "pay_gep"
                ).map_err(|e| format!("gep error: {}", e))?;
                let payload = self.builder.build_load(
                    BasicTypeEnum::IntType(i64_ty), gep1, "payload"
                ).map_err(|e| format!("load error: {}", e))?;

                self.builder.build_conditional_branch(disc, ok_bb, err_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                // Err path: run compensations, print error message, exit(1)
                self.builder.position_at_end(err_bb);
                let mut comp_vars = vars.clone();
                self.compile_compensations(&mut comp_vars).map_err(|e| e.to_string())?;
                let try_exit_fn = self.module.get_function("mimi_try_exit")
                    .ok_or("mimi_try_exit not declared")?;
                self.builder.build_call(try_exit_fn, &[
                    BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                ], "try_exit")
                    .map_err(|e| format!("try_exit error: {}", e))?;
                let unreachable = self.context.append_basic_block(function, "unreachable");
                self.builder.build_unconditional_branch(unreachable)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(payload)
            }
            BasicValueEnum::StructValue(sv) => {
                // Extract via extract_value for struct values
                let disc = self.builder.build_extract_value(sv, 0, "discriminant")
                    .map_err(|e| format!("extract_value error: {}", e))?;
                let payload = self.builder.build_extract_value(sv, 1, "payload")
                    .map_err(|e| format!("extract_value error: {}", e))?;

                self.builder.build_conditional_branch(disc.into_int_value(), ok_bb, err_bb)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(err_bb);
                let mut comp_vars = vars.clone();
                self.compile_compensations(&mut comp_vars).map_err(|e| e.to_string())?;
                let try_exit_fn = self.module.get_function("mimi_try_exit")
                    .ok_or("mimi_try_exit not declared")?;
                self.builder.build_call(try_exit_fn, &[
                    BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                ], "try_exit")
                    .map_err(|e| format!("try_exit error: {}", e))?;
                let unreachable = self.context.append_basic_block(function, "unreachable");
                self.builder.build_unconditional_branch(unreachable)
                    .map_err(|e| format!("branch error: {}", e))?;

                self.builder.position_at_end(ok_bb);
                Ok(payload)
            }
            _ => Err("? operator requires a Result/Option type (struct pointer or value)".into()),
        }
    }

    fn compile_typeof_expr(
        &mut self,
        inner: &Box<Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // type_name(x): resolve type name at compile time
        let type_str = match inner.as_ref() {
            Expr::Ident(var_name) => self.var_type_names.get(var_name)
                .cloned().unwrap_or_else(|| "unknown".to_string()),
            _ => "unknown".to_string(),
        };
        // Build string literal struct { i8*, i64 }
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let global = self.builder.build_global_string_ptr(&type_str, "typename")
            .map_err(|e| format!("global string error: {}", e))?;
        let string_ty = self.context.struct_type(&[
            BasicTypeEnum::PointerType(i8_ptr),
            BasicTypeEnum::IntType(i64_ty),
        ], false);
        let alloca = self.builder.build_alloca(string_ty, "type_str")
            .map_err(|e| format!("alloca error: {}", e))?;
        let ptr_gep = self.builder.build_struct_gep(string_ty, alloca, 0, "ptr")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(ptr_gep, global.as_pointer_value())
            .map_err(|e| format!("store error: {}", e))?;
        let len_gep = self.builder.build_struct_gep(string_ty, alloca, 1, "len")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(len_gep, i64_ty.const_int(type_str.len() as u64, false))
            .map_err(|e| format!("store error: {}", e))?;
        Ok(alloca.into())
    }

    fn compile_typeinfo_expr(
        &mut self,
        ty: &Type,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // type_info(T): compile-time reflection on type (future)
        let _ = ty;
        Err("type_info is not available in codegen mode (compile-time reflection only)".into())
    }

    fn compile_old_expr(
        &mut self,
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // old(expr): snapshot value at function entry
        // For codegen, old() is transparent — just compile the inner expression
        self.compile_expr(inner, vars)
    }

    fn compile_tuple_expr(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut field_vals = Vec::new();
        for e in elems {
            field_vals.push(self.compile_expr(e, vars)?);
        }
        let field_tys: Vec<BasicTypeEnum<'ctx>> = field_vals.iter().map(|v| v.get_type()).collect();
        let struct_ty = self.context.struct_type(&field_tys, false);
        let alloca = self.builder.build_alloca(struct_ty, "tuple")
            .map_err(|e| format!("alloca error: {}", e))?;
        for (i, val) in field_vals.iter().enumerate() {
            let gep = self.builder.build_struct_gep(struct_ty, alloca, i as u32, &format!("tuple_{}", i))
                .map_err(|e| format!("gep error: {}", e))?;
            self.builder.build_store(gep, *val)
                .map_err(|e| format!("store error: {}", e))?;
        }
        Ok(alloca.into())
    }
    fn compile_if_expr(
        &mut self,
        cond: &Expr,
        then_: &Block,
        else_: &Option<Block>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let cond_val = self.compile_expr(cond, vars)?;
        let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
            iv
        } else {
            return Err("if expression condition must be boolean".into());
        };
        let function = self.current_function().ok_or_else(|| "codegen: no current function for if expr".to_string())?;
        let then_bb = self.context.append_basic_block(function, "ifexpr_then");
        let else_bb = self.context.append_basic_block(function, "ifexpr_else");
        let merge_bb = self.context.append_basic_block(function, "ifexpr_merge");
        self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        // Then branch
        self.builder.position_at_end(then_bb);
        let mut then_vars = vars.clone();
        let then_val = self.compile_block_last_val(then_, &mut then_vars).map_err(|e| e.to_string())?;
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(merge_bb)
                .map_err(|e| format!("branch error: {}", e))?;
        }
        let then_bb_end = self.builder.get_insert_block().ok_or_else(|| "codegen: no insert block after then branch".to_string())?;
        // Else branch
        self.builder.position_at_end(else_bb);
        let else_val = if let Some(eb) = else_ {
            let mut else_vars = vars.clone();
            let v = self.compile_block_last_val(eb, &mut else_vars).map_err(|e| e.to_string())?;
            if !self.block_has_terminator() {
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
            }
            Some(v)
        } else {
            if !self.block_has_terminator() {
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| format!("branch error: {}", e))?;
            }
            None
        };
        let else_bb_end = self.builder.get_insert_block().ok_or_else(|| "codegen: no insert block after else branch".to_string())?;
        // Merge with phi
        self.builder.position_at_end(merge_bb);
        let ty = then_val.get_type();
        let phi = self.builder.build_phi(ty, "ifexpr_result")
            .map_err(|e| format!("phi error: {}", e))?;
        let else_v = else_val.unwrap_or(self.context.i64_type().const_int(0, false).into());
        phi.add_incoming(&[
            (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
            (&else_v as &dyn inkwell::values::BasicValue, else_bb_end),
        ]);
        Ok(phi.as_basic_value())
    }

    fn compile_range_expr(
        &mut self,
        start: &Expr,
        end: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let start_val = self.compile_expr(start, vars)?;
        let end_val = self.compile_expr(end, vars)?;
        let start_iv = match start_val {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("[E0712] range start must be i64".into()),
        };
        let end_iv = match end_val {
            BasicValueEnum::IntValue(iv) => iv,
            _ => return Err("[E0712] range end must be i64".into()),
        };
        // Create a range struct { start: i64, end: i64 }
        let range_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(self.context.i64_type()),
            BasicTypeEnum::IntType(self.context.i64_type()),
        ], false);
        let alloca = self.builder.build_alloca(range_ty, "range")
            .map_err(|e| format!("alloca error: {}", e))?;
        let start_gep = self.builder.build_struct_gep(range_ty, alloca, 0, "range_start")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(start_gep, start_iv)
            .map_err(|e| format!("store error: {}", e))?;
        let end_gep = self.builder.build_struct_gep(range_ty, alloca, 1, "range_end")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(end_gep, end_iv)
            .map_err(|e| format!("store error: {}", e))?;
        Ok(alloca.into())
    }

    fn compile_slice_expr(
        &mut self,
        target: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Slice: arr[start..end] — compile target, compute slice offset and length
        let target_val = self.compile_expr(target, vars)?;
        let target_ptr = match target_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("slice target must be a list/array pointer".into()),
        };
        // Get list length from struct field 0
        let list_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(self.context.i64_type()),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let len_gep = self.builder.build_struct_gep(list_ty, target_ptr, 0, "slice_len")
            .map_err(|e| format!("gep error: {}", e))?;
        let list_len = self.builder.build_load(BasicTypeEnum::IntType(self.context.i64_type()), len_gep, "len")
            .map_err(|e| format!("load error: {}", e))?.into_int_value();
        let data_gep = self.builder.build_struct_gep(list_ty, target_ptr, 1, "slice_data")
            .map_err(|e| format!("gep error: {}", e))?;
        let data_ptr = self.builder.build_load(
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            data_gep, "data").map_err(|e| format!("load error: {}", e))?.into_pointer_value();
        // Compute start index (default 0)
        let start_idx = match start {
            Some(e) => self.compile_expr(e, vars)?.into_int_value(),
            None => self.context.i64_type().const_int(0, false),
        };
        // Compute end index (default: list length)
        let end_idx = match end {
            Some(e) => self.compile_expr(e, vars)?.into_int_value(),
            None => list_len,
        };
        // Compute new length = end - start
        let new_len = self.builder.build_int_sub(end_idx, start_idx, "slice_len")
            .map_err(|e| format!("sub error: {}", e))?;
        // Compute new data pointer: data + start * sizeof(i64)
        let i64_ty = self.context.i64_type();
        let elem_size = i64_ty.const_int(8, false);
        let byte_offset = self.builder.build_int_mul(start_idx, elem_size, "slice_offset")
            .map_err(|e| format!("mul error: {}", e))?;
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let data_i8 = self.builder.build_pointer_cast(data_ptr, i8_ptr, "data_as_i8")
            .map_err(|e| format!("bitcast error: {}", e))?;
        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
        let new_data_i8 = unsafe {
            self.builder.build_gep(self.context.i8_type(), data_i8, &[byte_offset], "new_data")
        }.map_err(|e| format!("gep error: {}", e))?;
        let new_data_ptr = self.builder.build_pointer_cast(new_data_i8,
            self.context.ptr_type(inkwell::AddressSpace::default()), "new_data_void")
            .map_err(|e| format!("bitcast error: {}", e))?;
        // Build new list struct { new_len, new_data_ptr }
        let result_alloca = self.builder.build_alloca(list_ty, "slice_result")
            .map_err(|e| format!("alloca error: {}", e))?;
        let rlen_gep = self.builder.build_struct_gep(list_ty, result_alloca, 0, "rlen")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(rlen_gep, new_len)
            .map_err(|e| format!("store error: {}", e))?;
        let rdata_gep = self.builder.build_struct_gep(list_ty, result_alloca, 1, "rdata")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(rdata_gep, new_data_ptr)
            .map_err(|e| format!("store error: {}", e))?;
        Ok(result_alloca.into())
    }
    fn compile_lambda_expr(
        &mut self,
        params: &[Param],
        ret: &Option<Type>,
        body: &Block,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let param_names: std::collections::HashSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        let mut free_vars = HashMap::new();
        self.collect_free_vars(body, &param_names, vars, &mut free_vars);

        let ret_type = match ret {
            Some(ty) => types::mimi_type_to_llvm(self.context, ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type())),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        // Function type: fn(env_ptr: i8*, params...) -> ret_type
        let mut param_types_llvm = vec![BasicTypeEnum::PointerType(i8_ptr)];
        for p in params {
            let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            param_types_llvm.push(ty);
        }
        let metadata_params: Vec<_> = param_types_llvm.iter()
            .map(|t| types::basic_to_metadata(self.context, *t)).collect();
        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let lambda_name = format!("__lambda_{}_{}", self.spawn_counter, body.len());
        self.spawn_counter += 1;
        let lambda_fn = self.module.add_function(&lambda_name, fn_type, None);
        let entry = self.context.append_basic_block(lambda_fn, "entry");
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

        let mut lambda_vars = vars.clone();
        // Bind env_ptr param (param 0)
        let env_ptr_param = lambda_fn.get_nth_param(0)
            .ok_or_else(|| "codegen: lambda env_ptr param index out of range".to_string())?
            .into_pointer_value();

        // Load captured variables from env struct
        if !free_vars.is_empty() {
            let env_field_types: Vec<BasicTypeEnum<'ctx>> =
                free_vars.values().map(|&(_, ty)| ty).collect();
            let env_struct_type = self.context.struct_type(&env_field_types, false);
            let env_struct_ptr = self.builder.build_pointer_cast(
                env_ptr_param,
                env_struct_type.ptr_type(inkwell::AddressSpace::default()),
                "env_struct",
            ).map_err(|e| format!("pointer cast error: {}", e))?;
            for (i, (name, &(_, ty))) in free_vars.iter().enumerate() {
                let field_gep = self.builder.build_struct_gep(
                    env_struct_type, env_struct_ptr, i as u32, &format!("env_{}_gep", name),
                ).map_err(|e| format!("gep error: {}", e))?;
                let field_val = self.builder.build_load(ty, field_gep, &format!("cap_{}", name))
                    .map_err(|e| format!("load error: {}", e))?;
                let alloca = self.builder.build_alloca(ty, &format!("cap_{}_alloca", name))
                    .map_err(|e| format!("alloca error: {}", e))?;
                self.builder.build_store(alloca, field_val)
                    .map_err(|e| format!("store error: {}", e))?;
                lambda_vars.insert(name.clone(), (alloca, ty));
            }
        }

        // Bind regular parameters (params start at index 1)
        let mut param_idx = 1u32;
        for p in params.iter() {
            let ty = types::mimi_type_to_llvm(self.context, &p.ty)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            let alloca = self.builder.build_alloca(ty, &p.name)
                .map_err(|e| format!("alloca error: {}", e))?;
            self.builder.build_store(alloca, lambda_fn.get_nth_param(param_idx).ok_or_else(|| "codegen: lambda param index out of range".to_string())?)
                .map_err(|e| format!("store error: {}", e))?;
            lambda_vars.insert(p.name.clone(), (alloca, ty));
            param_idx += 1;
        }

        // Compile body
        let mut last_val = self.context.i64_type().const_int(0, false).into();
        for stmt in body {
            match stmt {
                Stmt::Expr(e) => { last_val = self.compile_expr(e, &lambda_vars)?; }
                Stmt::Return(Some(e)) => {
                    let v = self.compile_expr(e, &lambda_vars)?;
                    self.builder.build_return(Some(&v)).map_err(|e| format!("return error: {}", e))?;
                    break;
                }
                Stmt::Return(None) => {
                    self.builder.build_return(None).map_err(|e| format!("return error: {}", e))?;
                    break;
                }
                Stmt::Let { pat, init: Some(init), .. } => {
                    let val = self.compile_expr(init, &lambda_vars)?;
                    let name = match pat { Pattern::Variable(n) => n.clone(), _ => continue };
                    let llvm_ty = val.get_type();
                    let alloca = self.builder.build_alloca(llvm_ty, &name).map_err(|e| format!("alloca error: {}", e))?;
                    self.builder.build_store(alloca, val).map_err(|e| format!("store error: {}", e))?;
                    lambda_vars.insert(name, (alloca, llvm_ty));
                }
                _ => {}
            }
        }
        if !self.block_has_terminator() {
            self.builder.build_return(Some(&last_val)).map_err(|e| format!("return error: {}", e))?;
        }
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        // Build closure struct: { fn_ptr: i8*, env_ptr: i8* } on stack
        let closure_struct_type = types::closure_struct_type(self.context);
        let closure_alloca = self.builder.build_alloca(
            BasicTypeEnum::StructType(closure_struct_type),
            "closure",
        ).map_err(|e| format!("alloca error: {}", e))?;

        let fn_ptr = lambda_fn.as_global_value().as_pointer_value();
        let fn_gep = self.builder.build_struct_gep(
            closure_struct_type, closure_alloca, 0, "fn_gep",
        ).map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(fn_gep, fn_ptr)
            .map_err(|e| format!("store error: {}", e))?;

        if !free_vars.is_empty() {
            let env_field_types: Vec<BasicTypeEnum<'ctx>> =
                free_vars.values().map(|&(_, ty)| ty).collect();
            let env_struct_type = self.context.struct_type(&env_field_types, false);
            let env_byte_size = env_struct_type.size_of().ok_or_else(|| "size_of error".to_string())?;
            let malloc_fn = self.module.get_function("malloc")
                .ok_or_else(|| "malloc not declared".to_string())?;
            let env_heap_ptr = self.builder.build_call(malloc_fn, &[
                BasicMetadataValueEnum::IntValue(env_byte_size),
            ], "env_heap")
                .map_err(|e| format!("malloc error: {}", e))?
                .try_as_basic_value().left()
                .ok_or("malloc returned void")?
                .into_pointer_value();
            // NOTE: not registered in heap_allocs — closure env must outlive
            // the creating scope if the closure escapes (returned or stored
            // to a shared variable), so we cannot auto-free it on scope exit.
            for (i, (name, &(var_alloca, ty))) in free_vars.iter().enumerate() {
                let val = self.builder.build_load(ty, var_alloca, &format!("cap_val_{}", name))
                    .map_err(|e| format!("load error: {}", e))?;
                let field_gep = self.builder.build_struct_gep(
                    env_struct_type, env_heap_ptr, i as u32, &format!("env_{}_gep", name),
                ).map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(field_gep, val)
                    .map_err(|e| format!("store error: {}", e))?;
            }
            let env_gep = self.builder.build_struct_gep(
                closure_struct_type, closure_alloca, 1, "env_gep",
            ).map_err(|e| format!("gep error: {}", e))?;
            let env_ptr_i8 = self.builder.build_pointer_cast(
                env_heap_ptr,
                i8_ptr,
                "env_ptr_i8",
            ).map_err(|e| format!("pointer cast error: {}", e))?;
            self.builder.build_store(env_gep, env_ptr_i8)
                .map_err(|e| format!("store error: {}", e))?;
        } else {
            let env_gep = self.builder.build_struct_gep(
                closure_struct_type, closure_alloca, 1, "env_gep",
            ).map_err(|e| format!("gep error: {}", e))?;
            self.builder.build_store(env_gep, i8_ptr.const_null())
                .map_err(|e| format!("store error: {}", e))?;
        }

        let closure_val = self.builder.build_load(
            BasicTypeEnum::StructType(closure_struct_type),
            closure_alloca,
            "closure_val",
        ).map_err(|e| format!("load error: {}", e))?;
        Ok(closure_val)
    }

    fn compile_comprehension_expr(
        &mut self,
        expr: &Expr,
        var: &String,
        iter: &Expr,
        guard: &Option<Box<Expr>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // List comprehension: [expr for x in iter if guard]
        // Compile iter to get list pointer
        let iter_val = self.compile_expr(iter, vars)?;
        let list_ptr = match iter_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Err("comprehension iter must be a list pointer".into()),
        };
        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let list_struct_ty = BasicTypeEnum::StructType(self.context.struct_type(&[
            BasicTypeEnum::IntType(i64_ty),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false));
        // Read list length and data
        let len_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 0, "comp_len")
            .map_err(|e| format!("gep error: {}", e))?;
        let list_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), len_gep, "len")
            .map_err(|e| format!("load error: {}", e))?.into_int_value();
        let data_gep = self.builder.build_struct_gep(list_struct_ty, list_ptr, 1, "comp_data")
            .map_err(|e| format!("gep error: {}", e))?;
        let data_i8 = self.builder.build_load(BasicTypeEnum::PointerType(i8_ptr), data_gep, "data")
            .map_err(|e| format!("load error: {}", e))?.into_pointer_value();
        let data_ptr = self.builder.build_bit_cast(data_i8,
            i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
            .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
        // Allocate output array (same max size as input)
        let elem_size = i64_ty.const_int(8, false);
        let alloc_size = self.builder.build_int_mul(list_len, elem_size, "comp_alloc")
            .map_err(|e| format!("mul error: {}", e))?;
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let out_ptr = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(alloc_size),
        ], "comp_malloc")
            .map_err(|e| format!("malloc error: {}", e))?
            .try_as_basic_value().left()
            .ok_or("malloc returned void")?.into_pointer_value();
        let out_i64 = self.builder.build_bit_cast(out_ptr,
            i64_ty.ptr_type(inkwell::AddressSpace::default()), "out_i64")
            .map_err(|e| format!("bitcast error: {}", e))?.into_pointer_value();
        // Loop: for i in 0..len
        let function = self.current_function().ok_or_else(|| "codegen: no current function for comprehension".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "comp_loop");
        let body_bb = self.context.append_basic_block(function, "comp_body");
        let done_bb = self.context.append_basic_block(function, "comp_done");
        let idx_alloca = self.builder.build_alloca(i64_ty, "ci")
            .map_err(|e| format!("alloca error: {}", e))?;
        let wi_alloca = self.builder.build_alloca(i64_ty, "cw")
            .map_err(|e| format!("alloca error: {}", e))?;
        self.builder.build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| format!("store error: {}", e))?;
        self.builder.build_store(wi_alloca, i64_ty.const_int(0, false))
            .map_err(|e| format!("store error: {}", e))?;
        self.builder.build_unconditional_branch(loop_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(loop_bb);
        let idx = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
            .map_err(|e| format!("load error: {}", e))?.into_int_value();
        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
            .map_err(|e| format!("cmp error: {}", e))?;
        self.builder.build_conditional_branch(cmp, body_bb, done_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(body_bb);
        // Load element
        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
        let elem_ptr = unsafe {
            self.builder.build_gep(i64_ty, data_ptr, &[idx], "elem")
        }.map_err(|e| format!("gep error: {}", e))?;
        let elem = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
            .map_err(|e| format!("load error: {}", e))?;
        // Bind var
        let mut comp_vars = vars.clone();
        let elem_alloca = self.builder.build_alloca(i64_ty, var)
            .map_err(|e| format!("alloca error: {}", e))?;
        self.builder.build_store(elem_alloca, elem)
            .map_err(|e| format!("store error: {}", e))?;
        comp_vars.insert(var.clone(), (elem_alloca, BasicTypeEnum::IntType(i64_ty)));
        // Check guard
        let include = if let Some(g) = guard {
            let g_val = self.compile_expr(g, &comp_vars)?;
            let g_bool = match g_val {
                BasicValueEnum::IntValue(iv) => self.builder.build_int_z_extend(iv, i64_ty, "g_ext")
                    .map_err(|e| format!("zext error: {}", e))?,
                _ => return Err("guard must be boolean".into()),
            };
            self.builder.build_int_compare(inkwell::IntPredicate::NE, g_bool, i64_ty.const_int(0, false), "g_truthy")
                .map_err(|e| format!("cmp error: {}", e))?
        } else {
            self.context.bool_type().const_int(1, false)
        };
        let store_bb = self.context.append_basic_block(function, "comp_store");
        let next_bb = self.context.append_basic_block(function, "comp_next");
        self.builder.build_conditional_branch(include, store_bb, next_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(store_bb);
        // Evaluate expression
        let result = self.compile_expr(expr, &comp_vars)?;
        let wi = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "wi")
            .map_err(|e| format!("load error: {}", e))?.into_int_value();
        // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
        let out_elem_ptr = unsafe {
            self.builder.build_gep(i64_ty, out_i64, &[wi], "out_elem")
        }.map_err(|e| format!("gep error: {}", e))?;
        let result_i64 = match result {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::FloatValue(fv) => self.builder.build_float_to_signed_int(fv, i64_ty, "f_to_i")
                .map_err(|e| format!("fptosi error: {}", e))?,
            BasicValueEnum::PointerValue(pv) => self.builder.build_ptr_to_int(pv, i64_ty, "p_to_i")
                .map_err(|e| format!("ptrtoint error: {}", e))?,
            _ => return Err("comprehension expression must produce i64-compatible value".into()),
        };
        self.builder.build_store(out_elem_ptr, result_i64)
            .map_err(|e| format!("store error: {}", e))?;
        let next_wi = self.builder.build_int_add(wi, i64_ty.const_int(1, false), "next_wi")
            .map_err(|e| format!("add error: {}", e))?;
        self.builder.build_store(wi_alloca, next_wi)
            .map_err(|e| format!("store error: {}", e))?;
        self.builder.build_unconditional_branch(next_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(next_bb);
        let next = self.builder.build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| format!("add error: {}", e))?;
        self.builder.build_store(idx_alloca, next)
            .map_err(|e| format!("store error: {}", e))?;
        self.builder.build_unconditional_branch(loop_bb)
            .map_err(|e| format!("branch error: {}", e))?;
        self.builder.position_at_end(done_bb);
        // Build result list
        let result_len = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), wi_alloca, "result_len")
            .map_err(|e| format!("load error: {}", e))?;
        let result_alloca = self.builder.build_alloca(list_struct_ty, "comp_result")
            .map_err(|e| format!("alloca error: {}", e))?;
        let rlen_gep = self.builder.build_struct_gep(list_struct_ty, result_alloca, 0, "rlen")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(rlen_gep, result_len)
            .map_err(|e| format!("store error: {}", e))?;
        let rdata_gep = self.builder.build_struct_gep(list_struct_ty, result_alloca, 1, "rdata")
            .map_err(|e| format!("gep error: {}", e))?;
        let out_void = self.builder.build_pointer_cast(out_i64, i8_ptr, "out_void")
            .map_err(|e| format!("bitcast error: {}", e))?;
        self.builder.build_store(rdata_gep, out_void)
            .map_err(|e| format!("store error: {}", e))?;
        Ok(result_alloca.into())
    }
    /// Infer the type name of an object expression from the codegen's type definitions
    /// Build a List<string> from a slice of string values (compile-time constant list)
    fn build_string_list(
        &self,
        strings: &[String],
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i8_ty = self.context.i8_type();
        let i8_ptr = i8_ty.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let count = strings.len() as u64;

        // Allocate array of string structs: [ { i8*, i64 } x N ]
        let str_ty = self.context.struct_type(&[
            BasicTypeEnum::PointerType(i8_ptr),
            BasicTypeEnum::IntType(i64_ty),
        ], false);
        let arr_type = str_ty.array_type(count as u32);
        let arr_alloca = self.builder.build_alloca(BasicTypeEnum::ArrayType(arr_type), "str_arr")
            .map_err(|e| format!("alloca error: {}", e))?;

        for (i, s) in strings.iter().enumerate() {
            let global = self.builder.build_global_string_ptr(s, &format!("str_{}", i))
                .map_err(|e| format!("global string error: {}", e))?;
            let elem_ptr = self.builder.build_struct_gep(
                BasicTypeEnum::StructType(str_ty),
                arr_alloca,
                i as u32,
                &format!("elem_{}", i),
            ).map_err(|e| format!("gep error: {}", e))?;
            let ptr_gep = self.builder.build_struct_gep(str_ty, elem_ptr, 0, "ptr")
                .map_err(|e| format!("gep error: {}", e))?;
            self.builder.build_store(ptr_gep, global.as_pointer_value())
                .map_err(|e| format!("store error: {}", e))?;
            let len_gep = self.builder.build_struct_gep(str_ty, elem_ptr, 1, "len")
                .map_err(|e| format!("gep error: {}", e))?;
            self.builder.build_store(len_gep, i64_ty.const_int(s.len() as u64, false))
                .map_err(|e| format!("store error: {}", e))?;
        }

        // Build list struct: { i64 len, i8* data }
        let list_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(i64_ty),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let list_alloca = self.builder.build_alloca(list_ty, "str_list")
            .map_err(|e| format!("alloca error: {}", e))?;
        let len_gep = self.builder.build_struct_gep(list_ty, list_alloca, 0, "len")
            .map_err(|e| format!("gep error: {}", e))?;
        self.builder.build_store(len_gep, i64_ty.const_int(count, false))
            .map_err(|e| format!("store error: {}", e))?;
        let data_gep = self.builder.build_struct_gep(list_ty, list_alloca, 1, "data")
            .map_err(|e| format!("gep error: {}", e))?;
        let arr_void_ptr = self.builder.build_pointer_cast(
            arr_alloca,
            i8_ptr,
            "arr_void"
        ).map_err(|e| format!("bitcast error: {}", e))?;
        self.builder.build_store(data_gep, arr_void_ptr)
            .map_err(|e| format!("store error: {}", e))?;
        Ok(list_alloca.into())
    }

    /// Collect free variables used in a block that are defined in the enclosing scope
    fn collect_free_vars(
        &self,
        block: &Block,
        param_names: &std::collections::HashSet<String>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        free_vars: &mut HashMap<String, (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    ) {
        let mut defined = param_names.clone();
        for stmt in block {
            match stmt {
                Stmt::Expr(e) => self.collect_free_vars_expr(e, &defined, vars, free_vars),
                Stmt::Let { pat, init: Some(init), .. } => {
                    self.collect_free_vars_expr(init, &defined, vars, free_vars);
                    if let Pattern::Variable(name) = pat {
                        defined.insert(name.clone());
                    }
                }
                Stmt::Return(Some(e)) => self.collect_free_vars_expr(e, &defined, vars, free_vars),
                Stmt::If { cond, then_, else_ } => {
                    self.collect_free_vars_expr(cond, &defined, vars, free_vars);
                    self.collect_free_vars(then_, &defined, vars, free_vars);
                    if let Some(eb) = else_ {
                        self.collect_free_vars(eb, &defined, vars, free_vars);
                    }
                }
                _ => {}
            }
        }
    }

    fn collect_free_vars_expr(
        &self,
        expr: &Expr,
        defined: &std::collections::HashSet<String>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        free_vars: &mut HashMap<String, (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)>,
    ) {
        match expr {
            Expr::Ident(name) => {
                if !defined.contains(name.as_str()) {
                    if let Some(&(ptr, ty)) = vars.get(name.as_str()) {
                        free_vars.entry(name.clone()).or_insert((ptr, ty));
                    }
                }
            }
            Expr::Binary(_, l, r) => {
                self.collect_free_vars_expr(l, defined, vars, free_vars);
                self.collect_free_vars_expr(r, defined, vars, free_vars);
            }
            Expr::Unary(_, e) => self.collect_free_vars_expr(e, defined, vars, free_vars),
            Expr::Call(callee, args) => {
                self.collect_free_vars_expr(callee, defined, vars, free_vars);
                for arg in args {
                    self.collect_free_vars_expr(arg, defined, vars, free_vars);
                }
            }
            Expr::Field(obj, _) => self.collect_free_vars_expr(obj, defined, vars, free_vars),
            Expr::Index(obj, idx) => {
                self.collect_free_vars_expr(obj, defined, vars, free_vars);
                self.collect_free_vars_expr(idx, defined, vars, free_vars);
            }
            Expr::List(elems) | Expr::Tuple(elems) => {
                for e in elems {
                    self.collect_free_vars_expr(e, defined, vars, free_vars);
                }
            }
            Expr::If { cond, then_, else_ } => {
                self.collect_free_vars_expr(cond, defined, vars, free_vars);
                self.collect_free_vars(then_, defined, vars, free_vars);
                if let Some(eb) = else_ {
                    self.collect_free_vars(eb, defined, vars, free_vars);
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn infer_object_type(&self, expr: &Expr, vars: &HashMap<String, VarEntry<'ctx>>) -> String {
        match expr {
            Expr::Ident(name) => {
                // Look up variable's type name from our tracking map
                if let Some(ty_name) = self.var_type_names.get(name) {
                    ty_name.clone()
                } else {
                    name.clone()
                }
            }
            Expr::Record { ty: Some(name), .. } => name.clone(),
            Expr::Call(callee, _) => {
                // constructor call like ActorName(args) -> return type is the name
                if let Expr::Ident(name) = callee.as_ref() {
                    // Try to strip _new suffix used by our codegen constructors
                    if let Some(stripped) = name.strip_suffix("_new") {
                        stripped.to_string()
                    } else {
                        name.clone()
                    }
                } else {
                    String::new()
                }
            }
            Expr::Field(obj, _) => self.infer_object_type(obj, vars),
            _ => String::new(),
        }
    }

    fn compile_fstring(
        &mut self,
        parts: &[crate::ast::FStringPart],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let _i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        if parts.is_empty() {
            let global = self.builder.build_global_string_ptr("", "fstr_empty")
                .map_err(|e| format!("string error: {}", e))?;
            return Ok(global.as_pointer_value().into());
        }

        // Optimization: if all parts are text, return a single global string
        let all_text: Option<String> = parts.iter().map(|p| {
            match p {
                crate::ast::FStringPart::Text(t) => Some(t.as_str()),
                _ => None,
            }
        }).collect();
        if let Some(text) = all_text {
            let global = self.builder.build_global_string_ptr(&text, "fstr_literal")
                .map_err(|e| format!("string error: {}", e))?;
            return Ok(global.as_pointer_value().into());
        }

        // For f-strings with interpolation: use malloc + strcpy + strcat
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let strcpy_fn = self.module.get_function("strcpy")
            .ok_or_else(|| "strcpy not declared".to_string())?;
        let strcat_fn = self.module.get_function("strcat")
            .ok_or_else(|| "strcat not declared".to_string())?;
        let strlen_fn = self.module.get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let sprintf_fn = self.module.get_function("sprintf")
            .ok_or_else(|| "sprintf not declared".to_string())?;

        // Allocate a 1024-byte buffer for the result
        let buf_size = i64_ty.const_int(1024, false);
        let buf = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(buf_size),
        ], "fstr_buf")
            .map_err(|e| format!("malloc error: {}", e))?
            .try_as_basic_value().left()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        self.register_heap_alloc(buf);

        // Initialize buffer with empty string
        let empty = self.builder.build_global_string_ptr("", "fstr_empty_init")
            .map_err(|e| format!("string error: {}", e))?;
        self.builder.build_call(strcpy_fn, &[
            BasicMetadataValueEnum::PointerValue(buf),
            BasicMetadataValueEnum::PointerValue(empty.as_pointer_value()),
        ], "fstr_init")
            .map_err(|e| format!("strcpy error: {}", e))?;

        // Append each part
        for (i, part) in parts.iter().enumerate() {
            match part {
                crate::ast::FStringPart::Text(t) => {
                    if t.is_empty() { continue; }
                    let global = self.builder.build_global_string_ptr(t, &format!("fstr_part_{}", i))
                        .map_err(|e| format!("string error: {}", e))?;
                    self.builder.build_call(strcat_fn, &[
                        BasicMetadataValueEnum::PointerValue(buf),
                        BasicMetadataValueEnum::PointerValue(global.as_pointer_value()),
                    ], &format!("fstr_cat_{}", i))
                        .map_err(|e| format!("strcat error: {}", e))?;
                }
                crate::ast::FStringPart::Interp(expr) => {
                    let val = self.compile_expr(expr, vars)?;
                    // Convert value to string based on type
                    match val {
                        BasicValueEnum::IntValue(iv) => {
                            let len = self.builder.build_call(strlen_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                            ], "fstr_strlen")
                                .map_err(|e| format!("strlen error: {}", e))?
                                .try_as_basic_value().left()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            let i8_type = self.context.i8_type();
                            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                            let pos = unsafe { self.builder.build_gep(i8_type, buf, &[len], "fstr_pos") }
                                .map_err(|e| format!("gep error: {}", e))?;
                            let fmt = self.builder.build_global_string_ptr("%ld", &format!("fstr_fmt_{}", i))
                                .map_err(|e| format!("string error: {}", e))?;
                            self.builder.build_call(sprintf_fn, &[
                                BasicMetadataValueEnum::PointerValue(pos),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::IntValue(iv),
                            ], &format!("fstr_sprintf_{}", i))
                                .map_err(|e| format!("sprintf error: {}", e))?;
                        }
                        BasicValueEnum::FloatValue(fv) => {
                            let len = self.builder.build_call(strlen_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                            ], "fstr_strlen")
                                .map_err(|e| format!("strlen error: {}", e))?
                                .try_as_basic_value().left()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            let i8_type = self.context.i8_type();
                            // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                            let pos = unsafe { self.builder.build_gep(i8_type, buf, &[len], "fstr_pos") }
                                .map_err(|e| format!("gep error: {}", e))?;
                            let fmt = self.builder.build_global_string_ptr("%f", &format!("fstr_fmt_{}", i))
                                .map_err(|e| format!("string error: {}", e))?;
                            self.builder.build_call(sprintf_fn, &[
                                BasicMetadataValueEnum::PointerValue(pos),
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::FloatValue(fv),
                            ], &format!("fstr_sprintf_{}", i))
                                .map_err(|e| format!("sprintf error: {}", e))?;
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            // String pointer: use strcat
                            self.builder.build_call(strcat_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(pv),
                            ], &format!("fstr_cat_{}", i))
                                .map_err(|e| format!("strcat error: {}", e))?;
                        }
                        _ => {
                            let unknown = self.builder.build_global_string_ptr("<unsupported>", &format!("fstr_unsup_{}", i))
                                .map_err(|e| format!("string error: {}", e))?;
                            self.builder.build_call(strcat_fn, &[
                                BasicMetadataValueEnum::PointerValue(buf),
                                BasicMetadataValueEnum::PointerValue(unknown.as_pointer_value()),
                            ], &format!("fstr_cat_unsup_{}", i))
                                .map_err(|e| format!("strcat error: {}", e))?;
                        }
                    }
                }
            }
        }

        Ok(buf.into())
    }

    fn compile_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (lhs, rhs) = match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let lw = l.get_type().get_bit_width();
                let rw = r.get_type().get_bit_width();
                if lw == rw {
                    (lhs, rhs)
                } else if lw < rw {
                    let ext = self.builder.build_int_z_extend(l, r.get_type(), "promote")
                        .map_err(|e| format!("int promote error: {}", e))?;
                    (ext.into(), rhs)
                } else {
                    let ext = self.builder.build_int_z_extend(r, l.get_type(), "promote")
                        .map_err(|e| format!("int promote error: {}", e))?;
                    (lhs, ext.into())
                }
            }
            _ => (lhs, rhs),
        };
        match op {
            BinOp::Add => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_add(l, r, "add").map_err(|e| format!("add error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_add(l, r, "fadd").map_err(|e| format!("add error: {}", e))?.into()),
                _ => Err("add requires same numeric types".into()),
            },
            BinOp::Sub => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_sub(l, r, "sub").map_err(|e| format!("sub error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_sub(l, r, "fsub").map_err(|e| format!("sub error: {}", e))?.into()),
                _ => Err("sub requires same numeric types".into()),
            },
            BinOp::Mul => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_mul(l, r, "mul").map_err(|e| format!("mul error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_mul(l, r, "fmul").map_err(|e| format!("mul error: {}", e))?.into()),
                _ => Err("mul requires same numeric types".into()),
            },
            BinOp::Div => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_div(l, r, "div").map_err(|e| format!("div error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_div(l, r, "fdiv").map_err(|e| format!("div error: {}", e))?.into()),
                _ => Err("div requires same numeric types".into()),
            },
            BinOp::Mod => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_rem(l, r, "rem").map_err(|e| format!("rem error: {}", e))?.into()),
                _ => Err("mod requires integer types".into()),
            },
            BinOp::EqCmp => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "eq").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "feq").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::PointerValue(l), BasicValueEnum::PointerValue(r)) => {
                    let strcmp_fn = self.module.get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let result = self.builder.build_call(strcmp_fn, &[
                        BasicMetadataValueEnum::PointerValue(l),
                        BasicMetadataValueEnum::PointerValue(r),
                    ], "strcmp_call")
                        .map_err(|e| format!("strcmp error: {}", e))?
                        .try_as_basic_value()
                        .left()
                        .ok_or_else(|| "strcmp returned void".to_string())?;
                    let cmp = result.into_int_value();
                    let zero = self.context.i32_type().const_int(0, false);
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp, zero, "streq")
                        .map_err(|e| format!("cmp error: {}", e))?.into())
                }
                _ => Err("eq requires same types".into()),
            },
            BinOp::NeCmp => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::NE, l, r, "ne").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "fne").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::PointerValue(l), BasicValueEnum::PointerValue(r)) => {
                    let strcmp_fn = self.module.get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let result = self.builder.build_call(strcmp_fn, &[
                        BasicMetadataValueEnum::PointerValue(l),
                        BasicMetadataValueEnum::PointerValue(r),
                    ], "strcmp_call")
                        .map_err(|e| format!("strcmp error: {}", e))?
                        .try_as_basic_value()
                        .left()
                        .ok_or_else(|| "strcmp returned void".to_string())?;
                    let cmp = result.into_int_value();
                    let zero = self.context.i32_type().const_int(0, false);
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::NE, cmp, zero, "strne")
                        .map_err(|e| format!("cmp error: {}", e))?.into())
                }
                _ => Err("ne requires same types".into()),
            },
            BinOp::Lt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLT, l, r, "lt").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OLT, l, r, "flt").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("lt requires same numeric types".into()),
            },
            BinOp::Gt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGT, l, r, "gt").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OGT, l, r, "fgt").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("gt requires same numeric types".into()),
            },
            BinOp::Le => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLE, l, r, "le").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OLE, l, r, "fle").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("le requires same numeric types".into()),
            },
            BinOp::Ge => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGE, l, r, "ge").map_err(|e| format!("cmp error: {}", e))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OGE, l, r, "fge").map_err(|e| format!("cmp error: {}", e))?.into()),
                _ => Err("ge requires same numeric types".into()),
            },
            BinOp::And => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_and(l, r, "and").map_err(|e| format!("and error: {}", e))?.into()),
                _ => Err("and requires boolean types".into()),
            },
            BinOp::Or => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_or(l, r, "or").map_err(|e| format!("or error: {}", e))?.into()),
                _ => Err("or requires boolean types".into()),
            },
            BinOp::Range => {
                let start_iv = match lhs {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] range start must be i64".into()),
                };
                let end_iv = match rhs {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("[E0712] range end must be i64".into()),
                };
                // Create a range struct { start: i64, end: i64 }
                let i64_ty = self.context.i64_type();
                let range_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let alloca = self.builder.build_alloca(range_ty, "range")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let start_gep = self.builder.build_struct_gep(range_ty, alloca, 0, "range_start")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(start_gep, start_iv)
                    .map_err(|e| format!("store error: {}", e))?;
                let end_gep = self.builder.build_struct_gep(range_ty, alloca, 1, "range_end")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(end_gep, end_iv)
                    .map_err(|e| format!("store error: {}", e))?;
                Ok(alloca.into())
            }
            BinOp::Pow => match (lhs, rhs) {
                (BasicValueEnum::IntValue(base), BasicValueEnum::IntValue(exp)) => {
                    let pow_fn_name = "__mimi_pow_i64";
                    let i64_ty = self.context.i64_type();
                    let fn_ty = i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false);
                    let pow_fn = self.module.get_function(pow_fn_name)
                        .unwrap_or_else(|| {
                            self.module.add_function(pow_fn_name, fn_ty, Some(inkwell::module::Linkage::External))
                        });
                    Ok(self.builder.build_call(pow_fn, &[
                        BasicMetadataValueEnum::IntValue(base),
                        BasicMetadataValueEnum::IntValue(exp),
                    ], "pow_i64_call")
                        .map_err(|e| format!("pow error: {}", e))?
                        .try_as_basic_value().left()
                        .ok_or("pow returned void")?
                        .into())
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let pow_fn = self.module.get_function("llvm.pow.f64")
                        .ok_or_else(|| "llvm.pow.f64 not declared".to_string())?;
                    Ok(self.builder.build_call(pow_fn, &[
                        BasicMetadataValueEnum::FloatValue(l),
                        BasicMetadataValueEnum::FloatValue(r),
                    ], "pow_f64")
                        .map_err(|e| format!("pow error: {}", e))?
                        .try_as_basic_value().left()
                        .ok_or("pow returned void")?
                        .into())
                }
                _ => Err("pow requires matching numeric types".into()),
            },
            BinOp::BitAnd => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_and(l, r, "bitand").map_err(|e| format!("and error: {}", e))?.into()),
                _ => Err("bitand requires integer types".into()),
            },
            BinOp::BitOr => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_or(l, r, "bitor").map_err(|e| format!("or error: {}", e))?.into()),
                _ => Err("bitor requires integer types".into()),
            },
            BinOp::BitXor => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_xor(l, r, "bitxor").map_err(|e| format!("xor error: {}", e))?.into()),
                _ => Err("bitxor requires integer types".into()),
            },
            BinOp::Shl => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_left_shift(l, r, "shl").map_err(|e| format!("shl error: {}", e))?.into()),
                _ => Err("shl requires integer types".into()),
            },
            BinOp::Shr => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_right_shift(l, r, false, "shr").map_err(|e| format!("shr error: {}", e))?.into()),
                _ => Err("shr requires integer types".into()),
            },
            _ => Err(format!("unsupported binary operator {:?}", op)),
        }
    }

    fn compile_call(
        &mut self,
        name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut compiled_args = Vec::new();
        for arg in args {
            compiled_args.push(self.compile_expr(arg, vars)?);
        }

        // G1b: Convert closure struct args to thunk pointers for extern callback params
        if let Some(param_types) = self.extern_param_types.get(name).cloned() {
            for (i, compiled) in compiled_args.iter_mut().enumerate() {
                if i >= param_types.len() { break; }
                let (cb_params, cb_ret) = match &param_types[i] {
                    crate::ast::Type::ExternFunc(p, r) => (p.as_slice(), r.as_ref()),
                    crate::ast::Type::Func(p, r) => (p.as_slice(), r.as_ref()),
                    _ => continue,
                };
                if let BasicValueEnum::StructValue(sv) = compiled {
                        let struct_ty = sv.get_type();
                        if struct_ty.get_field_types().len() == 2 {
                            let fn_ptr = self.builder.build_extract_value(*sv, 0, "cb_fn_ptr")
                                .map_err(|e| format!("extract fn_ptr: {}", e))?;
                            let env_ptr = self.builder.build_extract_value(*sv, 1, "cb_env_ptr")
                                .map_err(|e| format!("extract env_ptr: {}", e))?;
                            let cb_fn_ptr = fn_ptr.into_pointer_value();
                            let cb_env_ptr = env_ptr.into_pointer_value();
                            let thunk_entry = self.get_or_create_callback_thunk(cb_params, cb_ret)
                                .map_err(|e| format!("callback thunk: {}", e))?;
                            self.builder.build_store(
                                thunk_entry.fn_ptr_global.as_pointer_value(), cb_fn_ptr,
                            ).map_err(|e| format!("store fn_ptr: {}", e))?;
                            self.builder.build_store(
                                thunk_entry.env_ptr_global.as_pointer_value(), cb_env_ptr,
                            ).map_err(|e| format!("store env_ptr: {}", e))?;
                            let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                            let thunk_ptr = thunk_entry.thunk_fn.as_global_value().as_pointer_value();
                            let casted = self.builder.build_pointer_cast(thunk_ptr, i8_ptr_ty, "thunk_i8")
                                .map_err(|e| format!("bitcast thunk: {}", e))?;
                            *compiled = casted.into();
                        }
                    }
                }
            }

        let metadata_args: Vec<_> = compiled_args.iter().map(|v| {
            match v {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
            }
        }).collect();

        // Dispatch builtins
        if super::builtins::is_builtin(name) {
            return self.compile_builtin_call(name, &metadata_args).map_err(|e| e.to_string());
        }

        // Handle built-in Option/Result constructors
        match name {
            "Ok" | "Some" => {
                if compiled_args.len() != 1 {
                    return Err(format!("[E0711] {} expects 1 argument", name));
                }
                let val = compiled_args[0];
                let bool_ty = self.context.bool_type();
                let disc = bool_ty.const_int(1, false);
                let inner_ty = val.get_type();
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    inner_ty,
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "result_val")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| format!("store error: {}", e))?;
                let val_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "payload")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(val_gep, val)
                    .map_err(|e| format!("store error: {}", e))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| format!("load error: {}", e))?;
                return Ok(result);
            }
            "Err" | "None" => {
                if name == "Err" && compiled_args.len() != 1 {
                    return Err("[E0711] Err expects 1 argument".into());
                }
                if name == "None" && compiled_args.len() != 0 {
                    return Err("[E0711] None expects 0 arguments".into());
                }
                let bool_ty = self.context.bool_type();
                let disc = bool_ty.const_int(0, false);
                let payload_ty = BasicTypeEnum::IntType(self.context.i64_type());
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    payload_ty,
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "result_val")
                    .map_err(|e| format!("alloca error: {}", e))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| format!("store error: {}", e))?;
                let val_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "payload")
                    .map_err(|e| format!("gep error: {}", e))?;
                self.builder.build_store(val_gep, self.context.i64_type().const_int(0, false))
                    .map_err(|e| format!("store error: {}", e))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| format!("load error: {}", e))?;
                return Ok(result);
            }
            _ => {}
        }

        if let Some(function) = self.module.get_function(name) {
            let call = self.builder.build_call(function, &metadata_args, "call")
                .map_err(|e| format!("call error: {}", e))?;
            Ok(call.try_as_basic_value().left().unwrap_or(
                self.context.i64_type().const_int(0, false).into()
            ))
        } else {
            // Try mangled name with current type_map
            let mangled = Self::mangle_name(name, &self.type_map);
            if let Some(function) = self.module.get_function(&mangled) {
                let call = self.builder.build_call(function, &metadata_args, "call")
                    .map_err(|e| format!("call error: {}", e))?;
                Ok(call.try_as_basic_value().left().unwrap_or(
                    self.context.i64_type().const_int(0, false).into()
                ))
            } else {
                Err(format!("undefined function '{}' in codegen", name))
            }
        }
    }

    /// Call a function by its mangled name
    fn compile_call_mangled(
        &mut self,
        mangled: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut compiled_args = Vec::new();
        for arg in args {
            compiled_args.push(self.compile_expr(arg, vars)?);
        }

        let metadata_args: Vec<_> = compiled_args.iter().map(|v| {
            match v {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
            }
        }).collect();

        if let Some(function) = self.module.get_function(mangled) {
            let call = self.builder.build_call(function, &metadata_args, "call")
                .map_err(|e| format!("call error: {}", e))?;
            Ok(call.try_as_basic_value().left().unwrap_or(
                self.context.i64_type().const_int(0, false).into()
            ))
        } else {
            Err(format!("undefined function '{}' in codegen", mangled))
        }
    }

    /// Find a FuncDef by name from the codegen's stored func_defs
    fn find_func_def(&self, name: &str) -> Result<FuncDef, String> {
        self.func_defs.get(name)
            .cloned()
            .ok_or_else(|| format!("function '{}' definition not available for monomorphization", name))
    }

    /// Extract a raw C string pointer (i8*) from a Mimi string argument.
    /// Mimi strings are represented as either:
    ///   - An i8* raw C string (from string literals)
    ///   - A {i8*, i64} struct (from string variables)
    pub(super) fn extract_raw_str_ptr(&self, arg: &BasicMetadataValueEnum<'ctx>) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => {
                // Could be a raw C string pointer OR a pointer to a Mimi string struct {i8*, i64}.
                // Try to detect: if it points to a struct with ptr+len, load field 0.
                // For now, assume it's a raw C string pointer (string literal case).
                // String variables may produce pointer-to-struct — handle below.
                Ok(*pv)
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let extracted = self.builder.build_extract_value(*sv, 0, "str_ptr")
                    .map_err(|e| format!("extract str ptr error: {}", e))?;
                match extracted {
                    BasicValueEnum::PointerValue(pv) => Ok(pv),
                    _ => Err("[E0712] string struct field 0 is not a pointer".into()),
                }
            }
            _ => Err("[E0712] expected a string argument".into()),
        }
    }

    /// Return an error if running in no_std mode for a builtin that depends on libc.
    pub(super) fn require_std(&self, builtin: &str) -> Result<(), String> {
        if self.no_std {
            Err(format!("[E0750] '{}' requires libc (not available in no_std mode)", builtin))
        } else {
            Ok(())
        }
    }

}
