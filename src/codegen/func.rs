use crate::ast::*;
use crate::codegen::types;
use std::collections::HashMap;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::AddressSpace;

use crate::codegen::CallSiteValueExt;
use crate::error::{CompileError, MimiResult};

/// Recursively collect all Stmt::Ensures from a list of statements,
/// descending into nested blocks (if, while, for, parasteps, lambda, expr block).
fn collect_ensures(stmts: &[Stmt]) -> Vec<Expr> {
    let mut result = Vec::new();
    for s in stmts {
        match s {
            Stmt::Ensures(expr, _) => result.push(expr.clone()),
            Stmt::If { then_, else_, .. } => {
                result.extend(collect_ensures(then_));
                if let Some(eb) = else_ {
                    result.extend(collect_ensures(eb));
                }
            }
            Stmt::While { body, .. } => result.extend(collect_ensures(body)),
            Stmt::Loop(body) => result.extend(collect_ensures(body)),
            Stmt::For { body, .. } => result.extend(collect_ensures(body)),
            Stmt::Parasteps(body) => result.extend(collect_ensures(body)),
            Stmt::Expr(Expr::Lambda { body, .. }) => result.extend(collect_ensures(body)),
            Stmt::Expr(Expr::Block(body)) => result.extend(collect_ensures(body)),
            Stmt::Return(Some(Expr::Block(body))) => result.extend(collect_ensures(body)),
            _ => {}
        }
    }
    result
}

use super::CodeGenerator;
use super::VarEntry;

// Submodules for clearly independent method groups. The originally suggested
// groups (params, actor, shared) do not map to standalone methods in this file:
//
// - Parameter handling and ABI layout are inlined in `compile_func` / `compile_generic_func`;
//   there is no `compile_param` helper to extract without restructuring logic.
// - Actor constructor / method compilation already lives in `codegen/actors.rs`.
// - Shared / RC scope cleanup helpers already live in `codegen/scope.rs` and `codegen/mod.rs`.
//
// What was split out:
// - `func/body.rs`: statement-level body helpers (loops and assignment forms).
// - `func/pattern.rs`: recursive `compile_pattern_bind`.
mod body;
mod pattern;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_async_func(&mut self, func: &FuncDef) -> MimiResult<()> {
        // 1. Compile the actual body as a hidden regular function
        let body_name = format!("{}__async_body", func.name);
        let body_func = FuncDef {
            name: body_name.clone(),
            pub_: false,
            params: func.params.clone(),
            ret: func.ret.clone(),
            body: func.body.clone(),
            where_clause: None,
            generics: vec![],
            effects: vec![],
            is_comptime: false,
            is_async: false,
            extern_abi: None,
            pos: (0, 0),
        };
        self.compile_func(&body_func)?;

        let result_ty = func.ret.as_ref()
            .and_then(|t| self.llvm_type_for(t))
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
        let result_size = self.llvm_type_size_bytes(result_ty);
        let aligned_result = result_size.max(8);

        // Determine param types and sizes
        let mut param_types = Vec::new();
        let mut param_sizes: Vec<u64> = Vec::new();
        for param in &func.params {
            if let Some(ty) = self.llvm_type_for(&param.ty) {
                param_types.push(ty);
                param_sizes.push(self.llvm_type_size_bytes(ty));
            }
        }
        let total_args_size: u64 = param_sizes.iter().sum();
        // total allocation: 8 header + aligned_result (result) + total_args_size (args)
        let total_alloc_size = 8 + aligned_result + total_args_size;
        let args_offset: u64 = 8 + aligned_result;

        // i8 pointer type
        let i8_ty = self.context.i8_type();
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // ── Step 2a: Generate poll function ──
        // void @foo_poll(i8* %future_ptr)
        let poll_name = format!("{}__poll", func.name);
        let poll_fn_type = i8_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr_ty)], false);
        let poll_fn = self.module.add_function(&poll_name, poll_fn_type,
            Some(inkwell::module::Linkage::Internal));
        let poll_entry = self.context.append_basic_block(poll_fn, "entry");
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(poll_entry);

        let poll_future_ptr = poll_fn.get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("poll_fn: param 0 not found".into()))?
            .into_pointer_value();

        // Load args from future + args_offset and call body
        let body_fn = self.module.get_function(&body_name)
            .ok_or_else(|| CompileError::LlvmError(format!("body fn '{}' not found", body_name)))?;
        let mut poll_call_args = Vec::new();
        let mut current_arg_offset = args_offset;
        for (param_idx, _param) in func.params.iter().enumerate() {
            if param_idx < param_types.len() {
                let ty = param_types[param_idx];
                let size = param_sizes[param_idx];
                // GEP to load arg: future + current_arg_offset
                let arg_ptr_i8 = unsafe {
                    self.builder.build_gep(i8_ty, poll_future_ptr,
                        &[i64_ty.const_int(current_arg_offset, false)],
                        &format!("poll_arg_{}", param_idx))
                        .map_err(|e| CompileError::LlvmError(format!("poll arg gep: {}", e)))?
                };
                let arg_typed_ptr = self.builder.build_pointer_cast(
                    arg_ptr_i8,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    &format!("poll_arg_typed_{}", param_idx),
                ).map_err(|e| CompileError::LlvmError(format!("poll arg cast: {}", e)))?;
                let arg_val = self.builder.build_load(ty, arg_typed_ptr,
                    &format!("poll_arg_val_{}", param_idx))
                    .map_err(|e| CompileError::LlvmError(format!("poll arg load: {}", e)))?;
                poll_call_args.push(match arg_val {
                    BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
                    BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(fv),
                    BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(pv),
                    BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(sv),
                    BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(av),
                    BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(vv),
                    BasicValueEnum::ScalableVectorValue(svv) => BasicMetadataValueEnum::ScalableVectorValue(svv),
                });
                current_arg_offset += size;
            }
        }

        let poll_body_result = self.builder.build_call(body_fn, &poll_call_args, "poll_body_call")
            .map_err(|e| CompileError::LlvmError(format!("poll body call: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("poll body returned void".into()))?;

        // Store result at future + 8
        if !func.ret.as_ref().map_or(true, |t| matches!(t, Type::Name(n, _) if n == "unit")) {
            let result_ptr_i8 = unsafe {
                self.builder.build_gep(i8_ty, poll_future_ptr,
                    &[i64_ty.const_int(8, false)], "poll_result_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("poll result gep: {}", e)))?
            };
            let result_typed_ptr = self.builder.build_pointer_cast(
                result_ptr_i8,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "poll_result_typed",
            ).map_err(|e| CompileError::LlvmError(format!("poll result cast: {}", e)))?;
            self.builder.build_store(result_typed_ptr, poll_body_result)
                .map_err(|e| CompileError::LlvmError(format!("poll result store: {}", e)))?;
        }

        // Set completed
        let set_c_fn = self.module.get_function("mimi_future_set_completed")
            .ok_or_else(|| CompileError::LlvmError("mimi_future_set_completed not declared".into()))?;
        self.builder.build_call(set_c_fn, &[
            BasicMetadataValueEnum::PointerValue(poll_future_ptr),
        ], "poll_set_completed")
            .map_err(|e| CompileError::LlvmError(format!("poll set completed: {}", e)))?;

        self.builder.build_return(None)
            .map_err(|e| CompileError::LlvmError(format!("poll return: {}", e)))?;

        // Restore insertion point
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        // ── Step 2b: Generate async constructor function ──
        // foo(args...) -> i8*  (returns future pointer, submitted to executor)
        let metadata_params: Vec<_> = param_types.iter()
            .map(|t| types::basic_to_metadata(self.context, *t))
            .collect();

        let fn_type = i8_ptr_ty.fn_type(&metadata_params, false);
        let function = self.module.add_function(&func.name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if i < param_types.len() {
                let ty = param_types[i];
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let param_val = function.get_nth_param(i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("param {} not found", i)))?;
                self.builder.build_store(alloca, param_val)
                    .map_err(|e| CompileError::LlvmError(format!("store param: {}", e)))?;
                vars.insert(param.name.clone(), (alloca, ty));
                let param_ty_name = match &param.ty {
                    Type::Name(tn, _) => Some(tn.clone()),
                    _ => None,
                };
                if let Some(ref tn) = param_ty_name {
                    self.var_type_names.insert(param.name.clone(), tn.clone());
                }
                // Register list element type for List<T> params where T is a struct
                self.register_list_elem_type(&param.name, &param.ty);
            }
        }

        // Allocate future: call mimi_future_alloc(total_size)
        let alloc_fn = self.module.get_function("mimi_future_alloc")
            .ok_or_else(|| CompileError::LlvmError("mimi_future_alloc not declared".into()))?;
        let total_size_val = i64_ty.const_int(total_alloc_size, false);
        let future_ptr = self.builder.build_call(alloc_fn, &[
            BasicMetadataValueEnum::IntValue(total_size_val),
        ], "future_alloc")
            .map_err(|e| CompileError::LlvmError(format!("future_alloc error: {}", e)))?
            .try_as_basic_value_opt()
            .map(|v: BasicValueEnum<'ctx>| v.into_pointer_value())
            .ok_or_else(|| CompileError::LlvmError("future_alloc returned non-pointer".into()))?;

        // Store args in future at args_offset
        let mut current_arg_store_offset = args_offset;
        for (param_idx, param) in func.params.iter().enumerate() {
            if param_idx < param_types.len() {
                let ty = param_types[param_idx];
                let size = param_sizes[param_idx];
                let alloca = vars.get(&param.name)
                    .ok_or_else(|| CompileError::LlvmError(format!("var '{}' not found", param.name)))?;
                let val = self.builder.build_load(ty, alloca.0, &format!("store_{}", param.name))
                    .map_err(|e| CompileError::LlvmError(format!("load for store: {}", e)))?;
                let arg_slot_i8 = unsafe {
                    self.builder.build_gep(i8_ty, future_ptr,
                        &[i64_ty.const_int(current_arg_store_offset, false)],
                        &format!("arg_slot_{}", param_idx))
                        .map_err(|e| CompileError::LlvmError(format!("arg slot gep: {}", e)))?
                };
                let arg_slot_typed = self.builder.build_pointer_cast(
                    arg_slot_i8,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    &format!("arg_slot_typed_{}", param_idx),
                ).map_err(|e| CompileError::LlvmError(format!("arg slot cast: {}", e)))?;
                self.builder.build_store(arg_slot_typed, val)
                    .map_err(|e| CompileError::LlvmError(format!("arg store: {}", e)))?;
                current_arg_store_offset += size;
            }
        }

        // Call mimi_executor_spawn(future, poll_fn)
        let spawn_fn = self.module.get_function("mimi_executor_spawn")
            .ok_or_else(|| CompileError::LlvmError("mimi_executor_spawn not declared".into()))?;
        let poll_fn_as_i8 = self.builder.build_pointer_cast(
            poll_fn.as_global_value().as_pointer_value(),
            i8_ptr_ty,
            "poll_fn_i8",
        ).map_err(|e| CompileError::LlvmError(format!("poll fn cast: {}", e)))?;
        self.builder.build_call(spawn_fn, &[
            BasicMetadataValueEnum::PointerValue(future_ptr),
            BasicMetadataValueEnum::PointerValue(poll_fn_as_i8),
        ], "executor_spawn")
            .map_err(|e| CompileError::LlvmError(format!("executor_spawn error: {}", e)))?;

        // Return the future pointer
        self.builder.build_return(Some(&BasicValueEnum::PointerValue(future_ptr)))
            .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;

        Ok(())
    }

    /// For a function returning `impl Trait`, extract the concrete return type
    /// from the function body (e.g., a record literal's type annotation).
    fn concrete_return_type_for_impl_trait(body: &[Stmt]) -> Option<String> {
        let last = body.last()?;
        match last {
            Stmt::Expr(expr) | Stmt::Return(Some(expr)) => match expr {
                Expr::Record { ty, .. } => ty.clone(),
                Expr::Call(callee, _) => {
                    if let Expr::Ident(_fname) = callee.as_ref() {
                        None
                    } else {
                        None
                    }
                }
                Expr::Block(block) => Self::concrete_return_type_for_impl_trait(block),
                _ => None,
            },
            Stmt::If { cond: _, then_, else_ } => {
                let then_ty = Self::concrete_return_type_for_impl_trait(then_);
                if then_ty.is_some() {
                    then_ty
                } else {
                    else_.as_ref()
                        .and_then(|el| Self::concrete_return_type_for_impl_trait(el))
                }
            }
            Stmt::Block(block) => Self::concrete_return_type_for_impl_trait(block),
            _ => None,
        }
    }

    pub(super) fn compile_func(&mut self, func: &FuncDef) -> MimiResult<()> {
        // Delegate async funcs to compile_async_func
        if func.is_async {
            return self.compile_async_func(func);
        }

        // For impl Trait return types, determine the concrete type from the body
        // so the function's LLVM signature uses the right type.
        let effective_ret_override = if let Some(Type::ImplTrait(_)) = &func.ret {
            Self::concrete_return_type_for_impl_trait(&func.body)
                .and_then(|tn| self.type_llvm.get(&tn).cloned())
        } else {
            None
        };

        let ret_type = effective_ret_override.or_else(|| {
            match &func.ret {
                Some(ty) => self.llvm_type_for(ty),
                None => None,
            }
        }).unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let mut param_types = Vec::new();
        for param in &func.params {
            if let Some(ty) = self.llvm_type_for(&param.ty) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let linkage = if func.extern_abi.is_some() {
            Some(inkwell::module::Linkage::External)
        } else {
            None
        };
        let function = self.module.add_function(&func.name, fn_type, linkage);
        // Set calling convention for extern "C" / extern "stdcall" etc.
        if let Some(ref abi) = func.extern_abi {
            let cc = crate::ffi::abi_to_llvm_call_conv(abi);
            function.set_call_conventions(cc);
        }
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // Push scopes for function body
        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if let Some(ty) = self.llvm_type_for(&param.ty) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).ok_or_else(|| CompileError::LlvmError(format!("param index {} out of range for function '{}' with {} params", i, func.name, function.count_params())))?)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                vars.insert(param.name.clone(), (alloca, ty));
                
                // Track type name for method dispatch
                if let Type::Name(tn, _) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), tn.clone());
                }
                if let Type::DynTrait(_) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&param.ty));
                }
                if let Type::ImplTrait(_) = &param.ty {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&param.ty));
                }
                
                // Register list element type for List<T> params where T is a struct
                self.register_list_elem_type(&param.name, &param.ty);
                
                // Track capability parameters
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }
        
        // Collect ensures contracts for runtime checking at return points.
        // Recursively walks nested blocks (if, while, for, parasteps, lambda).
        self.ensures_stmts = if self.verify_contracts {
            collect_ensures(&func.body)
        } else {
            Vec::new()
        };

        // Snapshot parameters for old() in ensures contracts.
        // At function entry, copy each parameter to an old-snapshot alloca so that
        // old(x) in postconditions reads the entry-time value, not the current value.
        self.old_snapshots.clear();
        if !self.ensures_stmts.is_empty() {
            let snap_vars: Vec<(String, inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)> = vars.iter()
                .map(|(name, &(alloca, ty))| (name.clone(), alloca, ty))
                .collect();
            for (name, alloca, ty) in snap_vars {
                let old_alloca = self.builder.build_alloca(ty, &format!("{}_old", name))
                    .map_err(|e| CompileError::LlvmError(format!("old snapshot alloca: {}", e)))?;
                let val = self.builder.build_load(ty, alloca, &format!("{}_snap", name))
                    .map_err(|e| CompileError::LlvmError(format!("old snapshot load: {}", e)))?;
                self.builder.build_store(old_alloca, val)
                    .map_err(|e| CompileError::LlvmError(format!("old snapshot store: {}", e)))?;
                self.old_snapshots.insert(name, (old_alloca, ty));
            }
        }

        // Compile requires contracts as runtime asserts when verify_contracts is enabled
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    self.compile_contract_assert(expr, &vars, &format!("requires violation in '{}'", func.name))?;
                }
            }
        }

        let default_val = match ret_type {
            BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
            BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
            _ => self.context.i64_type().const_int(0, false).into(),
        };
        let mut last_val: BasicValueEnum = default_val;
        for stmt in &func.body {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(&mut vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    last_val = self.compile_expr(expr, &vars)?;
                    last_val = self.adjust_int_val(last_val, ret_type)?;
                }
                Stmt::Return(Some(expr)) => {
                    let val = self.compile_expr(expr, &vars)?;
                    let val = self.adjust_int_val(val, ret_type)?;
                    let ensures = self.ensures_stmts.clone();
                    if !ensures.is_empty() {
                        let result_alloca = self.builder.build_alloca(ret_type, "result")
                            .map_err(|e| CompileError::LlvmError(format!("result alloca: {}", e)))?;
                        self.builder.build_store(result_alloca, val)
                            .map_err(|e| CompileError::LlvmError(format!("result store: {}", e)))?;
                        let mut ensures_vars = vars.clone();
                        ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
                        for ensures_expr in &ensures {
                            self.compile_contract_assert(ensures_expr, &ensures_vars, &format!("ensures violation in '{}'", func.name))?;
                        }
                    }
                    self.pop_shared_scope()?;
                    self.free_heap_allocs()?;
                    self.pop_comp_scope();
                    self.pop_cap_scope();
                    self.builder.build_return(Some(&val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Return(None) => {
                    let ensures = self.ensures_stmts.clone();
                    if !ensures.is_empty() {
                        let result_alloca = self.builder.build_alloca(ret_type, "result")
                            .map_err(|e| CompileError::LlvmError(format!("result alloca: {}", e)))?;
                        self.builder.build_store(result_alloca, self.context.i64_type().const_int(0, false))
                            .map_err(|e| CompileError::LlvmError(format!("result store: {}", e)))?;
                        let mut ensures_vars = vars.clone();
                        ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
                        for ensures_expr in &ensures {
                            self.compile_contract_assert(ensures_expr, &ensures_vars, &format!("ensures violation in '{}'", func.name))?;
                        }
                    }
                    self.pop_shared_scope()?;
                    self.free_heap_allocs()?;
                    self.pop_comp_scope();
                    self.pop_cap_scope();
                    self.builder.build_return(None).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
                    return Ok(());
                }
                Stmt::Let { pat, init: Some(init), ty, .. } => {
                    // dyn Trait let-binding: build fat pointer from concrete value (requires Variable pattern)
                    if let Some(Type::DynTrait(trait_names)) = &ty {
                        let name = match pat {
                            Pattern::Variable(n) => n.clone(),
                            _ => return Err(CompileError::LlvmError(
                                "dyn Trait binding requires a simple variable pattern".to_string()
                            )),
                        };
                        let concrete_val = self.compile_expr(init, &vars)?;
                        let concrete_type = match init {
                            Expr::Record { ty: Some(tn), .. } => tn.clone(),
                            Expr::Ident(var_name) => self.var_type_names.get(var_name).cloned().unwrap_or_default(),
                            _ => {
                                return Err(CompileError::LlvmError(
                                    format!("cannot infer concrete type for dyn Trait binding '{}'", name)
                                ));
                            }
                        };
                        if concrete_type.is_empty() {
                            return Err(CompileError::LlvmError(
                                format!("cannot infer concrete type for dyn Trait binding '{}'", name)
                            ));
                        }
                        let trait_name = &trait_names[0];
                        let concrete_ty = self.type_llvm.get(&concrete_type)
                            .cloned()
                            .unwrap_or_else(|| concrete_val.get_type());
                        let data_alloca = self.builder.build_alloca(concrete_ty, &format!("{}_data", name))
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        self.builder.build_store(data_alloca, concrete_val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let data_ptr = self.builder.build_pointer_cast(
                            data_alloca, i8_ptr, &format!("{}_data_i8", name)
                        ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                        let vtable_key = format!("{}__{}", concrete_type, trait_name);
                        let vtable_gv = self.vtable_globals.get(&vtable_key)
                            .ok_or_else(|| CompileError::LlvmError(
                                format!("no vtable for {}.{}", concrete_type, trait_name)
                            ))?;
                        let vtable_ptr = self.builder.build_pointer_cast(
                            vtable_gv.as_pointer_value(), i8_ptr,
                            &format!("{}_vtable_i8", name)
                        ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                        let fat_ty = BasicTypeEnum::StructType(
                            self.context.struct_type(&[
                                BasicTypeEnum::PointerType(i8_ptr),
                                BasicTypeEnum::PointerType(i8_ptr),
                            ], false)
                        );
                        let fat_alloca = self.builder.build_alloca(fat_ty, &name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        let data_gep = self.gep().build_struct_gep(fat_ty, fat_alloca, 0, &format!("{}_data_gep", name))
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(data_gep, data_ptr)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let vtable_gep = self.gep().build_struct_gep(fat_ty, fat_alloca, 1, &format!("{}_vtable_gep", name))
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.builder.build_store(vtable_gep, vtable_ptr)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        let ty_ref = ty.as_ref().ok_or_else(|| CompileError::LlvmError(format!("missing type for variable '{}'", name)))?;
                        let dyn_type_str = crate::core::fmt_type(ty_ref);
                        self.var_type_names.insert(name.clone(), dyn_type_str);
                        vars.insert(name.clone(), (fat_alloca, fat_ty));
                        if let Some(Type::Cap(_)) = &ty {
                            self.register_cap(&name, fat_alloca);
                        }
                        continue;
                    }
                    // Shared ref copy: let v = shared_var
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Ident(src_name) = init {
                            if self.shared_var_names.contains(src_name.as_str()) {
                                self.compile_shared_ref_copy(name, src_name, &mut vars)?;
                                continue;
                            }
                        }
                    }
                    // Shared var clone: let v = shared_var.clone()
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Call(callee, cargs) = init {
                            if cargs.is_empty() {
                                if let Expr::Field(obj, method_name) = callee.as_ref() {
                                    if method_name == "clone" {
                                        if let Expr::Ident(src_name) = obj.as_ref() {
                                            if self.shared_var_names.contains(src_name.as_str()) {
                                                self.compile_shared_ref_copy(name, src_name, &mut vars)?;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Non-dyn Trait: compile init and bind via recursive pattern matching
                    let mut val = self.compile_expr(init, &vars)?;
                    if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        val = self.adjust_int_val(val, target)?;
                    }
                    // Track type info for simple Variable patterns
                    if let Pattern::Variable(name) = pat {
                        if let Some(Type::Name(tn, _)) = &ty {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if self.expr_is_string(init) {
                            self.var_type_names.insert(name.clone(), "string".to_string());
                        } else if let Expr::Record { ty: Some(tn), .. } = init {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if matches!(init, Expr::SetLiteral(_)) {
                            self.var_type_names.insert(name.clone(), "set".to_string());
                        } else if let Expr::Call(callee, _) = init {
                            if let Expr::Field(obj, method_name) = callee.as_ref() {
                                if method_name == "spawn" {
                                    let obj_type = self.infer_object_type(obj, &vars);
                                    if !obj_type.is_empty() {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(method_name.as_str(), "map" | "and_then" | "map_err" | "ok_or") {
                                    let obj_type = self.infer_object_type(obj, &vars);
                                    if obj_type == "Result" || obj_type == "Option" {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                }
                            } else if let Expr::Ident(func_name) = callee.as_ref() {
                                match func_name.as_str() {
                                    "Ok" | "Err" => {
                                        self.var_type_names.insert(name.clone(), "Result".to_string());
                                    }
                                    "Some" | "None" => {
                                        self.var_type_names.insert(name.clone(), "Option".to_string());
                                    }
                                    _ => {
                                        if let Some(fdef) = self.func_defs.get(func_name) {
                                            if let Some(ret_ty) = &fdef.ret {
                                                match ret_ty {
                                                    Type::ImplTrait(traits) => {
                                                        self.var_type_names.insert(
                                                            name.clone(),
                                                            format!("impl {}", traits.join(" + ")),
                                                        );
                                                    }
                                                    Type::Name(tn, _) => {
                                                        self.var_type_names.insert(name.clone(), tn.clone());
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Track list element type for nested List<List<T>> indexing
                        if let Some(decl_ty) = &ty {
                            self.register_list_elem_type(name, decl_ty);
                        }
                        // Track capability variables
                        if let Some(Type::Cap(_)) = &ty {
                            if let Some(&(alloca, _)) = vars.get(name) {
                                self.register_cap(name, alloca);
                            }
                        }
                    }
                    self.compile_pattern_bind(pat, val, &mut vars)?;
                    if let Pattern::Variable(name) = pat {
                        if let Expr::Ident(fn_name) = init {
                            if self.module.get_function(fn_name.as_str()).is_some() {
                                self.fn_ptr_var_names.insert(name.clone());
                            }
                            if self.cap_type_names.contains(fn_name.as_str()) {
                                self.var_type_names.insert(name.clone(), fn_name.clone());
                            }
                        }
                    }
                }
                Stmt::Assign { target, value } => {
                    self.compile_assign_stmt(target, value, &mut vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, &vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        let fn_name = function.get_name().to_str().unwrap_or("unknown");
                        return Err(CompileError::TypeMismatch(
                            format!("if condition must be bool, got {} in function '{}'", cond_val.get_type(), fn_name)
                        ));
                    };

                    let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for if".to_string()))?;
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                    // Then block
                    let then_val = {
                        self.builder.position_at_end(then_bb);
                        let mut then_vars = vars.clone();
                        let v = self.compile_block_last_val(then_, &mut then_vars)?;
                        let current = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block for then block".to_string()))?;
                        if current.get_terminator().is_none() {
                            self.builder.build_unconditional_branch(merge_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                        v
                    };
                    let then_bb_end = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block after then".to_string()))?;

                    // Else block
                    let else_val = {
                        self.builder.position_at_end(else_bb);
                        if let Some(else_block) = else_ {
                            let mut else_vars = vars.clone();
                            let v = self.compile_block_last_val(else_block, &mut else_vars)?;
                            let current = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block for else block".to_string()))?;
                            if current.get_terminator().is_none() {
                                self.builder.build_unconditional_branch(merge_bb)
                                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                            }
                            v
                        } else {
                            self.context.i64_type().const_int(0, false).into()
                        }
                    };
                    let else_bb_end = self.builder.get_insert_block().ok_or_else(|| CompileError::LlvmError("codegen: no insert block after else".to_string()))?;
                    // No-else case: else_bb has no terminator yet — supply one
                    if else_bb_end.get_terminator().is_none() {
                        self.builder.build_unconditional_branch(merge_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    }

                    // Continue at merge, produce phi if both branches have values
                    self.builder.position_at_end(merge_bb);
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self.builder.build_phi(then_val.get_type(), "if_result")
                            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                        phi.add_incoming(&[
                            (&then_val as &dyn inkwell::values::BasicValue, then_bb_end),
                            (&else_val as &dyn inkwell::values::BasicValue, else_bb_end),
                        ]);
                        last_val = phi.as_basic_value();
                    }
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, &mut vars)?;
                }
                Stmt::Loop(body) => {
                    self.compile_loop_stmt(body, &mut vars)?;
                }
                Stmt::For { var, iterable, body } => {
                    self.compile_for_stmt(var, iterable, body, &mut vars)?;
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| CompileError::LlvmError(format!("break error: {}", e)))?;
                        // Create unreachable block for subsequent statements
                        let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for break".to_string()))?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::BreakOutsideLoop);
                    }
                }
                Stmt::Continue => {
                    if let Some(target) = self.loop_continue {
                        self.builder.build_unconditional_branch(target)
                            .map_err(|e| CompileError::LlvmError(format!("continue error: {}", e)))?;
                        let function = self.current_function().ok_or_else(|| CompileError::LlvmError("codegen: no current function for continue".to_string()))?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::ContinueOutsideLoop);
                    }
                }
                Stmt::MmsBlock { .. } => {
                    // Skip MMS blocks in codegen (they're for documentation/contracts)
                }
                Stmt::Parasteps(block) => {
                    // Parasteps: execute spawn statements in parallel, join at block end
                    self.enter_parasteps();
                    self.compile_block(block, &mut vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and mark capability as consumed
                    let _val = self.compile_expr(expr, &vars)?;
                    // If the expression is a variable, mark it as consumed and call mimi_cap_consume
                    if let Expr::Ident(name) = expr {
                        self.consume_cap(name)?;
                        // Generate runtime cap consume call
                        if self.is_cap_var(name) {
                            if let Some(consume_fn) = self.module.get_function("mimi_cap_consume") {
                                if let Some(&(alloca, _)) = vars.get(name) {
                                    let cap_val = self.builder.build_load(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        alloca, &format!("cap_val_{}", name))
                                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                                    let name_global = self.builder.build_global_string_ptr(
                                        &format!("{}\0", name), &format!("cap_name_drop_{}", name))
                                        .map_err(|e| CompileError::LlvmError(format!("string global error: {}", e)))?;
                                    let name_ptr = name_global.as_pointer_value();
                                    self.builder.build_call(consume_fn, &[
                                        BasicMetadataValueEnum::IntValue(cap_val.into_int_value()),
                                        BasicMetadataValueEnum::PointerValue(name_ptr),
                                    ], &format!("cap_consume_{}", name))
                                        .map_err(|e| CompileError::LlvmError(format!("cap_consume error: {}", e)))?;
                                }
                            }
                        }
                    }
                }
                Stmt::SharedLet { kind, name, ty, init } => {
                    self.compile_shared_let_stmt(kind, name, ty, init, &mut vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    self.compile_arena_block(block, &mut vars, "arena")?;
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, &mut vars)?;
                }
                Stmt::Alloc { kind: AllocKind::Arena, body } => {
                    self.compile_arena_block(body, &mut vars, "alloc(Arena)")?;
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified - no custom allocator in codegen)
                    self.compile_block(body, &mut vars)?;
                }
                Stmt::Desc(..) | Stmt::Rule(..) | Stmt::Requires(..) | Stmt::Ensures(..) | Stmt::Invariant(..) | Stmt::Math(_) => {
                    // Skip contract-related statements in codegen
                }
                Stmt::Block(block) => {
                    self.compile_block(block, &mut vars)?;
                }
                _ => {}
            }
        }

        // Check for unconsumed capabilities before returning
        self.check_unconsumed_caps()?;
        
        // Convert pointer-to-struct to struct value when return type expects a struct
        // Must happen BEFORE free_heap_allocs to null out heap data pointers in the original struct,
        // preventing use-after-free on the returned value's heap-allocated data.
        let last_val = match (last_val, ret_type) {
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                let loaded = self.builder.build_load(BasicTypeEnum::StructType(st), pv, "ret_load")
                    .map_err(|e| CompileError::LlvmError(format!("load return struct: {}", e)))?;
                // Null out field at index 1 (data pointer) to prevent free_heap_allocs from freeing
                // the heap data that's now owned by the caller via the returned struct value.
                if st.get_field_types().len() > 1 {
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    if let Ok(data_gep) = self.gep().build_struct_gep(st, pv, 1, "ret_data_null") {
                        let _ = self.builder.build_store(data_gep, null_ptr);
                    }
                }
                loaded
            }
            _ => last_val,
        };

        // Pop scopes (discard compensations on normal exit)
        self.release_all_shared()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        self.pop_cap_scope();

        if !self.block_has_terminator() {
            let ensures = self.ensures_stmts.clone();
            if !ensures.is_empty() {
                let result_alloca = self.builder.build_alloca(ret_type, "result")
                    .map_err(|e| CompileError::LlvmError(format!("result alloca: {}", e)))?;
                let adjusted = self.adjust_int_val(last_val, ret_type)?;
                self.builder.build_store(result_alloca, adjusted)
                    .map_err(|e| CompileError::LlvmError(format!("result store: {}", e)))?;
                let mut ensures_vars = vars.clone();
                ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
                for ensures_expr in &ensures {
                    self.compile_contract_assert(ensures_expr, &ensures_vars, &format!("ensures violation in '{}'", func.name))?;
                }
            }
        }
        let last_val = self.adjust_int_val(last_val, ret_type)?;
        self.builder.build_return(Some(&last_val)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        Ok(())
    }

    /// Compile a generic function with concrete type arguments (monomorphization)
    pub(super) fn compile_generic_func(&mut self, func: &FuncDef, type_map: &HashMap<String, crate::ast::Type>) -> MimiResult<()> {
        // Save and set the type_map
        let prev_type_map = self.type_map.clone();
        self.type_map = type_map.clone();

        let mangled = Self::mangle_name(&func.name, type_map);

        // Skip if already compiled
        if self.module.get_function(&mangled).is_some() {
            self.type_map = prev_type_map;
            return Ok(());
        }

        // Delegate async generic funcs to compile_async_func
        if func.is_async {
            return self.compile_async_func(func);
        }

        // For impl Trait return types, determine the concrete type from the body
        let effective_ret_override = if let Some(Type::ImplTrait(_)) = &func.ret {
            Self::concrete_return_type_for_impl_trait(&func.body)
                .and_then(|tn| self.type_llvm.get(&tn).cloned())
        } else {
            None
        };

        // Substitute generic params in ret type and param types
        let ret_type = effective_ret_override.or_else(|| {
            match &func.ret {
                Some(ty) => {
                    let resolved = self.resolve_type(ty);
                    self.llvm_type_for(&resolved)
                }
                None => None,
            }
        }).unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let mut param_types = Vec::new();
        for param in &func.params {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = self.llvm_type_for(&resolved) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types.iter().map(|t| types::basic_to_metadata(self.context, *t)).collect();

        let fn_type = match ret_type {
            BasicTypeEnum::IntType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&metadata_params, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&metadata_params, false),
            _ => self.context.i64_type().fn_type(&metadata_params, false),
        };

        let function = self.module.add_function(&mangled, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();
        self.push_comp_scope();
        self.push_heap_scope();

        let mut vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = self.llvm_type_for(&resolved) {
                let alloca = self.builder.build_alloca(ty, &param.name)
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, function.get_nth_param(i as u32).ok_or_else(|| CompileError::LlvmError("param index matches".to_string()))?)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                vars.insert(param.name.clone(), (alloca, ty));
                
                // Track type name for method dispatch
                let resolved_param = self.resolve_type(&param.ty);
                if let Type::Name(tn, _) = &resolved_param {
                    self.var_type_names.insert(param.name.clone(), tn.clone());
                }
                if let Type::DynTrait(_) = &resolved_param {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&resolved_param));
                }
                if let Type::ImplTrait(_) = &resolved_param {
                    self.var_type_names.insert(param.name.clone(), crate::core::fmt_type(&resolved_param));
                }
                
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }

        // Collect ensures contracts for runtime checking at return points.
        // Recursively walks nested blocks (if, while, for, parasteps, lambda).
        self.ensures_stmts = if self.verify_contracts {
            collect_ensures(&func.body)
        } else {
            Vec::new()
        };

        // Compile requires contracts as runtime asserts when verify_contracts is enabled
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    self.compile_contract_assert(expr, &vars, &format!("requires violation in '{}'", func.name))?;
                }
            }
        }

        let last_val = self.compile_block_last_val(&func.body, &mut vars)?;

        self.check_unconsumed_caps()?;
        self.pop_comp_scope();

        // Convert pointer-to-struct to struct value when return type expects a struct
        // Must happen BEFORE free_heap_allocs to null out heap data pointers in the original struct,
        // preventing use-after-free on the returned value's heap-allocated data.
        let last_val = match (last_val, ret_type) {
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                let loaded = self.builder.build_load(BasicTypeEnum::StructType(st), pv, "ret_load")
                    .map_err(|e| CompileError::LlvmError(format!("load return struct: {}", e)))?;
                if st.get_field_types().len() > 1 {
                    let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                    if let Ok(data_gep) = self.gep().build_struct_gep(st, pv, 1, "ret_data_null") {
                        let _ = self.builder.build_store(data_gep, null_ptr);
                    }
                }
                loaded
            }
            _ => last_val,
        };

        self.free_heap_allocs()?;
        self.release_all_shared()?;
        self.pop_cap_scope();

        if !self.block_has_terminator() {
            let ensures = self.ensures_stmts.clone();
            if !ensures.is_empty() {
                let result_alloca = self.builder.build_alloca(ret_type, "result")
                    .map_err(|e| CompileError::LlvmError(format!("result alloca: {}", e)))?;
                let adjusted = self.adjust_int_val(last_val, ret_type)?;
                self.builder.build_store(result_alloca, adjusted)
                    .map_err(|e| CompileError::LlvmError(format!("result store: {}", e)))?;
                let mut ensures_vars = vars.clone();
                ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
                for ensures_expr in &ensures {
                    self.compile_contract_assert(ensures_expr, &ensures_vars, &format!("ensures violation in '{}'", func.name))?;
                }
            }
            let adjusted = self.adjust_int_val(last_val, ret_type)?;
            self.builder.build_return(Some(&adjusted)).map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        }
        self.type_map = prev_type_map;
        Ok(())
    }
}
