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
        // Create a poll-based future instead of a pthread.
        // The expression body runs on a real thread via mimi_spawn_future.
        // Returns i8* (future pointer).
        let (future_ptr, result_type) = self.compile_spawn_future(expr, vars)?;
        if self.in_parasteps {
            self.parasteps_future_ptrs.push((future_ptr, result_type));
        }
        self.pending_spawn_type = Some(result_type);
        Ok(BasicValueEnum::PointerValue(future_ptr))
    }

    /// Generate poll-based spawn: creates a wrapper function that evaluates `expr`
    /// on a real thread via mimi_spawn_future.
    fn compile_spawn_future(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>), CompileError> {
        let i8_ty = self.context.i8_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();

        let parent_fn = self.current_function()
            .ok_or_else(|| "codegen: no current function for spawn".to_string())?;
        let parent_name = parent_fn.get_name().to_str().unwrap_or("unknown").to_string();
        let wrapper_name = format!("{}{}__spawn_poll", parent_name, self.spawn_counter).to_string();
        self.spawn_counter += 1;

        // Collect free variables
        let mut free_vars: BTreeMap<String, (inkwell::values::PointerValue<'ctx>, BasicTypeEnum<'ctx>)> = BTreeMap::new();
        let empty_defined = std::collections::HashSet::new();
        self.collect_free_vars_expr(expr, &empty_defined, vars, &mut free_vars);

        // ── Generate poll function void(i8* future_ptr) ──
        let poll_fn_type = void_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
        let poll_fn = self.module.add_function(&wrapper_name, poll_fn_type, None);
        let poll_entry = self.context.append_basic_block(poll_fn, "entry");
        let saved_block = self.builder.get_insert_block();
        self.builder.position_at_end(poll_entry);

        let future_ptr_param = poll_fn.get_nth_param(0)
            .ok_or_else(|| "codegen: spawn poll fn param 0 not found".to_string())?
            .into_pointer_value();

        let mut poll_vars = HashMap::new();
        let mut env_ptr_opt: Option<inkwell::values::PointerValue<'ctx>> = None;
        if !free_vars.is_empty() {
            let env_field_types: Vec<BasicTypeEnum<'ctx>> =
                free_vars.values().map(|&(_, ty)| ty).collect();
            let env_struct_type = self.context.struct_type(&env_field_types, false);

            // Load env_ptr from future+8 (data area holds the env pointer)
            let env_ptr_slot = self.gep().build_gep(i8_ty, future_ptr_param,
                &[i64_ty.const_int(8, false)], "env_ptr_slot")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let env_ptr_typed = self.builder.build_pointer_cast(
                env_ptr_slot, self.context.ptr_type(inkwell::AddressSpace::default()),
                "env_ptr_typed",
            ).map_err(|e| CompileError::LlvmError(format!("cast error: {}", e)))?;
            let env_ptr_val = self.builder.build_load(
                BasicTypeEnum::PointerType(i8_ptr), env_ptr_typed, "env_ptr_val"
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            let env_ptr = if let BasicValueEnum::PointerValue(pv) = env_ptr_val { pv }
                else { return Err("spawn poll: env ptr not a pointer".into()); };
            env_ptr_opt = Some(env_ptr);

            // Unpack env struct fields
            for (i, (name, &(_, ty))) in free_vars.iter().enumerate() {
                let field_gep = self.gep().build_struct_gep(
                    env_struct_type, env_ptr, i as u32, &format!("spawn_env_{}_gep", name),
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let field_val = self.builder.build_load(ty, field_gep, &format!("spawn_cap_{}", name))
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                let alloca = self.builder.build_alloca(ty, &format!("spawn_cap_{}_alloca", name))
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, field_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                poll_vars.insert(name.clone(), (alloca, ty));
            }
        }

        // Evaluate the expression inside the poll function
        let result = self.compile_expr(expr, &poll_vars)?;
        let result_type = result.get_type();

        // Store result at future+8 (this overwrites the env pointer slot at future+8)
        let result_ptr_i8 = self.gep().build_gep(i8_ty, future_ptr_param,
            &[i64_ty.const_int(8, false)], "spawn_result_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let result_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let result_typed_ptr = self.builder.build_pointer_cast(
            result_ptr_i8, result_ptr_type,
            "spawn_result_typed",
        ).map_err(|e| CompileError::LlvmError(format!("cast error: {}", e)))?;
        self.builder.build_store(result_typed_ptr, result)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Set completed
        let set_c_fn = self.module.get_function("mimi_future_set_completed")
            .ok_or_else(|| CompileError::LlvmError("mimi_future_set_completed not declared".into()))?;
        self.builder.build_call(set_c_fn, &[
            BasicMetadataValueEnum::PointerValue(future_ptr_param),
        ], "spawn_set_completed")
            .map_err(|e| CompileError::LlvmError(format!("set_completed error: {}", e)))?;

        // Free env (if any) — use the env_ptr saved BEFORE result overwrote future+8
        if let Some(env_ptr) = env_ptr_opt {
            let free_fn = self.module.get_function("free")
                .ok_or_else(|| "free not declared".to_string())?;
            self.builder.build_call(free_fn, &[
                BasicMetadataValueEnum::PointerValue(env_ptr),
            ], "spawn_free_env")
                .map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;
        }

        self.builder.build_return(None)
            .map_err(|e| CompileError::LlvmError(format!("return error: {}", e)))?;

        // Restore insertion point
        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        // ── At spawn site: allocate future + env, call mimi_spawn_future ──
        let alloc_fn = self.module.get_function("mimi_future_alloc")
            .ok_or_else(|| CompileError::LlvmError("mimi_future_alloc not declared".into()))?;
        // Request at least 76 bytes (MimiFutureRepr size: 8+4+64 = 76, aligned to 8)
        let future_total_size = 8u64 + 64u64; // 8 header + 64 data
        let total_size_val = i64_ty.const_int(future_total_size, false);
        let future_ptr = self.builder.build_call(alloc_fn, &[
            BasicMetadataValueEnum::IntValue(total_size_val),
        ], "spawn_future_alloc")
            .map_err(|e| CompileError::LlvmError(format!("future_alloc error: {}", e)))?
            .try_as_basic_value_opt()
            .map(|v: BasicValueEnum<'ctx>| v.into_pointer_value())
            .ok_or_else(|| CompileError::LlvmError("future_alloc returned non-pointer".into()))?;

        // Store free vars in a separate heap allocation, and store the pointer at future+8
        if !free_vars.is_empty() {
            let env_field_types: Vec<BasicTypeEnum<'ctx>> =
                free_vars.values().map(|&(_, ty)| ty).collect();
            let env_struct_type = self.context.struct_type(&env_field_types, false);
            let env_byte_size = env_struct_type.size_of()
                .ok_or_else(|| "size_of error".to_string())?;
            let malloc_fn = self.module.get_function("malloc")
                .ok_or_else(|| "malloc not declared".to_string())?;
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

            // Store env pointer at future+8
            let env_ptr_slot = self.gep().build_gep(i8_ty, future_ptr,
                &[i64_ty.const_int(8, false)], "env_ptr_slot")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let env_ptr_typed = self.builder.build_pointer_cast(
                env_ptr_slot, self.context.ptr_type(inkwell::AddressSpace::default()),
                "env_ptr_typed",
            ).map_err(|e| CompileError::LlvmError(format!("cast error: {}", e)))?;
            self.builder.build_store(env_ptr_typed, BasicValueEnum::PointerValue(env_heap_ptr))
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        } else {
            // No free vars: store null at future+8
            let env_ptr_slot = self.gep().build_gep(i8_ty, future_ptr,
                &[i64_ty.const_int(8, false)], "null_env_slot")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let env_ptr_typed = self.builder.build_pointer_cast(
                env_ptr_slot, self.context.ptr_type(inkwell::AddressSpace::default()),
                "null_env_typed",
            ).map_err(|e| CompileError::LlvmError(format!("cast error: {}", e)))?;
            self.builder.build_store(env_ptr_typed, BasicValueEnum::PointerValue(i8_ptr.const_null()))
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        }

        // Call mimi_spawn_future(future, poll_fn)
        let spawn_fn = self.module.get_function("mimi_spawn_future")
            .ok_or_else(|| CompileError::LlvmError("mimi_spawn_future not declared".into()))?;
        let poll_fn_as_i8 = self.builder.build_pointer_cast(
            poll_fn.as_global_value().as_pointer_value(),
            i8_ptr,
            "poll_fn_i8",
        ).map_err(|e| CompileError::LlvmError(format!("ptr cast: {}", e)))?;
        self.builder.build_call(spawn_fn, &[
            BasicMetadataValueEnum::PointerValue(future_ptr),
            BasicMetadataValueEnum::PointerValue(poll_fn_as_i8),
        ], "spawn_future_call")
            .map_err(|e| CompileError::LlvmError(format!("spawn_future error: {}", e)))?;

        Ok((future_ptr, result_type))
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
        let handle_val = self.compile_expr(expr, vars)?;

        let (future_ptr, result_type) = match handle_val {
            // Future pointer from spawn or async fn: i8*
            BasicValueEnum::PointerValue(fp) => {
                let ty = self.infer_future_inner_type(expr, vars)
                    .or_else(|| self.pending_spawn_type.take())
                    .unwrap_or_else(|| self.context.i64_type().into());
                (fp, ty)
            }
            _ => {
                return Err(CompileError::Generic(
                    "await requires a future pointer (i8*)".into()
                ));
            }
        };

        // 1. Run single-threaded executor for async fn futures
        let executor_run = self.module.get_function("mimi_executor_run")
            .ok_or_else(|| CompileError::LlvmError("mimi_executor_run not declared".into()))?;
        self.builder.build_call(executor_run, &[], "executor_run")
            .map_err(|e| CompileError::LlvmError(format!("executor_run error: {}", e)))?;

        // 2. Spin-wait on completed flag (for thread-backed spawn futures)
        let await_fn = self.module.get_function("mimi_await_future")
            .ok_or_else(|| CompileError::LlvmError("mimi_await_future not declared".into()))?;
        self.builder.build_call(await_fn, &[
            BasicMetadataValueEnum::PointerValue(future_ptr),
        ], "await_future")
            .map_err(|e| CompileError::LlvmError(format!("await_future error: {}", e)))?;

        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();

        // Load result from future + 8
        let result_data_ptr = self.gep().build_gep(i8_ty, future_ptr,
            &[i64_ty.const_int(8, false)], "result_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
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

        // Free the future (unless inside parasteps — leave_parasteps handles cleanup)
        if !self.in_parasteps {
            let free_fn = self.module.get_function("mimi_future_free")
                .ok_or_else(|| CompileError::LlvmError("mimi_future_free not declared".into()))?;
            self.builder.build_call(free_fn, &[
                BasicMetadataValueEnum::PointerValue(future_ptr),
            ], "future_free")
                .map_err(|e| CompileError::LlvmError(format!("future_free error: {}", e)))?;
        }

        Ok(result_val)
    }
}
