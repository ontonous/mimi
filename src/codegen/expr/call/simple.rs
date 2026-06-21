use crate::ast::*;
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
                    "type_name" | "type_fields" | "type_variants" | "keys" | "values"
                    | "map" | "filter" | "reduce" => {
                        return self.compile_builtin_intrinsic(name, args, vars);
                    }
                    _ => {}
                }
                // Check if this is a closure variable call
                if let Some(&(alloca, ty)) = vars.get(name.as_str()) {
                    if let BasicTypeEnum::StructType(st) = ty {
                        if st.get_field_types().len() == 2 {
                            // Closure struct {fn_ptr, env_ptr} — do indirect call
                            let closure_val = self.builder.build_load(
                                BasicTypeEnum::StructType(st), alloca,
                                &format!("{}_closure", name),
                            ).map_err(|e| CompileError::LlvmError(format!("load closure error: {}", e)))?;
                            let closure_struct = closure_val.into_struct_value();
                            let fn_ptr = self.builder.build_extract_value(closure_struct, 0, "fn_ptr")
                                .map_err(|e| CompileError::LlvmError(format!("extract fn_ptr error: {}", e)))?
                                .into_pointer_value();
                            let env_ptr = self.builder.build_extract_value(closure_struct, 1, "env_ptr")
                                .map_err(|e| CompileError::LlvmError(format!("extract env_ptr error: {}", e)))?
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
                                    BasicValueEnum::ScalableVectorValue(_) => BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                                });
                            }
                            let ret_type = self.context.i64_type();
                            let indirect_fn_type = ret_type.fn_type(&all_meta, false);
                            let fn_ptr_typed = self.builder.build_pointer_cast(
                                fn_ptr,
                                indirect_fn_type.ptr_type(inkwell::AddressSpace::default()),
                                "fn_typed",
                            ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                            let mut call_args = vec![BasicMetadataValueEnum::PointerValue(env_ptr)];
                            for arg in &compiled_args {
                                call_args.push(match arg {
                                    BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
                                    BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
                                    BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
                                    BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
                                    BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
                                    BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
                                    BasicValueEnum::ScalableVectorValue(_) => BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                                });
                            }
                            let call = self.builder.build_indirect_call(
                                indirect_fn_type, fn_ptr_typed, &call_args, "closure_call",
                            ).map_err(|e| CompileError::LlvmError(format!("closure call error: {}", e)))?;
                            return Ok(call_try_basic_value(&call).unwrap_or(
                                self.context.i64_type().const_int(0, false).into()
                            ));
                        }
                    }
                }
                self.compile_call(name, args, vars)
            }
            Expr::Field(obj, method_name) => {
                self.compile_method_call(obj, method_name, args, vars)
            }
            _ => Err("only direct function calls and method calls supported in codegen".into()),
        }
    }
    pub(in crate::codegen) fn compile_call_fn_ref(
        &mut self,
        fn_ref: BasicValueEnum<'ctx>,
        arg_expr: &Expr,
        payload: BasicValueEnum<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match fn_ref {
            BasicValueEnum::StructValue(sv) => {
                let fn_ptr = self.builder.build_extract_value(sv, 0, "fn_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract fn_ptr error: {}", e)))?.into_pointer_value();
                let env_ptr = self.builder.build_extract_value(sv, 1, "env_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract env_ptr error: {}", e)))?.into_pointer_value();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let fn_type = i64_ty.fn_type(&[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                ], false);
                let fn_typed = self.builder.build_pointer_cast(
                    fn_ptr, fn_type.ptr_type(inkwell::AddressSpace::default()), "fn_typed"
                ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                let call = self.builder.build_indirect_call(
                    fn_type, fn_typed, &[
                        BasicMetadataValueEnum::PointerValue(env_ptr),
                        BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                    ], "fn_call"
                ).map_err(|e| CompileError::LlvmError(format!("indirect call error: {}", e)))?;
                Ok(call_try_basic_value(&call).unwrap_or(
                    BasicValueEnum::IntValue(i64_ty.const_int(0, false))
                ))
            }
            BasicValueEnum::PointerValue(pv) => {
                if let Expr::Ident(fn_name) = arg_expr {
                    if let Some(func) = self.module.get_function(fn_name) {
                        let call = self.builder.build_call(func, &[
                            BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                        ], "fn_call")
                            .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
                        return Ok(call_try_basic_value(&call).unwrap_or(
                            BasicValueEnum::IntValue(i64_ty.const_int(0, false))
                        ));
                    }
                }
                let closure_struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(self.context.i8_type().ptr_type(inkwell::AddressSpace::default())),
                    BasicTypeEnum::PointerType(self.context.i8_type().ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let loaded = self.builder.build_load(BasicTypeEnum::StructType(closure_struct_ty), pv, "closure_loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load closure error: {}", e)))?.into_struct_value();
                let fn_ptr = self.builder.build_extract_value(loaded, 0, "fn_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract fn_ptr error: {}", e)))?.into_pointer_value();
                let env_ptr = self.builder.build_extract_value(loaded, 1, "env_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract env_ptr error: {}", e)))?.into_pointer_value();
                let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let fn_type = i64_ty.fn_type(&[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                ], false);
                let fn_typed = self.builder.build_pointer_cast(
                    fn_ptr, fn_type.ptr_type(inkwell::AddressSpace::default()), "fn_typed"
                ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                let call = self.builder.build_indirect_call(
                    fn_type, fn_typed, &[
                        BasicMetadataValueEnum::PointerValue(env_ptr),
                        BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                    ], "fn_call"
                ).map_err(|e| CompileError::LlvmError(format!("indirect call error: {}", e)))?;
                Ok(call_try_basic_value(&call).unwrap_or(
                    BasicValueEnum::IntValue(i64_ty.const_int(0, false))
                ))
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
                                .map_err(|e| CompileError::LlvmError(format!("extract fn_ptr: {}", e)))?;
                            let env_ptr = self.builder.build_extract_value(*sv, 1, "cb_env_ptr")
                                .map_err(|e| CompileError::LlvmError(format!("extract env_ptr: {}", e)))?;
                            let cb_fn_ptr = fn_ptr.into_pointer_value();
                            let cb_env_ptr = env_ptr.into_pointer_value();
                            let thunk_entry = self.get_or_create_callback_thunk(cb_params, cb_ret)
                                .map_err(|e| CompileError::LlvmError(format!("callback thunk: {}", e)))?;
                            self.builder.build_store(
                                thunk_entry.fn_ptr_global.as_pointer_value(), cb_fn_ptr,
                            ).map_err(|e| CompileError::LlvmError(format!("store fn_ptr: {}", e)))?;
                            self.builder.build_store(
                                thunk_entry.env_ptr_global.as_pointer_value(), cb_env_ptr,
                            ).map_err(|e| CompileError::LlvmError(format!("store env_ptr: {}", e)))?;
                            let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                            let thunk_ptr = thunk_entry.thunk_fn.as_global_value().as_pointer_value();
                            let casted = self.builder.build_pointer_cast(thunk_ptr, i8_ptr_ty, "thunk_i8")
                                .map_err(|e| CompileError::LlvmError(format!("bitcast thunk: {}", e)))?;
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
                            BasicValueEnum::ScalableVectorValue(_) => BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
            }
        }).collect();

        // Dispatch builtins
        if name == "len" && args.len() == 1 {
            self.pending_len_is_string = self.expr_is_string(&args[0]);
        }
        if crate::codegen::builtins::is_builtin(name) {
            return self.compile_builtin_call(name, &metadata_args).map_err(|e| CompileError::Generic(e.to_string()));
        }

        // Handle built-in Option/Result constructors
        match name {
            "Ok" | "Some" | "Err" | "None" => return self.compile_constructor(name, compiled_args),
            _ => {}
        }

        if let Some(function) = self.module.get_function(name) {
            let call = self.builder.build_call(function, &metadata_args, "call")
                .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
            Ok(call_try_basic_value(&call).unwrap_or(
                self.context.i64_type().const_int(0, false).into()
            ))
        } else {
            // Not found by direct name — must be a generic function.
            // Build a callee-specific type_map by inferring generic bindings
            // from the argument types at the call site, instead of using the
            // caller's type_map (which has different generic param names).
            let mangled = if let Some(fdef) = self.func_defs.get(name) {
                if !fdef.generics.is_empty() {
                    let mut callee_map: HashMap<String, Type> = HashMap::new();
                    for gp in &fdef.generics {
                        // Find the first callee param whose type references this generic
                        for (i, param) in fdef.params.iter().enumerate() {
                            if i < args.len() && Self::type_references_generic(&param.ty, &gp.name) {
                                if let Some(arg_type) = self.expr_type_of(&args[i], vars) {
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
                let call = self.builder.build_call(function, &metadata_args, "call")
                    .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
                Ok(call_try_basic_value(&call).unwrap_or(
                    self.context.i64_type().const_int(0, false).into()
                ))
            } else {
                let msg = if self.comptime_func_names.contains(name) {
                    format!("comptime function '{}' is compile-time only and cannot be called from runtime code", name)
                } else {
                    format!("undefined function '{}' in codegen", name)
                };
                Err(msg.into())
            }
        }
    }
    /// Call a function by its mangled name
    pub(in crate::codegen) fn compile_call_mangled(
        &mut self,
        mangled: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
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
                            BasicValueEnum::ScalableVectorValue(_) => BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
            }
        }).collect();

        if let Some(function) = self.module.get_function(mangled) {
            let call = self.builder.build_call(function, &metadata_args, "call")
                .map_err(|e| CompileError::LlvmError(format!("call error: {}", e)))?;
            Ok(call_try_basic_value(&call).unwrap_or(
                self.context.i64_type().const_int(0, false).into()
            ))
        } else {
            let msg = if self.comptime_func_names.contains(mangled) {
                format!("comptime function '{}' is compile-time only and cannot be called from runtime code", mangled)
            } else {
                format!("undefined function '{}' in codegen", mangled)
            };
            Err(msg.into())
        }
    }
    /// Find a FuncDef by name from the codegen's stored func_defs
    pub(in crate::codegen) fn find_func_def(&self, name: &str) -> Result<FuncDef, CompileError> {
        self.func_defs.get(name)
            .cloned()
            .ok_or_else(|| CompileError::Generic(format!("function '{}' definition not available for monomorphization", name)))
    }
}
