use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::{BTreeMap, HashMap};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_spawn_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Create a real pthread for spawn both inside and outside parasteps.
        // Inside parasteps, track the thread ID for joining at the barrier.
        let (thread_id, result_type) = self.compile_spawn_pthread(expr, vars)?;
        if self.in_parasteps {
            if let BasicValueEnum::IntValue(tid) = thread_id {
                self.parasteps_thread_ids.push((tid, result_type));
            }
        }
        Ok(thread_id)
    }

    /// Full wrapper-based spawn for standalone (outside parasteps) — uses pthread_create.
    /// Returns (thread_id, result_type).
    fn compile_spawn_pthread(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(BasicValueEnum<'ctx>, BasicTypeEnum<'ctx>), CompileError> {
        let parent_fn = self.current_function().ok_or_else(|| "codegen: no current function for spawn".to_string())?;
        let parent_name = parent_fn.get_name().to_str().unwrap_or("unknown").to_string();
        let wrapper_name = format!("{}{}__spawn_wrapper", parent_name, self.spawn_counter).to_string();
        self.spawn_counter += 1;
        
        let mut free_vars: BTreeMap<String, (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)> = BTreeMap::new();
        let empty_defined = std::collections::HashSet::new();
        self.collect_free_vars_expr(expr, &empty_defined, vars, &mut free_vars);
        
        let _i8_ty = self.context.i8_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let wrapper_fn_type = i8_ptr.fn_type(
            &[BasicMetadataTypeEnum::PointerType(i8_ptr)], false
        );
        let wrapper_fn = self.module.add_function(&wrapper_name, wrapper_fn_type, None);
        let wrapper_entry = self.context.append_basic_block(wrapper_fn, "entry");
        
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(wrapper_entry);
        
        let env_ptr_param = wrapper_fn.get_nth_param(0)
            .ok_or_else(|| "codegen: spawn wrapper env_ptr param index out of range".to_string())?
            .into_pointer_value();
        let mut wrapper_vars = HashMap::new();
        if !free_vars.is_empty() {
            let env_field_types: Vec<BasicTypeEnum<'ctx>> =
                free_vars.values().map(|&(_, ty)| ty).collect();
            let env_struct_type = self.context.struct_type(&env_field_types, false);
            let env_struct_ptr = self.builder.build_pointer_cast(
                env_ptr_param,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "spawn_env",
            ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
            for (i, (name, &(_, ty))) in free_vars.iter().enumerate() {
                let field_gep = self.gep().build_struct_gep(
                    env_struct_type, env_struct_ptr, i as u32, &format!("spawn_env_{}_gep", name),
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let field_val = self.builder.build_load(ty, field_gep, &format!("spawn_cap_{}", name))
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                let alloca = self.builder.build_alloca(ty, &format!("spawn_cap_{}_alloca", name))
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, field_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                wrapper_vars.insert(name.clone(), (alloca, ty));
            }
        }
        
        let result = self.compile_expr(expr, &wrapper_vars)?;
        let result_type = result.get_type();
        
        let i64_ty = self.context.i64_type();
        let malloc_fn = self.module.get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let byte_size_val = self.llvm_type_size_bytes(result_type);
        let byte_size = i64_ty.const_int(byte_size_val, false);
        let result_storage = self.builder.build_call(malloc_fn, &[
            BasicMetadataValueEnum::IntValue(byte_size),
        ], "malloc_result")
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?;
        let result_storage_ptr = if let BasicValueEnum::PointerValue(pv) = result_storage {
            pv
        } else {
            return Err("malloc should return a pointer".into());
        };
        let result_llvm_ty = result.get_type();
        let result_ptr_ty = match result_llvm_ty {
            BasicTypeEnum::IntType(_) => self.context.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::FloatType(_) => self.context.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::PointerType(_) => self.context.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::StructType(_) => self.context.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::ArrayType(_) => self.context.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::VectorType(_) => self.context.ptr_type(inkwell::AddressSpace::default()),
            BasicTypeEnum::ScalableVectorType(_) => self.context.ptr_type(inkwell::AddressSpace::default()),
        };
        let result_typed_ptr = self.builder.build_pointer_cast(
            result_storage_ptr,
            result_ptr_ty,
            "result_typed"
        ).map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        self.builder.build_store(result_typed_ptr, result)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder.build_return(Some(&result_storage))
            .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;
        
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }
        
        let capture_arg = if !free_vars.is_empty() {
            let env_field_types: Vec<BasicTypeEnum<'ctx>> =
                free_vars.values().map(|&(_, ty)| ty).collect();
            let env_struct_type = self.context.struct_type(&env_field_types, false);
            let env_byte_size = env_struct_type.size_of()
                .ok_or_else(|| "size_of error".to_string())?;
            let env_heap_ptr = self.builder.build_call(malloc_fn, &[
                BasicMetadataValueEnum::IntValue(env_byte_size),
            ], "spawn_env_heap")
                .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                .try_as_basic_value_opt()
                .ok_or("malloc returned void")?
                .into_pointer_value();
            for (i, (name, &(var_alloca, ty))) in free_vars.iter().enumerate() {
                let val = self.builder.build_load(ty, var_alloca, &format!("spawn_cap_val_{}", name))
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                let field_gep = self.gep().build_struct_gep(
                    env_struct_type, env_heap_ptr, i as u32, &format!("spawn_env_{}_gep", name),
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(field_gep, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
            }
            self.builder.build_pointer_cast(
                env_heap_ptr, i8_ptr, "spawn_env_i8",
            ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?
        } else {
            i8_ptr.const_null()
        };
        
        let wrapper_fn_ptr = self.builder.build_pointer_cast(
            wrapper_fn.as_global_value().as_pointer_value(),
            i8_ptr,
            "wrapper_i8"
        ).map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;

        let thread_alloca = self.builder.build_alloca(i64_ty, "thread")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder.build_store(thread_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        let pthread_create_fn = self.module.get_function("pthread_create")
            .ok_or("pthread_create not declared")?;
        self.builder.build_call(pthread_create_fn, &[
            BasicMetadataValueEnum::PointerValue(thread_alloca),
            BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()),
            BasicMetadataValueEnum::PointerValue(wrapper_fn_ptr),
            BasicMetadataValueEnum::PointerValue(capture_arg),
        ], "pthread_create_call")
            .map_err(|e| CompileError::LlvmError(format!("pthread_create error: {}", e)))?;

        let thread_id_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), thread_alloca, "thread_id")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        // Track the spawn result type so compile_await_expr can load with the correct type
        self.pending_spawn_type = Some(result_type);
        Ok((thread_id_val, result_type))
    }

    /// Infer the inner result type of a Future from an await expression.
    /// Returns the LLVM type of the inner value, e.g. for `await f` where f: Future<i32>,
    /// returns IntType(i32).
    fn infer_future_inner_type(
        &self,
        expr: &Expr,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Option<BasicTypeEnum<'ctx>> {
        match expr {
            Expr::Ident(name) => {
                // Check async_var_inner_types first (variable holding a future from async fn)
                if let Some(&ty) = self.async_var_inner_types.get(name) {
                    return Some(ty);
                }
                // Fallback: var_type_names lookup
                if let Some(tn) = self.var_type_names.get(name) {
                    if tn == "Future" {
                        // We don't know the generic param from name alone, so return None
                        // and let the caller use pending_spawn_type
                        return None;
                    }
                    // Could be a plain type (not a future) — in that case it's a spawn handle
                    return None;
                }
                None
            }
            Expr::Call(callee, _) => {
                if let Expr::Ident(func_name) = callee.as_ref() {
                    if let Some(fdef) = self.func_defs.get(func_name) {
                        if let Some(ret_ty) = &fdef.ret {
                            // The function's own return type (before Future wrapping)
                            return self.llvm_type_for(ret_ty);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub(in crate::codegen) fn compile_await_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // Evaluate the child expression to get the handle
        let handle_val = self.compile_expr(expr, vars)?;

        match handle_val {
            // Thread handle from spawn: i64 → pthread_join
            BasicValueEnum::IntValue(handle) => {
                self.compile_await_pthread(handle, expr, vars)
            }
            // Future pointer from async fn: i8* → load result and free
            BasicValueEnum::PointerValue(future_ptr) => {
                // Determine the result type from context
                let result_type = self.infer_future_inner_type(expr, vars)
                    .or_else(|| self.pending_spawn_type.take())
                    .unwrap_or_else(|| self.context.i64_type().into());

                // Run the executor to ensure all pending futures are completed
                let executor_run = self.module.get_function("mimi_executor_run")
                    .ok_or_else(|| CompileError::LlvmError("mimi_executor_run not declared".into()))?;
                self.builder.build_call(executor_run, &[], "executor_run")
                    .map_err(|e| CompileError::LlvmError(format!("executor_run error: {}", e)))?;

        let i8_ty = self.context.i8_type();
                let i64_ty = self.context.i64_type();

                // Load result from future_ptr + 8 (skip completed flag)
                let result_data_ptr = unsafe {
                    self.builder.build_gep(i8_ty, future_ptr, &[i64_ty.const_int(8, false)], "result_data")
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?
                };
                let result_typed_ptr = self.builder.build_pointer_cast(
                    result_data_ptr,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "result_typed",
                ).map_err(|e| CompileError::LlvmError(format!("pointer cast error: {}", e)))?;
                let result_val = self.builder.build_load(
                    result_type,
                    result_typed_ptr,
                    "future_result",
                ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;

                // Free the future
                let free_fn = self.module.get_function("mimi_future_free")
                    .ok_or_else(|| CompileError::LlvmError("mimi_future_free not declared".into()))?;
                self.builder.build_call(free_fn, &[
                    BasicMetadataValueEnum::PointerValue(future_ptr),
                ], "future_free")
                    .map_err(|e| CompileError::LlvmError(format!("future_free error: {}", e)))?;

                Ok(result_val)
            }
            _ => Err(CompileError::Generic("await requires a thread (i64) or future (ptr) value".into())),
        }
    }

    /// Handle `await` for thread handles from `spawn` (pthread_join path).
    fn compile_await_pthread(
        &mut self,
        handle: inkwell::values::IntValue<'ctx>,
        _expr: &Expr,
        _vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let retval_storage = self.builder.build_alloca(i8_ptr, "retval_ptr")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder.build_store(retval_storage, i8_ptr.const_null())
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Determine the result type
        let result_type = if self.in_parasteps {
            let pos = self.parasteps_thread_ids.iter().position(|(id, _)| *id == handle);
            if let Some(pos) = pos {
                let (_, ty) = self.parasteps_thread_ids.remove(pos);
                ty
            } else {
                self.pending_spawn_type.take()
                    .unwrap_or_else(|| self.context.i64_type().into())
            }
        } else {
            self.pending_spawn_type.take()
                .unwrap_or_else(|| self.context.i64_type().into())
        };

        if self.in_parasteps {
            self.parasteps_thread_ids.retain(|(id, _)| *id != handle);
        }

        let pthread_join_fn = self.module.get_function("pthread_join")
            .ok_or("pthread_join not declared")?;
        self.builder.build_call(pthread_join_fn, &[
            BasicMetadataValueEnum::IntValue(handle),
            BasicMetadataValueEnum::PointerValue(retval_storage),
        ], "pthread_join_call")
            .map_err(|e| CompileError::LlvmError(format!("pthread_join error: {}", e)))?;

        let result_i8_ptr = self.builder.build_load(
            BasicTypeEnum::PointerType(i8_ptr),
            retval_storage,
            "result_ptr"
        ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        let result_ptr = if let BasicValueEnum::PointerValue(pv) = result_i8_ptr {
            pv
        } else {
            return Err("expected pointer from pthread_join".into());
        };

        let result_typed = self.builder.build_pointer_cast(
            result_ptr,
            self.context.ptr_type(inkwell::AddressSpace::default()),
            "result_typed_ptr"
        ).map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?;
        let result_val = self.builder.build_load(
            result_type,
            result_typed,
            "spawn_result_val"
        ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;

        let free_fn = self.module.get_function("free")
            .ok_or_else(|| "free not declared".to_string())?;
        self.builder.build_call(free_fn, &[
            BasicMetadataValueEnum::PointerValue(result_ptr),
        ], "free_call")
            .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;

        Ok(result_val)
    }
}
