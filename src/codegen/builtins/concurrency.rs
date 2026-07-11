// Codegen for v0.28.20 concurrency primitives.
//
// Each primitive is a thin wrapper around its corresponding
// `mimi_*` runtime declaration. Handles flow as i64 through the rest of
// codegen, mirroring the interpreter's Value::Int payload.

use super::super::call_try_basic_value;
use super::CodeGenerator;
use crate::error::MimiResult;
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
        // Runtime returns i32; sext to i64 (Mimi's integer width).
        let raw = call_try_basic_value(&result)
            .ok_or("mimi_atomic_i32_load returned void")?
            .into_int_value();
        let i64_ty = self.context.i64_type();
        let sext = self
            .builder
            .build_int_s_extend(raw, i64_ty, "atomic_i32_load_sext")
            .map_err(|e| format!("atomic_i32_load sext error: {}", e))?;
        Ok(BasicValueEnum::IntValue(sext))
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
        // The user passes an i64 value (Mimi integer width); runtime expects
        // i32, so truncate.
        let val_i64 = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_store: value must be an integer".into()),
        };
        let i32_ty = self.context.i32_type();
        let val_i32 = self
            .builder
            .build_int_truncate(val_i64, i32_ty, "atomic_store_trunc")
            .map_err(|e| format!("atomic_i32_store truncate error: {}", e))?;
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
        let delta_i64 = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("atomic_i32_fetch_add: delta must be i64".into()),
        };
        let i32_ty = self.context.i32_type();
        let delta_i32 = self
            .builder
            .build_int_truncate(delta_i64, i32_ty, "fetch_add_trunc")
            .map_err(|e| format!("atomic_i32_fetch_add truncate error: {}", e))?;
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
        let i64_ty = self.context.i64_type();
        let sext = self
            .builder
            .build_int_s_extend(raw, i64_ty, "atomic_fetch_add_sext")
            .map_err(|e| format!("sext error: {}", e))?;
        Ok(BasicValueEnum::IntValue(sext))
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
        let exp_i64 = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("expected i64 expected-value".into()),
        };
        let new_i64 = match args[2] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => return Err("expected i64 new-value".into()),
        };
        let exp = self
            .builder
            .build_int_truncate(exp_i64, i32_ty, "cas_exp_trunc")
            .map_err(|e| format!("cas exp truncate error: {}", e))?;
        let nv = self
            .builder
            .build_int_truncate(new_i64, i32_ty, "cas_nv_trunc")
            .map_err(|e| format!("cas nv truncate error: {}", e))?;
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
        let i64_ty = self.context.i64_type();
        let sext = self
            .builder
            .build_int_s_extend(raw, i64_ty, "cas_sext")
            .map_err(|e| format!("cas sext error: {}", e))?;
        Ok(BasicValueEnum::IntValue(sext))
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
        Ok(call_try_basic_value(&result).ok_or_else(|| format!("{} returned void", runtime_name))?)
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

    pub(super) fn compile_actor_spawn_count(
        &self,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
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

    pub(super) fn compile_actor_max_children(
        &self,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
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
}
