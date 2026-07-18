// Codegen for v0.28.20 concurrency primitives.
//
// Each primitive is a thin wrapper around its corresponding
// `mimi_*` runtime declaration. Handles flow as i64 through the rest of
// codegen, mirroring the interpreter's Value::Int payload.

use super::super::call_try_basic_value;
use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    // ---------- AtomicI32 ----------

    pub(super) fn compile_atomic_i32_new(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("atomic_i32_new expects 1 argument".into());
        }
        let val = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_new: argument must be an integer".into()),
        };
        let func = self
            .module
            .get_function("mimi_atomic_i32_new")
            .ok_or("mimi_atomic_i32_new not declared")?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(val)], "atomic_new")
            .map_err(|e| format!("atomic_i32_new error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_atomic_i32_new returned void")?)
    }

    pub(super) fn compile_atomic_i32_load(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("atomic_i32_load expects 1 argument".into());
        }
        let handle = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_load: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_atomic_i32_load")
            .ok_or("mimi_atomic_i32_load not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(handle)],
                "atomic_load",
            )
            .map_err(|e| format!("atomic_i32_load error: {}", e))?;
        // Runtime returns i32 — after A1 restoration, i32 is a valid native type.
        // Keep the raw i32 value; callers that need i64 will extend via adjust_int_val.
        let raw = call_try_basic_value(&result)
            .ok_or("mimi_atomic_i32_load returned void")?
            .into_int_value();
        Ok(BasicValueEnum::IntValue(raw))
    }

    pub(super) fn compile_atomic_i32_store(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("atomic_i32_store expects 2 arguments".into());
        }
        let handle = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_store: handle must be i64".into()),
        };
        // Input may be i32 (native) or i64 — truncate to i32 for runtime.
        let val_in = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_store: value must be an integer".into()),
        };
        let i32_ty = self.context.i32_type();
        let val_i32 = if val_in.get_type().get_bit_width() > 32 {
            self.builder
                .build_int_truncate(val_in, i32_ty, "atomic_store_trunc")
                .map_err(|e| format!("atomic_i32_store truncate error: {}", e))?
        } else {
            val_in
        };
        let func = self
            .module
            .get_function("mimi_atomic_i32_store")
            .ok_or("mimi_atomic_i32_store not declared")?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(val_i32),
                ],
                "atomic_store",
            )
            .map_err(|e| format!("atomic_i32_store error: {}", e))?;
        // Returns unit; emit a dummy i64 0.
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    pub(super) fn compile_atomic_i32_fetch_add(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("atomic_i32_fetch_add expects 2 arguments".into());
        }
        let handle = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_fetch_add: handle must be i64".into()),
        };
        let delta_in = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_fetch_add: delta must be i64".into()),
        };
        let i32_ty = self.context.i32_type();
        let delta_i32 = if delta_in.get_type().get_bit_width() > 32 {
            self.builder
                .build_int_truncate(delta_in, i32_ty, "fetch_add_trunc")
                .map_err(|e| format!("atomic_i32_fetch_add truncate error: {}", e))?
        } else {
            delta_in
        };
        let func = self
            .module
            .get_function("mimi_atomic_i32_fetch_add")
            .ok_or("mimi_atomic_i32_fetch_add not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(delta_i32),
                ],
                "atomic_fetch_add",
            )
            .map_err(|e| format!("atomic_i32_fetch_add error: {}", e))?;
        let raw = call_try_basic_value(&result)
            .ok_or("mimi_atomic_i32_fetch_add returned void")?
            .into_int_value();
        // Runtime returns i32 — keep raw i32 (A1 restoration).
        Ok(BasicValueEnum::IntValue(raw))
    }

    pub(super) fn compile_atomic_i32_compare_exchange(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err("atomic_i32_compare_exchange expects 3 arguments".into());
        }
        let handle = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_compare_exchange: handle must be i64".into()),
        };
        let i32_ty = self.context.i32_type();
        let exp_in = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("expected i64 expected-value".into()),
        };
        let new_in = match args[2] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("expected i64 new-value".into()),
        };
        let exp = if exp_in.get_type().get_bit_width() > 32 {
            self.builder
                .build_int_truncate(exp_in, i32_ty, "cas_exp_trunc")
                .map_err(|e| format!("cas exp truncate error: {}", e))?
        } else {
            exp_in
        };
        let nv = if new_in.get_type().get_bit_width() > 32 {
            self.builder
                .build_int_truncate(new_in, i32_ty, "cas_nv_trunc")
                .map_err(|e| format!("cas nv truncate error: {}", e))?
        } else {
            new_in
        };
        let func = self
            .module
            .get_function("mimi_atomic_i32_compare_exchange")
            .ok_or("mimi_atomic_i32_compare_exchange not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(handle),
                    BasicMetadataValueEnum::IntValue(exp),
                    BasicMetadataValueEnum::IntValue(nv),
                ],
                "atomic_cas",
            )
            .map_err(|e| format!("cas error: {}", e))?;
        let raw = call_try_basic_value(&result)
            .ok_or("cas returned void")?
            .into_int_value();
        // Runtime returns i32 — keep raw i32 (A1 restoration).
        Ok(BasicValueEnum::IntValue(raw))
    }

    pub(super) fn compile_atomic_drop_helper(
        &self,
        rt_fn: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<()> {
        if args.len() != 1 {
            return Err(format!("{} expects 1 argument", rt_fn).into());
        }
        let handle = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err(format!("{}: handle must be i64", rt_fn).into()),
        };
        let func = self
            .module
            .get_function(rt_fn)
            .ok_or_else(|| format!("{} not declared", rt_fn))?;
        self.builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(handle)],
                "atomic_drop",
            )
            .map_err(|e| format!("{} error: {}", rt_fn, e))?;
        Ok(())
    }

    // ---------- AtomicI64 ----------

    pub(super) fn compile_atomic_i64_new(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("atomic_i64_new expects 1 argument".into());
        }
        let v = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i64_new: argument must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_atomic_i64_new")
            .ok_or("mimi_atomic_i64_new not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(v)],
                "atomic_i64_new",
            )
            .map_err(|e| format!("atomic_i64_new error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_atomic_i64_new returned void")?)
    }

    pub(super) fn compile_atomic_i64_load(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("atomic_i64_load expects 1 argument".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i64_load: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_atomic_i64_load")
            .ok_or("mimi_atomic_i64_load not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(h)],
                "atomic_i64_load",
            )
            .map_err(|e| format!("atomic_i64_load error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_atomic_i64_load returned void")?)
    }

    pub(super) fn compile_atomic_i64_store(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("atomic_i64_store expects 2 arguments".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i64_store: handle must be i64".into()),
        };
        let v = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i64_store: value must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_atomic_i64_store")
            .ok_or("mimi_atomic_i64_store not declared")?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(h),
                    BasicMetadataValueEnum::IntValue(v),
                ],
                "atomic_i64_store",
            )
            .map_err(|e| format!("atomic_i64_store error: {}", e))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    pub(super) fn compile_atomic_i64_fetch_add(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("atomic_i64_fetch_add expects 2 arguments".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i64_fetch_add: handle must be i64".into()),
        };
        let d = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i64_fetch_add: delta must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_atomic_i64_fetch_add")
            .ok_or("mimi_atomic_i64_fetch_add not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(h),
                    BasicMetadataValueEnum::IntValue(d),
                ],
                "atomic_i64_fetch_add",
            )
            .map_err(|e| format!("atomic_i64_fetch_add error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_atomic_i64_fetch_add returned void")?)
    }

    // ---------- AtomicBool ----------

    pub(super) fn compile_atomic_bool_new(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("atomic_bool_new expects 1 argument".into());
        }
        let v = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_bool_new: argument must be i64 (bool as 0/1)".into()),
        };
        // Run-time expects i32; truncate i64 → i32.
        let i32_ty = self.context.i32_type();
        let narrow = self
            .builder
            .build_int_truncate(v, i32_ty, "bool_trunc")
            .map_err(|e| format!("atomic_bool_new trunc error: {}", e))?;
        let func = self
            .module
            .get_function("mimi_atomic_bool_new")
            .ok_or("mimi_atomic_bool_new not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(narrow)],
                "atomic_bool_new",
            )
            .map_err(|e| format!("atomic_bool_new error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_atomic_bool_new returned void")?)
    }

    pub(super) fn compile_atomic_bool_load(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("atomic_bool_load expects 1 argument".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_bool_load: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_atomic_bool_load")
            .ok_or("mimi_atomic_bool_load not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(h)],
                "atomic_bool_load",
            )
            .map_err(|e| format!("atomic_bool_load error: {}", e))?;
        // Runtime returns i32 (0/1); zext to i64 (Mimi bool is i64 in memory).
        let raw = call_try_basic_value(&result)
            .ok_or("void")?
            .into_int_value();
        let i64_ty = self.context.i64_type();
        let zext = self
            .builder
            .build_int_z_extend(raw, i64_ty, "bool_load_zext")
            .map_err(|e| format!("zext error: {}", e))?;
        Ok(BasicValueEnum::IntValue(zext))
    }

    pub(super) fn compile_atomic_bool_store(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("atomic_bool_store expects 2 arguments".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_bool_store: handle must be i64".into()),
        };
        let v = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_bool_store: value must be i64".into()),
        };
        let i32_ty = self.context.i32_type();
        let narrow = self
            .builder
            .build_int_truncate(v, i32_ty, "bool_store_trunc")
            .map_err(|e| format!("trunc error: {}", e))?;
        let func = self
            .module
            .get_function("mimi_atomic_bool_store")
            .ok_or("mimi_atomic_bool_store not declared")?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(h),
                    BasicMetadataValueEnum::IntValue(narrow),
                ],
                "atomic_bool_store",
            )
            .map_err(|e| format!("atomic_bool_store error: {}", e))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    // ---------- Mutex ----------

    pub(super) fn compile_mutex_new(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("mutex_new expects 1 argument".into());
        }
        let v = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("mutex_new: argument must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_mutex_new")
            .ok_or("mimi_mutex_new not declared")?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(v)], "mutex_new")
            .map_err(|e| format!("mutex_new error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_mutex_new returned void")?)
    }

    pub(super) fn compile_mutex_lock(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("mutex_lock expects 1 argument".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("mutex_lock: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_mutex_lock")
            .ok_or("mimi_mutex_lock not declared")?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(h)], "mutex_lock")
            .map_err(|e| format!("mutex_lock error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_mutex_lock returned void")?)
    }

    pub(super) fn compile_mutex_get(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("mutex_get expects 1 argument".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("mutex_get: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_mutex_get")
            .ok_or("mimi_mutex_get not declared")?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(h)], "mutex_get")
            .map_err(|e| format!("mutex_get error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_mutex_get returned void")?)
    }

    pub(super) fn compile_mutex_set(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("mutex_set expects 2 arguments".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("mutex_set: handle must be i64".into()),
        };
        let v = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("mutex_set: value must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_mutex_set")
            .ok_or("mimi_mutex_set not declared")?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(h),
                    BasicMetadataValueEnum::IntValue(v),
                ],
                "mutex_set",
            )
            .map_err(|e| format!("mutex_set error: {}", e))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    pub(super) fn compile_mutex_unlock(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("mutex_unlock expects 1 argument".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("mutex_unlock: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_mutex_unlock")
            .ok_or("mimi_mutex_unlock not declared")?;
        self.builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(h)], "mutex_unlock")
            .map_err(|e| format!("mutex_unlock error: {}", e))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    // ---------- Channel ----------

    pub(super) fn compile_channel_new(
        &self,
        _args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let func = self
            .module
            .get_function("mimi_channel_new")
            .ok_or("mimi_channel_new not declared")?;
        let result = self
            .builder
            .build_call(func, &[], "channel_new")
            .map_err(|e| format!("channel_new error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_channel_new returned void")?)
    }

    pub(super) fn compile_channel_send(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("channel_send expects 2 arguments".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("channel_send: handle must be i64".into()),
        };
        let v = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("channel_send: value must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_channel_send")
            .ok_or("mimi_channel_send not declared")?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(h),
                    BasicMetadataValueEnum::IntValue(v),
                ],
                "channel_send",
            )
            .map_err(|e| format!("channel_send error: {}", e))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    pub(super) fn compile_channel_recv(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("channel_recv expects 1 argument".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("channel_recv: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_channel_recv")
            .ok_or("mimi_channel_recv not declared")?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(h)], "channel_recv")
            .map_err(|e| format!("channel_recv error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_channel_recv returned void")?)
    }

    pub(super) fn compile_channel_try_recv(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("channel_try_recv expects 1 argument".into());
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("channel_try_recv: handle must be i64".into()),
        };
        let func = self
            .module
            .get_function("mimi_channel_try_recv")
            .ok_or("mimi_channel_try_recv not declared")?;
        let result = self
            .builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(h)],
                "channel_try_recv",
            )
            .map_err(|e| format!("channel_try_recv error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_channel_try_recv returned void")?)
    }

    /// v0.29.21: actor_mailbox_depth / actor_is_muted → runtime query.
    pub(super) fn compile_actor_mailbox_query(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        runtime_name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(format!("{} expects actor handle", runtime_name).into());
        }
        let func = self
            .module
            .get_function(runtime_name)
            .ok_or_else(|| format!("{} not declared", runtime_name))?;
        let result = self
            .builder
            .build_call(func, &[args[0]], "actor_bp_query")
            .map_err(|e| format!("{} error: {}", runtime_name, e))?;
        Ok(call_try_basic_value(&result)
            .ok_or_else(|| format!("{} returned void", runtime_name))?)
    }

    /// v0.29.21: actor_set_mailbox_depth(handle, depth).
    pub(super) fn compile_actor_set_mailbox_depth(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() < 2 {
            return Err("actor_set_mailbox_depth expects (handle, depth)".into());
        }
        let func = self
            .module
            .get_function("mimi_actor_set_mailbox_depth")
            .ok_or("mimi_actor_set_mailbox_depth not declared")?;
        self.builder
            .build_call(func, &[args[0], args[1]], "actor_set_mb_depth")
            .map_err(|e| format!("actor_set_mailbox_depth error: {}", e))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    /// v0.29.24: actor_set_max_children(n) — 0 = unlimited.
    pub(super) fn compile_actor_set_max_children(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err("actor_set_max_children expects 1 argument".into());
        }
        let func = self
            .module
            .get_function("mimi_actor_set_max_children")
            .ok_or("mimi_actor_set_max_children not declared")?;
        self.builder
            .build_call(func, &[args[0]], "actor_set_max_children")
            .map_err(|e| format!("actor_set_max_children error: {}", e))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    pub(super) fn compile_actor_spawn_count(&self) -> MimiResult<BasicValueEnum<'ctx>> {
        let func = self
            .module
            .get_function("mimi_actor_spawn_count")
            .ok_or("mimi_actor_spawn_count not declared")?;
        let result = self
            .builder
            .build_call(func, &[], "actor_spawn_count")
            .map_err(|e| format!("actor_spawn_count error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_actor_spawn_count returned void")?)
    }

    pub(super) fn compile_actor_max_children(&self) -> MimiResult<BasicValueEnum<'ctx>> {
        let func = self
            .module
            .get_function("mimi_actor_max_children")
            .ok_or("mimi_actor_max_children not declared")?;
        let result = self
            .builder
            .build_call(func, &[], "actor_max_children")
            .map_err(|e| format!("actor_max_children error: {}", e))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_actor_max_children returned void")?)
    }

    /// v0.29.25: broadcast(targets: List, method: string) -> List<i64>
    ///
    /// Type-erased polymorphic dispatch over a list of actor handles.
    /// List slots store actor handles as ptrtoint(i64). Method resolved by name.
    pub(super) fn compile_broadcast(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        use inkwell::IntPredicate;
        if args.len() != 2 {
            return Err("broadcast expects 2 arguments (targets, method_name)".into());
        }
        let list_ptr = self.require_list_pointer(args[0], "broadcast")?;
        let method_basic: BasicValueEnum = match args[1] {
            BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
            BasicMetadataValueEnum::StructValue(sv) => sv.into(),
            BasicMetadataValueEnum::IntValue(iv) => iv.into(),
            other => return Err(format!("broadcast: unexpected method arg {:?}", other).into()),
        };
        let method_c = self
            .extract_string_ptr(&method_basic)
            .ok_or("broadcast: method name must be a string")?;

        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let len = self.load_list_len(list_ptr)?;
        let data_i64 = self.load_list_data_i64(list_ptr)?;

        // handles: i8** = malloc(len * 8)  (B4: NULL-checked malloc)
        let bytes = self
            .builder
            .build_int_mul(len, i64_ty.const_int(8, false), "handles_bytes")
            .map_err(|e| format!("mul: {}", e))?;
        let handles_arr = self.malloc_or_abort(bytes, "handles_malloc")?;

        let function = self
            .builder
            .get_insert_block()
            .ok_or("broadcast: no insert block")?
            .get_parent()
            .ok_or("broadcast: no parent function")?;
        let loop_bb = self.context.append_basic_block(function, "bc_loop");
        let body_bb = self.context.append_basic_block(function, "bc_body");
        let exit_bb = self.context.append_basic_block(function, "bc_exit");
        let idx_a = self
            .builder
            .build_alloca(i64_ty, "bc_idx")
            .map_err(|e| format!("alloca: {}", e))?;
        self.builder
            .build_store(idx_a, i64_ty.const_int(0, false))
            .map_err(|e| format!("store: {}", e))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| format!("br: {}", e))?;

        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(i64_ty, idx_a, "idx")
            .map_err(|e| format!("load: {}", e))?
            .into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "bc_cmp")
            .map_err(|e| format!("cmp: {}", e))?;
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .map_err(|e| format!("cbr: {}", e))?;

        self.builder.position_at_end(body_bb);
        // SAFETY: `idx` is bounded by the loop condition (0..len), and
        // `data_i64` is a valid pointer to an i64 array of at least `len`
        // elements. The GEP is in-bounds by construction.
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(i64_ty, data_i64, &[idx], "elem_ptr")
                .map_err(|e| format!("gep: {}", e))?
        };
        let elem = self
            .builder
            .build_load(i64_ty, elem_ptr, "elem")
            .map_err(|e| format!("load: {}", e))?
            .into_int_value();
        let hptr = self
            .builder
            .build_int_to_ptr(elem, i8_ptr, "handle")
            .map_err(|e| format!("i2p: {}", e))?;
        // SAFETY: `idx` is bounded by the loop condition (0..len), and
        // `handles_arr` is a valid pointer to an i8* array of at least `len`
        // elements. The GEP is in-bounds by construction.
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ptr, handles_arr, &[idx], "hslot")
                .map_err(|e| format!("gep: {}", e))?
        };
        self.builder
            .build_store(slot, hptr)
            .map_err(|e| format!("store: {}", e))?;
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| format!("add: {}", e))?;
        self.builder
            .build_store(idx_a, next)
            .map_err(|e| format!("store: {}", e))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| format!("br: {}", e))?;

        self.builder.position_at_end(exit_bb);
        let out_len_a = self
            .builder
            .build_alloca(i64_ty, "out_len")
            .map_err(|e| format!("alloca: {}", e))?;
        self.builder
            .build_store(out_len_a, i64_ty.const_int(0, false))
            .map_err(|e| format!("store: {}", e))?;

        let bc_fn = self
            .module
            .get_function("mimi_broadcast")
            .ok_or("mimi_broadcast not declared")?;
        let results_call = self
            .builder
            .build_call(
                bc_fn,
                &[
                    handles_arr.into(),
                    len.into(),
                    method_c.into(),
                    out_len_a.into(),
                ],
                "broadcast_call",
            )
            .map_err(|e| format!("broadcast: {}", e))?;
        let results_ptr = call_try_basic_value(&results_call)
            .ok_or("mimi_broadcast void")?
            .into_pointer_value();
        let out_len = self
            .builder
            .build_load(i64_ty, out_len_a, "out_len_v")
            .map_err(|e| format!("load: {}", e))?
            .into_int_value();
        if let Some(free_fn) = self.module.get_function("free") {
            let _ = self
                .builder
                .build_call(free_fn, &[handles_arr.into()], "free_handles");
        }
        // results_ptr is *mut i64; store as list data (i8*)
        let data_out = self
            .builder
            .build_bit_cast(results_ptr, i8_ptr, "results_i8")
            .map_err(|e| format!("cast: {}", e))?
            .into_pointer_value();
        let list_out = self.alloc_list_result(out_len, data_out)?;
        Ok(BasicValueEnum::PointerValue(list_out))
    }

    /// v0.29.34: session_send(ch, val) — delegates to mimi_channel_send.
    /// M7: errors use CompileError variants (not bare string).
    pub(super) fn compile_session_send(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "session_send expects 2 arguments".into(),
            ));
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "session_send: handle must be i64".into(),
                ))
            }
        };
        let v = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "session_send: value must be i64".into(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_channel_send")
            .ok_or_else(|| CompileError::UndefinedFunc("mimi_channel_send".into()))?;
        self.builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(h),
                    BasicMetadataValueEnum::IntValue(v),
                ],
                "session_send",
            )
            .map_err(|e| CompileError::LlvmError(format!("session_send error: {}", e)))?;
        // M5-note: returning i64(0) is equivalent to unit in codegen — unit
        // values are represented as zero-width i64 in the LLVM backend. The
        // interp returns Value::Unit which is also 0 when used in arithmetic.
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    /// v0.29.34: session_recv(ch) — delegates to mimi_channel_recv.
    pub(super) fn compile_session_recv(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "session_recv expects 1 argument".into(),
            ));
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "session_recv: handle must be i64".into(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_channel_recv")
            .ok_or_else(|| CompileError::UndefinedFunc("mimi_channel_recv".into()))?;
        let result = self
            .builder
            .build_call(func, &[BasicMetadataValueEnum::IntValue(h)], "session_recv")
            .map_err(|e| CompileError::LlvmError(format!("session_recv error: {}", e)))?;
        call_try_basic_value(&result)
            .ok_or_else(|| CompileError::LlvmError("mimi_channel_recv returned void".into()))
    }

    /// v0.29.34: session_close(ch) — delegates to mimi_channel_drop.
    pub(super) fn compile_session_close(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "session_close expects 1 argument".into(),
            ));
        }
        let h = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "session_close: handle must be i64".into(),
                ))
            }
        };
        let func = self
            .module
            .get_function("mimi_channel_drop")
            .ok_or_else(|| CompileError::UndefinedFunc("mimi_channel_drop".into()))?;
        self.builder
            .build_call(
                func,
                &[BasicMetadataValueEnum::IntValue(h)],
                "session_close",
            )
            .map_err(|e| CompileError::LlvmError(format!("session_close error: {}", e)))?;
        Ok(BasicValueEnum::IntValue(
            self.context.i64_type().const_int(0, false),
        ))
    }

    pub(super) fn compile_session_open(
        &self,
        _args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let f = self
            .module
            .get_function("mimi_session_pair")
            .ok_or_else(|| CompileError::UndefinedFunc("mimi_session_pair".into()))?;
        let pair = self
            .builder
            .build_call(f, &[], "sp")
            .map_err(|e| CompileError::LlvmError(format!("session_pair: {}", e)))?;
        let packed = call_try_basic_value(&pair)
            .ok_or_else(|| CompileError::LlvmError("session_pair returned void".into()))?
            .into_int_value();
        let lo_f = self
            .module
            .get_function("mimi_session_lo")
            .ok_or_else(|| CompileError::UndefinedFunc("mimi_session_lo".into()))?;
        let hi_f = self
            .module
            .get_function("mimi_session_hi")
            .ok_or_else(|| CompileError::UndefinedFunc("mimi_session_hi".into()))?;
        let lo = call_try_basic_value(
            &self
                .builder
                .build_call(lo_f, &[packed.into()], "lo")
                .map_err(|e| CompileError::LlvmError(format!("session_lo: {}", e)))?,
        )
        .ok_or_else(|| CompileError::LlvmError("session_lo returned void".into()))?
        .into_int_value();
        let hi = call_try_basic_value(
            &self
                .builder
                .build_call(hi_f, &[packed.into()], "hi")
                .map_err(|e| CompileError::LlvmError(format!("session_hi: {}", e)))?,
        )
        .ok_or_else(|| CompileError::LlvmError("session_hi returned void".into()))?
        .into_int_value();
        let i64_ty = self.context.i64_type();
        // CG-C2: use malloc_or_abort instead of bare build_array_malloc (OOM → null deref).
        let data = self.malloc_or_abort(i64_ty.const_int(16, false), "spd")?;
        // SAFETY (M9): in_bounds GEP with indices 0 and 1 on a freshly allocated
        // 2-element i64 array; stores write only within that allocation.
        // SAFETY: data is non-null (malloc_or_abort); indices 0/1 in 16-byte block.
        unsafe {
            self.builder
                .build_store(
                    self.builder
                        .build_in_bounds_gep(i64_ty, data, &[i64_ty.const_int(0, false)], "s0")
                        .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
                    lo,
                )
                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
            self.builder
                .build_store(
                    self.builder
                        .build_in_bounds_gep(i64_ty, data, &[i64_ty.const_int(1, false)], "s1")
                        .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
                    hi,
                )
                .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
        }
        let di8 = self
            .builder
            .build_bit_cast(
                data,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "spi8",
            )
            .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?
            .into_pointer_value();
        Ok(BasicValueEnum::PointerValue(
            self.alloc_list_result(i64_ty.const_int(2, false), di8)?,
        ))
    }

    // ── v0.29.44: Shadow memory tagging codegen ───────────────────────

    /// shadow_alloc(size: i64, tag: i64, label: string) -> i64 (pointer)
    pub(super) fn compile_shadow_alloc(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err("shadow_alloc expects 3 arguments".into());
        }
        let size = args[0].into_int_value();
        let tag = self
            .builder
            .build_int_truncate(args[1].into_int_value(), self.context.i8_type(), "tag_i8")
            .map_err(|e| format!("trunc: {}", e))?;
        let label_ptr = args[2].into_pointer_value();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let usize_ty = self.context.i64_type(); // size_t on 64-bit
        let fn_ty = usize_ty.fn_type(
            &[
                BasicMetadataTypeEnum::IntType(usize_ty),
                BasicMetadataTypeEnum::IntType(self.context.i8_type()),
                BasicMetadataTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );
        let func = self
            .module
            .get_function("mimi_shadow_alloc")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_shadow_alloc",
                    fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let call = self
            .builder
            .build_call(
                func,
                &[size.into(), tag.into(), label_ptr.into()],
                "shadow_alloc",
            )
            .map_err(|e| format!("shadow_alloc: {}", e))?;
        // TYS-C4: never unwrap call results — propagate as CompileError.
        let v = call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError("shadow_alloc returned void".into()))?;
        Ok(v.into_int_value().into())
    }

    /// Generic shadow_tag/check/free — calls a runtime function with i64 args.
    pub(super) fn compile_shadow_simple(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        fn_name: &str,
        expected_arg_count: usize,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != expected_arg_count {
            return Err(format!("{} expects {} arguments", fn_name, expected_arg_count).into());
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let ret_ty = self.context.i32_type();
        let mut param_types: Vec<BasicMetadataTypeEnum> = Vec::new();
        if fn_name == "mimi_shadow_tag" || fn_name == "mimi_shadow_check" {
            param_types.push(BasicMetadataTypeEnum::PointerType(i8_ptr)); // ptr
            param_types.push(BasicMetadataTypeEnum::IntType(self.context.i8_type()));
        // tag
        } else if fn_name == "mimi_shadow_free" {
            param_types.push(BasicMetadataTypeEnum::PointerType(i8_ptr)); // ptr
        }
        let fn_ty = ret_ty.fn_type(&param_types, false);
        let func = self.module.get_function(fn_name).unwrap_or_else(|| {
            self.module
                .add_function(fn_name, fn_ty, Some(inkwell::module::Linkage::External))
        });
        let mut call_args: Vec<BasicMetadataValueEnum> = Vec::new();
        if fn_name == "mimi_shadow_tag" || fn_name == "mimi_shadow_check" {
            let ptr_int = args[0].into_int_value();
            let ptr = self
                .builder
                .build_int_to_ptr(ptr_int, i8_ptr, "ptr_cast")
                .map_err(|e| format!("inttoptr: {}", e))?;
            let tag = self
                .builder
                .build_int_truncate(args[1].into_int_value(), self.context.i8_type(), "tag_i8")
                .map_err(|e| format!("trunc: {}", e))?;
            call_args.push(ptr.into());
            call_args.push(tag.into());
        } else if fn_name == "mimi_shadow_free" {
            let ptr_int = args[0].into_int_value();
            let ptr = self
                .builder
                .build_int_to_ptr(ptr_int, i8_ptr, "ptr_cast")
                .map_err(|e| format!("inttoptr: {}", e))?;
            call_args.push(ptr.into());
        }
        let call = self
            .builder
            .build_call(func, &call_args, fn_name)
            .map_err(|e| format!("{}: {}", fn_name, e))?;
        // TYS-C4: never unwrap call results — propagate as CompileError.
        let v = call_try_basic_value(&call)
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", fn_name)))?;
        Ok(v.into_int_value().into())
    }
}
