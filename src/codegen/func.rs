use crate::ast::*;
use crate::codegen::types;
use std::collections::HashMap;
use std::ops::ControlFlow;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
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
mod export;
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

        let result_ty = func
            .ret
            .as_ref()
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
        let poll_fn = self.module.add_function(
            &poll_name,
            poll_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );
        let poll_entry = self.context.append_basic_block(poll_fn, "entry");
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(poll_entry);

        let poll_future_ptr = poll_fn
            .get_nth_param(0)
            .ok_or_else(|| CompileError::LlvmError("poll_fn: param 0 not found".into()))?
            .into_pointer_value();

        // Load args from future + args_offset and call body
        let body_fn = self
            .module
            .get_function(&body_name)
            .ok_or_else(|| CompileError::LlvmError(format!("body fn '{}' not found", body_name)))?;
        let mut poll_call_args = Vec::new();
        let mut current_arg_offset = args_offset;
        for (param_idx, _param) in func.params.iter().enumerate() {
            if param_idx < param_types.len() {
                let ty = param_types[param_idx];
                let size = param_sizes[param_idx];
                // GEP to load arg: future + current_arg_offset
                let arg_ptr_i8 = self
                    .gep()
                    .build_gep(
                        i8_ty,
                        poll_future_ptr,
                        &[i64_ty.const_int(current_arg_offset, false)],
                        &format!("poll_arg_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("poll arg gep: {}", e)))?;
                let arg_typed_ptr = self
                    .builder
                    .build_pointer_cast(
                        arg_ptr_i8,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("poll_arg_typed_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("poll arg cast: {}", e)))?;
                let arg_val =
                    self.build_load(ty, arg_typed_ptr, &format!("poll_arg_val_{}", param_idx))?;
                poll_call_args.push(match arg_val {
                    BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
                    BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(fv),
                    BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(pv),
                    BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(sv),
                    BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(av),
                    BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(vv),
                    BasicValueEnum::ScalableVectorValue(svv) => {
                        BasicMetadataValueEnum::ScalableVectorValue(svv)
                    }
                });
                current_arg_offset += size;
            }
        }

        let poll_body_result = self
            .build_call(body_fn, &poll_call_args, "poll_body_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("poll body returned void".into()))?;

        // Store result at future + 8
        if !func
            .ret
            .as_ref()
            .map_or(true, |t| matches!(t, Type::Name(n, _) if n == "unit"))
        {
            let result_ptr_i8 = self
                .gep()
                .build_gep(
                    i8_ty,
                    poll_future_ptr,
                    &[i64_ty.const_int(8, false)],
                    "poll_result_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("poll result gep: {}", e)))?;
            let result_typed_ptr = self
                .builder
                .build_pointer_cast(
                    result_ptr_i8,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "poll_result_typed",
                )
                .map_err(|e| CompileError::LlvmError(format!("poll result cast: {}", e)))?;
            self.build_store(result_typed_ptr, poll_body_result)?;
        }

        // Set completed
        let set_c_fn = self
            .module
            .get_function("mimi_future_set_completed")
            .ok_or_else(|| {
                CompileError::LlvmError("mimi_future_set_completed not declared".into())
            })?;
        self.build_call(
            set_c_fn,
            &[BasicMetadataValueEnum::PointerValue(poll_future_ptr)],
            "poll_set_completed",
        )?;

        self.build_return(None)?;

        // Restore insertion point
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        // ── Step 2b: Generate async constructor function ──
        // foo(args...) -> i8*  (returns future pointer, submitted to executor)
        let metadata_params: Vec<_> = param_types
            .iter()
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
                let alloca = self.build_alloca(ty, &param.name)?;
                let param_val = function
                    .get_nth_param(i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("param {} not found", i)))?;
                self.build_store(alloca, param_val)?;
                vars.insert(param.name.clone(), (alloca, ty));
                if let Type::Name(tn, args) = &param.ty {
                    if tn == "List" && !args.is_empty() {
                        if let Some(full) = self.get_full_type_name(&param.ty) {
                            self.var_type_names.insert(param.name.clone(), full);
                        }
                    } else {
                        self.var_type_names.insert(param.name.clone(), tn.clone());
                    }
                }
                // Register list element type for List<T> params where T is a struct
                self.register_list_elem_type(&param.name, &param.ty);
            }
        }

        // Allocate future: call mimi_future_alloc(total_size)
        let alloc_fn = self
            .module
            .get_function("mimi_future_alloc")
            .ok_or_else(|| CompileError::LlvmError("mimi_future_alloc not declared".into()))?;
        let total_size_val = i64_ty.const_int(total_alloc_size, false);
        let future_ptr = self
            .build_call(
                alloc_fn,
                &[BasicMetadataValueEnum::IntValue(total_size_val)],
                "future_alloc",
            )?
            .try_as_basic_value_opt()
            .map(|v: BasicValueEnum<'ctx>| v.into_pointer_value())
            .ok_or_else(|| CompileError::LlvmError("future_alloc returned non-pointer".into()))?;

        // Store args in future at args_offset
        let mut current_arg_store_offset = args_offset;
        for (param_idx, param) in func.params.iter().enumerate() {
            if param_idx < param_types.len() {
                let ty = param_types[param_idx];
                let size = param_sizes[param_idx];
                let alloca = vars.get(&param.name).ok_or_else(|| {
                    CompileError::LlvmError(format!("var '{}' not found", param.name))
                })?;
                let val = self.build_load(ty, alloca.0, &format!("store_{}", param.name))?;
                let arg_slot_i8 = self
                    .gep()
                    .build_gep(
                        i8_ty,
                        future_ptr,
                        &[i64_ty.const_int(current_arg_store_offset, false)],
                        &format!("arg_slot_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("arg slot gep: {}", e)))?;
                let arg_slot_typed = self
                    .builder
                    .build_pointer_cast(
                        arg_slot_i8,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("arg_slot_typed_{}", param_idx),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("arg slot cast: {}", e)))?;
                self.build_store(arg_slot_typed, val)?;
                current_arg_store_offset += size;
            }
        }

        // Call mimi_executor_spawn(future, poll_fn)
        let spawn_fn = self
            .module
            .get_function("mimi_executor_spawn")
            .ok_or_else(|| CompileError::LlvmError("mimi_executor_spawn not declared".into()))?;
        let poll_fn_as_i8 = self
            .builder
            .build_pointer_cast(
                poll_fn.as_global_value().as_pointer_value(),
                i8_ptr_ty,
                "poll_fn_i8",
            )
            .map_err(|e| CompileError::LlvmError(format!("poll fn cast: {}", e)))?;
        self.build_call(
            spawn_fn,
            &[
                BasicMetadataValueEnum::PointerValue(future_ptr),
                BasicMetadataValueEnum::PointerValue(poll_fn_as_i8),
            ],
            "executor_spawn",
        )?;

        // Return the future pointer
        self.build_return(Some(&BasicValueEnum::PointerValue(future_ptr)))?;

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
            Stmt::If {
                cond: _,
                then_,
                else_,
            } => {
                let then_ty = Self::concrete_return_type_for_impl_trait(then_);
                if then_ty.is_some() {
                    then_ty
                } else {
                    else_
                        .as_ref()
                        .and_then(|el| Self::concrete_return_type_for_impl_trait(el))
                }
            }
            Stmt::Block(block) => Self::concrete_return_type_for_impl_trait(block),
            _ => None,
        }
    }

    /// Snapshot live variable values at function entry so that `old(x)` in
    /// postconditions refers to the value at call time, not the current value.
    fn snapshot_old_values(&mut self, vars: &HashMap<String, VarEntry<'ctx>>) -> MimiResult<()> {
        self.old_snapshots.clear();
        if self.ensures_stmts.is_empty() {
            return Ok(());
        }
        for (name, &(alloca, ty)) in vars {
            let old_alloca = self.build_alloca(ty, &format!("{}_old", name))?;
            let val = self.build_load(ty, alloca, &format!("{}_snap", name))?;
            self.build_store(old_alloca, val)?;
            self.old_snapshots.insert(name.clone(), (old_alloca, ty));
        }
        Ok(())
    }

    /// Collect `ensures` contracts and compile `requires` contracts as runtime
    /// assertions when contract verification is enabled.
    fn prepare_func_contracts(
        &mut self,
        func: &FuncDef,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        self.ensures_stmts = if self.verify_contracts {
            collect_ensures(&func.body)
        } else {
            Vec::new()
        };
        if self.verify_contracts {
            for stmt in &func.body {
                if let Stmt::Requires(expr, _) = stmt {
                    self.compile_contract_assert(
                        expr,
                        vars,
                        &format!("requires violation in '{}'", func.name),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Emit a function return: check `ensures` contracts, clean up scopes, and
    /// build the LLVM return instruction. `val` of `None` means a bare `return;`.
    fn emit_return(
        &mut self,
        ret_type: BasicTypeEnum<'ctx>,
        val: Option<BasicValueEnum<'ctx>>,
        func_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        let ensures = self.ensures_stmts.clone();
        if !ensures.is_empty() {
            let result_alloca = self.build_alloca(ret_type, "result")?;
            let stored_val =
                val.unwrap_or_else(|| self.context.i64_type().const_int(0, false).into());
            let adjusted = self.adjust_int_val(stored_val, ret_type)?;
            self.build_store(result_alloca, adjusted)?;
            let mut ensures_vars = vars.clone();
            ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
            for ensures_expr in &ensures {
                self.compile_contract_assert(
                    ensures_expr,
                    &ensures_vars,
                    &format!("ensures violation in '{}'", func_name),
                )?;
            }
        }
        self.pop_shared_scope()?;
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        self.pop_cap_scope();
        match val {
            Some(v) => {
                let adjusted = self.adjust_int_val(v, ret_type)?;
                self.build_return(Some(&adjusted))?;
            }
            None => self.build_return(None)?,
        }
        Ok(())
    }

    /// Bind all function parameters to stack allocas and track type metadata
    /// (type names, list element types, and capabilities).
    fn bind_func_params(
        &mut self,
        func: &FuncDef,
        function: FunctionValue<'ctx>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        for (i, param) in func.params.iter().enumerate() {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = self.llvm_type_for(&resolved) {
                let alloca = self.build_alloca(ty, &param.name)?;
                self.build_store(
                    alloca,
                    function.get_nth_param(i as u32).ok_or_else(|| {
                        CompileError::LlvmError(format!(
                            "param index {} out of range for function '{}' with {} params",
                            i,
                            func.name,
                            function.count_params()
                        ))
                    })?,
                )?;
                vars.insert(param.name.clone(), (alloca, ty));

                // Track type name for method dispatch
                if let Type::Name(tn, args) = &resolved {
                    if tn == "List" && !args.is_empty() {
                        if let Some(full) = self.get_full_type_name(&resolved) {
                            self.var_type_names.insert(param.name.clone(), full);
                        }
                    } else {
                        self.var_type_names.insert(param.name.clone(), tn.clone());
                    }
                }
                if let Type::DynTrait(_) = &resolved {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                }
                if let Type::ImplTrait(_) = &resolved {
                    self.var_type_names
                        .insert(param.name.clone(), crate::core::fmt_type(&resolved));
                }

                // Register list element type for List<T> params where T is a struct
                self.register_list_elem_type(&param.name, &resolved);

                // Track capability parameters
                if matches!(&param.ty, Type::Cap(_)) {
                    self.register_cap(&param.name, alloca);
                }
            }
        }
        Ok(())
    }

    /// Compile the body of a non-generic function statement-by-statement.
    /// Returns `ControlFlow::Break(())` when an explicit `return` statement
    /// has already emitted the terminator; otherwise returns the implicit last
    /// value that should be returned.
    fn compile_func_body(
        &mut self,
        func: &FuncDef,
        ret_type: BasicTypeEnum<'ctx>,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<ControlFlow<(), BasicValueEnum<'ctx>>> {
        let default_val = match ret_type {
            BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
            BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
            _ => self.context.i64_type().const_int(0, false).into(),
        };
        let mut last_val: BasicValueEnum<'ctx> = default_val;
        for stmt in &func.body {
            // Run compensations before exit()
            if let Stmt::Expr(Expr::Call(callee, _)) = stmt {
                if let Expr::Ident(name) = &**callee {
                    if name == "exit" {
                        self.compile_compensations(vars)?;
                    }
                }
            }
            match stmt {
                Stmt::Expr(expr) => {
                    last_val = self.compile_expr(expr, vars)?;
                    last_val = self.adjust_int_val(last_val, ret_type)?;
                }
                Stmt::Return(Some(expr)) => {
                    let val = self.compile_expr(expr, vars)?;
                    let val = self.adjust_int_val(val, ret_type)?;
                    self.emit_return(ret_type, Some(val), &func.name, vars)?;
                    return Ok(ControlFlow::Break(()));
                }
                Stmt::Return(None) => {
                    self.emit_return(ret_type, None, &func.name, vars)?;
                    return Ok(ControlFlow::Break(()));
                }
                Stmt::Let {
                    pat,
                    init: Some(init),
                    ty,
                    ..
                } => {
                    // dyn Trait let-binding: build fat pointer from concrete value (requires Variable pattern)
                    if let Some(Type::DynTrait(trait_names)) = &ty {
                        let name = match pat {
                            Pattern::Variable(n) => n.clone(),
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "dyn Trait binding requires a simple variable pattern"
                                        .to_string(),
                                ))
                            }
                        };
                        let concrete_val = self.compile_expr(init, vars)?;
                        let concrete_type = match init {
                            Expr::Record { ty: Some(tn), .. } => tn.clone(),
                            Expr::Ident(var_name) => self
                                .var_type_names
                                .get(var_name)
                                .cloned()
                                .unwrap_or_default(),
                            _ => {
                                return Err(CompileError::LlvmError(format!(
                                    "cannot infer concrete type for dyn Trait binding '{}'",
                                    name
                                )));
                            }
                        };
                        if concrete_type.is_empty() {
                            return Err(CompileError::LlvmError(format!(
                                "cannot infer concrete type for dyn Trait binding '{}'",
                                name
                            )));
                        }
                        let trait_name = &trait_names[0];
                        let concrete_ty = self
                            .type_llvm
                            .get(&concrete_type)
                            .cloned()
                            .unwrap_or_else(|| concrete_val.get_type());
                        let data_alloca =
                            self.build_alloca(concrete_ty, &format!("{}_data", name))?;
                        self.build_store(data_alloca, concrete_val)?;
                        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                        let data_ptr = self
                            .builder
                            .build_pointer_cast(data_alloca, i8_ptr, &format!("{}_data_i8", name))
                            .map_err(|e| {
                                CompileError::LlvmError(format!("pointer cast error: {}", e))
                            })?;
                        let vtable_key = format!("{}__{}", concrete_type, trait_name);
                        let vtable_gv = self.vtable_globals.get(&vtable_key).ok_or_else(|| {
                            CompileError::LlvmError(format!(
                                "no vtable for {}.{}",
                                concrete_type, trait_name
                            ))
                        })?;
                        let vtable_ptr = self
                            .builder
                            .build_pointer_cast(
                                vtable_gv.as_pointer_value(),
                                i8_ptr,
                                &format!("{}_vtable_i8", name),
                            )
                            .map_err(|e| {
                                CompileError::LlvmError(format!("pointer cast error: {}", e))
                            })?;
                        let fat_ty = BasicTypeEnum::StructType(self.context.struct_type(
                            &[
                                BasicTypeEnum::PointerType(i8_ptr),
                                BasicTypeEnum::PointerType(i8_ptr),
                            ],
                            false,
                        ));
                        let fat_alloca = self.build_alloca(fat_ty, &name)?;
                        let data_gep = self
                            .gep()
                            .build_struct_gep(fat_ty, fat_alloca, 0, &format!("{}_data_gep", name))
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.build_store(data_gep, data_ptr)?;
                        let vtable_gep = self
                            .gep()
                            .build_struct_gep(
                                fat_ty,
                                fat_alloca,
                                1,
                                &format!("{}_vtable_gep", name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        self.build_store(vtable_gep, vtable_ptr)?;
                        let ty_ref = ty.as_ref().ok_or_else(|| {
                            CompileError::LlvmError(format!("missing type for variable '{}'", name))
                        })?;
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
                                self.compile_shared_ref_copy(name, src_name, vars)?;
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
                                                self.compile_shared_ref_copy(name, src_name, vars)?;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Non-dyn Trait: compile init and bind via recursive pattern matching
                    let mut val = self.compile_expr(init, vars)?;
                    if let Some(decl_ty) = ty {
                        let target = types::mimi_type_to_llvm(self.context, decl_ty)
                            .unwrap_or_else(|| val.get_type());
                        val = self.adjust_int_val(val, target)?;
                    }
                    // Track type info for simple Variable patterns
                    if let Pattern::Variable(name) = pat {
                        if let Some(ty_ref) = &ty {
                            if let Type::Name(tn, args) = ty_ref {
                                if tn == "List" && !args.is_empty() {
                                    // Store full List<T> type for element reconstruction
                                    if let Some(full) = self.get_full_type_name(ty_ref) {
                                        self.var_type_names.insert(name.clone(), full);
                                    }
                                } else {
                                    self.var_type_names.insert(name.clone(), tn.clone());
                                }
                            }
                        } else if self.expr_is_string(init) {
                            self.var_type_names
                                .insert(name.clone(), "string".to_string());
                        } else if let Expr::Record { ty: Some(tn), .. } = init {
                            self.var_type_names.insert(name.clone(), tn.clone());
                        } else if matches!(init, Expr::SetLiteral(_)) {
                            self.var_type_names.insert(name.clone(), "set".to_string());
                        } else if let Expr::List(list_elems) = init {
                            // D1: infer List<T> type from first element
                            if let Some(first) = list_elems.first() {
                                let elem_type = self.infer_object_type(first, vars);
                                if !elem_type.is_empty() {
                                    self.var_type_names
                                        .insert(name.clone(), format!("List<{}>", elem_type));
                                }
                            }
                        } else if let Expr::Index(_, _) = init {
                            // D1: infer element type via infer_object_type (handles List<T> stripping)
                            let elem_type = self.infer_object_type(init, vars);
                            if !elem_type.is_empty() {
                                self.var_type_names.insert(name.clone(), elem_type);
                            }
                        } else if let Expr::Call(callee, _) = init {
                            if let Expr::Field(obj, method_name) = callee.as_ref() {
                                if method_name == "spawn" {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if !obj_type.is_empty() {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(
                                    method_name.as_str(),
                                    "map" | "and_then" | "map_err" | "ok_or"
                                ) {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type == "Result" || obj_type == "Option" {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                } else if matches!(method_name.as_str(), "insert" | "remove") {
                                    let obj_type = self.infer_object_type(obj, vars);
                                    if obj_type.starts_with("Set") || obj_type == "set" {
                                        self.var_type_names.insert(name.clone(), obj_type);
                                    }
                                }
                            } else if let Expr::Ident(func_name) = callee.as_ref() {
                                match func_name.as_str() {
                                    "Ok" | "Err" => {
                                        self.var_type_names
                                            .insert(name.clone(), "Result".to_string());
                                    }
                                    "Some" | "None" => {
                                        self.var_type_names
                                            .insert(name.clone(), "Option".to_string());
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
                                                        self.var_type_names
                                                            .insert(name.clone(), tn.clone());
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                        // G-41: Track return types for builtins that return List<string>
                                        match func_name.as_str() {
                                            "listdir" | "walk_dir" | "str_split" => {
                                                self.var_type_names.insert(
                                                    name.clone(),
                                                    "List<string>".to_string(),
                                                );
                                                self.var_types.insert(
                                                    name.clone(),
                                                    Type::Name(
                                                        "List".into(),
                                                        vec![Type::Name("string".into(), vec![])],
                                                    ),
                                                );
                                            }
                                            "exec" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "ExecResult".to_string());
                                            }
                                            "file_stat" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "StatResult".to_string());
                                            }
                                            "append_file" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "bool".to_string());
                                            }
                                            "set_env" => {
                                                self.var_type_names
                                                    .insert(name.clone(), "bool".to_string());
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        } else if let Expr::Turbofish(_func_name, turbo_type_args, _) = init {
                            if let Some(ta) = turbo_type_args.first() {
                                if let Type::Name(tn, args) = ta {
                                    if tn == "List" && !args.is_empty() {
                                        if let Some(full) = self.get_full_type_name(ta) {
                                            self.var_type_names.insert(name.clone(), full);
                                        }
                                    } else {
                                        self.var_type_names.insert(name.clone(), tn.clone());
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
                    // For tuple patterns, push the tuple type onto tuple_type_stack
                    // so that compile_pattern_bind can load the struct correctly
                    if let Pattern::Tuple(sub_pats) = pat {
                        if !sub_pats.is_empty() {
                            // Try to infer tuple type from declared type or init expression
                            let tuple_ty = if let Some(Type::Tuple(elem_tys)) = &ty {
                                let field_tys: Vec<BasicTypeEnum> = elem_tys
                                    .iter()
                                    .map(|t| {
                                        types::mimi_type_to_llvm(self.context, t).unwrap_or(
                                            BasicTypeEnum::IntType(self.context.i64_type()),
                                        )
                                    })
                                    .collect();
                                self.context.struct_type(&field_tys, false)
                            } else {
                                // Fallback: create a struct with i64 fields
                                let field_tys: Vec<BasicTypeEnum> = sub_pats
                                    .iter()
                                    .map(|_| BasicTypeEnum::IntType(self.context.i64_type()))
                                    .collect();
                                self.context.struct_type(&field_tys, false)
                            };
                            self.tuple_type_stack.push(tuple_ty);
                        }
                    }
                    self.compile_pattern_bind(pat, val, vars)?;
                    // Pop tuple type stack if we pushed it
                    if let Pattern::Tuple(sub_pats) = pat {
                        if !sub_pats.is_empty() {
                            self.tuple_type_stack.pop();
                        }
                    }
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
                    self.compile_assign_stmt(target, value, vars)?;
                }
                Stmt::If { cond, then_, else_ } => {
                    let cond_val = self.compile_expr(cond, vars)?;
                    let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                        iv
                    } else {
                        return Err(CompileError::TypeMismatch(format!(
                            "if condition must be bool, got {} in function '{}'",
                            cond_val.get_type(),
                            func.name
                        )));
                    };

                    let function = self.current_function().ok_or_else(|| {
                        CompileError::LlvmError("codegen: no current function for if".to_string())
                    })?;
                    let then_bb = self.context.append_basic_block(function, "then");
                    let else_bb = self.context.append_basic_block(function, "else");
                    let merge_bb = self.context.append_basic_block(function, "ifcont");

                    self.build_cond_br(cond_bool, then_bb, else_bb)?;

                    // Then block
                    self.builder.position_at_end(then_bb);
                    let mut then_vars = vars.clone();
                    let then_val = self.compile_block_last_val(then_, &mut then_vars)?;
                    let then_reaches = !self.block_has_terminator();
                    if then_reaches {
                        self.build_br(merge_bb)?;
                    }
                    let then_bb_end = then_reaches
                        .then(|| self.builder.get_insert_block())
                        .flatten();

                    // Else block
                    self.builder.position_at_end(else_bb);
                    let else_val = if let Some(else_block) = else_ {
                        let mut else_vars = vars.clone();
                        let v = self.compile_block_last_val(else_block, &mut else_vars)?;
                        let reaches = !self.block_has_terminator();
                        if reaches {
                            self.build_br(merge_bb)?;
                        }
                        (v, reaches)
                    } else {
                        let reaches = !self.block_has_terminator();
                        if reaches {
                            self.build_br(merge_bb)?;
                        }
                        (self.context.i64_type().const_int(0, false).into(), reaches)
                    };
                    let (else_val, else_reaches) = else_val;
                    let else_bb_end = else_reaches
                        .then(|| self.builder.get_insert_block())
                        .flatten();

                    // Continue at merge, produce phi with only blocks that reach merge
                    self.builder.position_at_end(merge_bb);
                    if then_val.get_type() == else_val.get_type() {
                        let phi = self
                            .builder
                            .build_phi(then_val.get_type(), "if_result")
                            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                        let mut phi_incoming: Vec<(
                            &dyn inkwell::values::BasicValue,
                            inkwell::basic_block::BasicBlock,
                        )> = Vec::new();
                        if let Some(bb) = then_bb_end {
                            phi_incoming.push((&then_val as &dyn inkwell::values::BasicValue, bb));
                        }
                        if let Some(bb) = else_bb_end {
                            phi_incoming.push((&else_val as &dyn inkwell::values::BasicValue, bb));
                        }
                        if !phi_incoming.is_empty() {
                            phi.add_incoming(&phi_incoming);
                        }
                        last_val = phi.as_basic_value();
                    }
                }
                Stmt::While { cond, body } => {
                    self.compile_while_stmt(cond, body, vars)?;
                }
                Stmt::WhileLet { pat, init, body } => {
                    self.compile_while_let_stmt(pat, init, body, vars)?;
                }
                Stmt::Loop(body) => {
                    self.compile_loop_stmt(body, vars)?;
                }
                Stmt::For {
                    var,
                    iterable,
                    body,
                } => {
                    self.compile_for_stmt(var, iterable, body, vars)?;
                }
                Stmt::Break(_) => {
                    if let Some(target) = self.loop_break {
                        self.build_br(target)?;
                        // Create unreachable block for subsequent statements
                        let function = self.current_function().ok_or_else(|| {
                            CompileError::LlvmError(
                                "codegen: no current function for break".to_string(),
                            )
                        })?;
                        let unreachable = self.context.append_basic_block(function, "unreachable");
                        self.builder.position_at_end(unreachable);
                    } else {
                        return Err(CompileError::BreakOutsideLoop);
                    }
                }
                Stmt::Continue => {
                    if let Some(target) = self.loop_continue {
                        self.build_br(target)?;
                        let function = self.current_function().ok_or_else(|| {
                            CompileError::LlvmError(
                                "codegen: no current function for continue".to_string(),
                            )
                        })?;
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
                    self.compile_block(block, vars)?;
                    self.leave_parasteps()?;
                }
                Stmt::Drop(expr) => {
                    // Drop: evaluate expression and mark capability as consumed
                    let _val = self.compile_expr(expr, vars)?;
                    // If the expression is a variable, mark it as consumed and call mimi_cap_consume
                    if let Expr::Ident(name) = expr {
                        self.consume_cap(name)?;
                        // Generate runtime cap consume call
                        if self.is_cap_var(name) {
                            if let Some(consume_fn) = self.module.get_function("mimi_cap_consume") {
                                if let Some(&(alloca, _)) = vars.get(name) {
                                    let cap_val = self.build_load(
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                        alloca,
                                        &format!("cap_val_{}", name),
                                    )?;
                                    let name_global = self
                                        .builder
                                        .build_global_string_ptr(
                                            &format!("{}\0", name),
                                            &format!("cap_name_drop_{}", name),
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "string global error: {}",
                                                e
                                            ))
                                        })?;
                                    let name_ptr = name_global.as_pointer_value();
                                    self.build_call(
                                        consume_fn,
                                        &[
                                            BasicMetadataValueEnum::IntValue(
                                                cap_val.into_int_value(),
                                            ),
                                            BasicMetadataValueEnum::PointerValue(name_ptr),
                                        ],
                                        &format!("cap_consume_{}", name),
                                    )?;
                                }
                            }
                        }
                    }
                }
                Stmt::SharedLet {
                    kind,
                    name,
                    ty,
                    init,
                } => {
                    self.compile_shared_let_stmt(kind, name, ty, init, vars)?;
                }
                Stmt::OnFailure(block) => {
                    // Register compensation block for LIFO execution on error exit
                    self.register_comp(block);
                }
                Stmt::Arena(block) => {
                    self.compile_arena_block(block, vars, "arena")?;
                }
                Stmt::Unsafe(block) => {
                    // Unsafe: execute block (no restrictions in codegen)
                    self.compile_block(block, vars)?;
                }
                Stmt::Alloc {
                    kind: AllocKind::Arena,
                    body,
                } => {
                    self.compile_arena_block(body, vars, "alloc(Arena)")?;
                }
                Stmt::Alloc { body, .. } => {
                    // Alloc: execute body sequentially (simplified - no custom allocator in codegen)
                    self.compile_block(body, vars)?;
                }
                Stmt::Desc(..)
                | Stmt::Rule(..)
                | Stmt::Requires(..)
                | Stmt::Ensures(..)
                | Stmt::Invariant(..)
                | Stmt::Math(_)
                | Stmt::Ellipsis => {
                    // Skip contract-related statements in codegen
                }
                Stmt::Block(block) => {
                    self.compile_block(block, vars)?;
                }
                _ => {}
            }
        }
        Ok(ControlFlow::Continue(last_val))
    }

    /// Emit the implicit return at the end of a function: check for unconsumed
    /// capabilities, convert pointer-to-struct returns, clean up scopes, verify
    /// postconditions, and build the final return instruction.
    fn emit_implicit_return(
        &mut self,
        ret_type: BasicTypeEnum<'ctx>,
        last_val: BasicValueEnum<'ctx>,
        func_name: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> MimiResult<()> {
        // Check for unconsumed capabilities before returning
        self.check_unconsumed_caps()?;

        // Convert pointer-to-struct to struct value when return type expects a struct.
        // Must happen BEFORE free_heap_allocs to null out heap data pointers in the original struct,
        // preventing use-after-free on the returned value's heap-allocated data.
        //
        // Special case: string literal returns a raw i8* (PointerValue), but the Mimi string
        // type is {i8*, i64}. We need to wrap the raw pointer into a struct via wrap_c_string.
        let last_val = match (last_val, ret_type) {
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                let field_types = st.get_field_types();
                // Check if this is the Mimi string struct {ptr, i64} — the pointer is
                // a raw C string (from literal), not a pointer to an alloca'd struct.
                let is_string_struct = field_types.len() == 2
                    && matches!(&field_types[0], BasicTypeEnum::PointerType(_))
                    && matches!(&field_types[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if is_string_struct {
                    self.wrap_c_string(pv)?
                } else {
                    let loaded = self.build_load(BasicTypeEnum::StructType(st), pv, "ret_load")?;
                    // Null out field at index 1 (data pointer) to prevent free_heap_allocs from freeing
                    // the heap data that's now owned by the caller via the returned struct value.
                    if field_types.len() > 1 {
                        let null_ptr = self.context.ptr_type(AddressSpace::default()).const_null();
                        if let Ok(data_gep) =
                            self.gep().build_struct_gep(st, pv, 1, "ret_data_null")
                        {
                            let _ = self.builder.build_store(data_gep, null_ptr);
                        }
                    }
                    loaded
                }
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
                let result_alloca = self.build_alloca(ret_type, "result")?;
                let adjusted = self.adjust_int_val(last_val, ret_type)?;
                self.build_store(result_alloca, adjusted)?;
                let mut ensures_vars = vars.clone();
                ensures_vars.insert("result".to_string(), (result_alloca, ret_type));
                for ensures_expr in &ensures {
                    self.compile_contract_assert(
                        ensures_expr,
                        &ensures_vars,
                        &format!("ensures violation in '{}'", func_name),
                    )?;
                }
            }
        }
        let last_val = self.adjust_int_val(last_val, ret_type)?;
        self.build_return(Some(&last_val))?;
        Ok(())
    }

    pub(super) fn compile_func(&mut self, func: &FuncDef) -> MimiResult<()> {
        // Delegate async funcs to compile_async_func
        if func.is_async {
            return self.compile_async_func(func);
        }

        // Exported extern functions get a C ABI wrapper around an internal body.
        if func.extern_abi.is_some() && func.generics.is_empty() {
            let body_name = format!("{}__mimi_export_body", func.name);
            if self.module.get_function(&body_name).is_none() {
                let mut body_func = func.clone();
                body_func.name = body_name.clone();
                body_func.extern_abi = None;
                self.compile_func(&body_func)?;
            }
            return self.compile_export_wrapper(func, &body_name);
        }

        // For impl Trait return types, determine the concrete type from the body
        // so the function's LLVM signature uses the right type.
        let effective_ret_override = if let Some(Type::ImplTrait(_)) = &func.ret {
            Self::concrete_return_type_for_impl_trait(&func.body)
                .and_then(|tn| self.type_llvm.get(&tn).cloned())
        } else {
            None
        };

        let ret_type = effective_ret_override
            .or_else(|| match &func.ret {
                Some(ty) => self.llvm_type_for(ty),
                None => None,
            })
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let mut param_types = Vec::new();
        for param in &func.params {
            if let Some(ty) = self.llvm_type_for(&param.ty) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types
            .iter()
            .map(|t| types::basic_to_metadata(self.context, *t))
            .collect();

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
        self.bind_func_params(func, function, &mut vars)?;

        // Prepare and compile function contracts.
        self.prepare_func_contracts(func, &vars)?;
        self.snapshot_old_values(&vars)?;

        match self.compile_func_body(func, ret_type, &mut vars)? {
            ControlFlow::Break(()) => return Ok(()),
            ControlFlow::Continue(last_val) => {
                self.emit_implicit_return(ret_type, last_val, &func.name, &vars)?;
            }
        }

        // v0.28.13 — Inline heuristic: small functions become candidates.
        // If the function's instruction count is below the threshold, it
        // is registered as an inline candidate. Pure functions (no calls,
        // no FFI, no side-effecting builtins) are also marked pure so
        // they are eligible for CSE.
        let inst_count = self.count_instructions_in_function(function);
        if inst_count > 0 && inst_count <= Self::INLINE_INSTRUCTION_THRESHOLD {
            self.register_inline_candidate(func.name.clone());
            // Heuristic: pure if no external calls in the body.
            // (Full purity analysis is left for v0.28.14.)
            let mut has_calls = false;
            for bb in function.get_basic_blocks() {
                for inst in bb.get_instructions() {
                    if matches!(
                        inst.get_opcode(),
                        inkwell::values::InstructionOpcode::Call
                    ) {
                        has_calls = true;
                    }
                }
            }
            if !has_calls {
                self.mark_pure(func.name.clone());
            }
        }

        Ok(())
    }

    /// Compile a generic function with concrete type arguments (monomorphization)
    pub(super) fn compile_generic_func(
        &mut self,
        func: &FuncDef,
        type_map: &HashMap<String, crate::ast::Type>,
    ) -> MimiResult<()> {
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
        let ret_type = effective_ret_override
            .or_else(|| match &func.ret {
                Some(ty) => {
                    let resolved = self.resolve_type(ty);
                    self.llvm_type_for(&resolved)
                }
                None => None,
            })
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let mut param_types = Vec::new();
        for param in &func.params {
            let resolved = self.resolve_type(&param.ty);
            if let Some(ty) = self.llvm_type_for(&resolved) {
                param_types.push(ty);
            }
        }

        let metadata_params: Vec<_> = param_types
            .iter()
            .map(|t| types::basic_to_metadata(self.context, *t))
            .collect();

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
        self.bind_func_params(func, function, &mut vars)?;

        // Prepare and compile function contracts.
        self.prepare_func_contracts(func, &vars)?;
        self.snapshot_old_values(&vars)?;

        let last_val = self.compile_block_last_val(&func.body, &mut vars)?;

        self.emit_implicit_return(ret_type, last_val, &func.name, &vars)?;
        self.type_map = prev_type_map;
        Ok(())
    }
}
