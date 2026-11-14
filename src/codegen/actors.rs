use crate::ast::*;
use crate::codegen::call_try_basic_value;
use std::collections::HashMap;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use crate::error::{CompileError, MimiResult};

use super::CodeGenerator;
use super::VarEntry;

/// Mailbox blob capacity in bytes. MUST stay in sync with
/// `MIMI_ACTOR_BLOB_SIZE` in src/runtime/actor.rs: the worker's result blob is
/// that size and `mimi_actor_call` caps the result copy at it. Packing past
/// this bound overwrites the stack (args/result blobs are stack allocas on the
/// caller side, a fixed vec on the worker side), so codegen traps instead.
const ACTOR_MAILBOX_BLOB_CAPACITY: u64 = 256;

impl<'ctx> CodeGenerator<'ctx> {
    /// ABI slot size for actor mailbox packing: natural type size, rounded up
    /// to 8-byte alignment so mixed scalar/struct args stay aligned.
    pub(in crate::codegen) fn actor_abi_slot_size(&self, ty: BasicTypeEnum<'ctx>) -> u64 {
        let size = self.llvm_type_size_bytes(ty).max(1);
        size.div_ceil(8) * 8
    }

    /// Single-source LLVM lowering for actor method parameter/return types
    /// crossing the mailbox ABI. 0.34.36 (audit §6.6): the call-site pack,
    /// the dispatch unpack, AND the `{Actor}__{method}__method` signature must
    /// agree on the exact type. Previously pack/unpack used
    /// `types::mimi_type_to_llvm`, whose `_` arm maps any named record to a
    /// bare i64 slot while the call site stored the full struct value into
    /// that 8-byte accounting — overlapping writes + wrong-sized reads. The
    /// registry-backed `llvm_type_for` resolves named records/enums to their
    /// real struct layout.
    pub(in crate::codegen) fn actor_abi_type_for(
        &self,
        ty: &crate::ast::Type,
    ) -> BasicTypeEnum<'ctx> {
        self.llvm_type_for(ty)
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()))
    }

    /// Emit an unconditional runtime abort with a clear message (used when a
    /// statically oversized mailbox pack is detected). The abort block is
    /// followed by `unreachable`; control continues in a fresh block so any
    /// subsequent (dead) IR stays well-formed. 0.34.36 (audit §6.6): replaced
    /// the silent stack overflow of the fixed-size blobs.
    fn emit_actor_mailbox_overflow_abort(&mut self, msg: &str, tag: &str) -> MimiResult<()> {
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for actor blob guard".into())
        })?;
        let abort_bb = self
            .context
            .append_basic_block(function, &format!("{}_mailbox_overflow", tag));
        let cont_bb = self
            .context
            .append_basic_block(function, &format!("{}_mailbox_ok", tag));
        self.build_br(abort_bb)?;
        self.builder.position_at_end(abort_bb);
        let abort_fn = self.get_or_declare_abort_fn();
        let msg_ptr = self
            .builder
            .build_global_string_ptr(msg, &format!("{}_mailbox_msg", tag))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(
                msg_ptr.as_pointer_value(),
            )],
            &format!("{}_mailbox_abort", tag),
        )?;
        // SAFETY: mimi_runtime_abort is marked noreturn; this block is
        // unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreach: {}", e)))?;
        self.builder.position_at_end(cont_bb);
        Ok(())
    }

    fn is_actor_string_abi_type(ty: BasicTypeEnum<'ctx>) -> bool {
        match ty {
            BasicTypeEnum::StructType(st) => {
                let fields = st.get_field_types();
                matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(it)]
                        if it.get_bit_width() == 64
                )
            }
            _ => false,
        }
    }

    /// v0.28.19: Compile an actor method call via the mailbox.
    ///
    /// This is called from `compile_method_call` when the object is an actor type.
    /// It handles:
    /// 1. Self-call detection (execute directly to avoid deadlock)
    /// 2. Cross-actor call (send to mailbox via mimi_actor_call)
    ///
    /// Returns the method result value, or None if this wasn't an actor method call.
    pub(in crate::codegen) fn try_compile_actor_mailbox_call(
        &mut self,
        obj: &Expr,
        method_name: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
        let obj_type = self.infer_object_type(obj, vars);

        // Check if this is an actor type.
        if !self.actor_names.contains(&obj_type) {
            return Ok(None);
        }

        // Look up the method ID and declared return type.
        let method_key = format!("{}::{}", obj_type, method_name);
        let method_id = match self.actor_method_ids.get(&method_key) {
            Some(&id) => id,
            None => return Ok(None), // Not an actor method, fall through
        };

        // Find the actor's method declaration to recover the declared return type.
        // The dispatch result is always packed as i64 in result_blob; we have to
        // re-shape the i64 to match the declared return type at the call site.
        let method_ret_ty: Option<crate::ast::Type> =
            self.actor_defs.get(&obj_type).and_then(|a| {
                a.methods
                    .iter()
                    .find(|m| m.name == method_name)
                    .and_then(|m| m.ret.clone())
            });
        // Get the actor handle value (i8*).
        let handle_val = self.compile_expr(obj, vars)?;
        let handle_ptr = if let BasicValueEnum::PointerValue(p) = handle_val {
            p
        } else {
            // Not a pointer — fall back to direct call (legacy struct-by-value path)
            return Ok(None);
        };

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();

        // Self-call detection: if mimi_actor_current_id() == mimi_actor_id(handle),
        // execute the method directly (avoid mailbox deadlock).
        let current_id_fn = self.get_runtime_fn("mimi_actor_current_id")?;
        let actor_id_fn = self.get_runtime_fn("mimi_actor_id")?;

        let current_id = self.build_call(current_id_fn, &[], "cur_actor_id")?;
        let current_id =
            call_try_basic_value(&current_id).unwrap_or(i64_ty.const_int(0, false).into());
        let actor_id = self.build_call(actor_id_fn, &[handle_ptr.into()], "handle_actor_id")?;
        let actor_id = call_try_basic_value(&actor_id).unwrap_or(i64_ty.const_int(0, false).into());

        let is_self_call = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                current_id.into_int_value(),
                actor_id.into_int_value(),
                "is_self_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("icmp error: {}", e)))?;

        // Create blocks for self-call vs mailbox-call.
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for actor method call".to_string())
        })?;
        let self_call_bb = self.context.append_basic_block(function, "actor_self_call");
        let mailbox_bb = self
            .context
            .append_basic_block(function, "actor_mailbox_call");
        let merge_bb = self
            .context
            .append_basic_block(function, "actor_call_merge");

        self.build_cond_br(is_self_call, self_call_bb, mailbox_bb)?;

        // ── Self-call path: direct method invocation ──
        self.builder.position_at_end(self_call_bb);

        // For self-call, we need the self_fields_ptr. But the handle doesn't
        // directly expose it. In the interpreter, the worker has the fields.
        // In codegen, the dispatch function is called with self_fields_ptr on
        // the worker thread. When a method calls another method on self,
        // `self` is the fields pointer (not the handle).
        //
        // Actually, in actor method codegen, `self` is bound as a pointer to
        // the fields struct. So `self.method()` inside an actor method would
        // go through this path. But `self` is the fields pointer, not the
        // handle. We need to handle this differently.
        //
        // For now: if the object is `self`, we can call the method directly
        // using the self pointer we already have.
        if let Expr::Ident(name) = obj.unlocated() {
            if name == "self" {
                // Direct call on self — call the method function directly.
                let mangled = format!("{}__{}__method", obj_type, method_name);
                if let Some(method_fn) = self.module.get_function(&mangled) {
                    let self_ptr =
                        vars.get("self").map(|&(alloca, _)| alloca).ok_or_else(|| {
                            CompileError::LlvmError("self not found in vars for self-call".into())
                        })?;
                    // Load the self pointer from the alloca.
                    let self_val = self.build_load(
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        self_ptr,
                        "self_ptr_load",
                    )?;

                    let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
                        vec![self_val.into()];
                    for (arg_idx, arg) in args.iter().enumerate() {
                        let mut val = self.compile_expr(arg, vars)?;
                        // 0.34.36 (audit §6.6): the signature now uses
                        // registry-resolved layouts — struct params cross by
                        // value, while record expressions are pointer-valued.
                        // Load through the declared param type so the direct
                        // call matches the signature.
                        if let (Some(param), BasicValueEnum::PointerValue(pv)) =
                            (method_fn.get_nth_param((arg_idx + 1) as u32), val)
                        {
                            if let BasicTypeEnum::StructType(st) = param.get_type() {
                                val = self.build_load(
                                    BasicTypeEnum::StructType(st),
                                    pv,
                                    &format!("self_arg_load_{}", arg_idx),
                                )?;
                            }
                        }
                        call_args.push(val.into());
                    }
                    let call = self.build_call(method_fn, &call_args, "self_method_call")?;
                    let result =
                        call_try_basic_value(&call).unwrap_or(i64_ty.const_int(0, false).into());

                    // Branch to merge with result. 0.34.36 (audit §6.6): the
                    // staging slot must carry the method's actual return type
                    // (record returns are no longer squashed to i64).
                    let self_ret_ty = method_fn
                        .get_type()
                        .get_return_type()
                        .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                    let result_alloca = self.build_alloca(self_ret_ty, "self_call_result")?;
                    self.build_store(result_alloca, result)?;
                    self.build_br(merge_bb)?;

                    // ── Mailbox path (unreachable for self, but needed for struct) ──
                    self.builder.position_at_end(mailbox_bb);
                    self.build_br(merge_bb)?;

                    // ── Merge ──
                    self.builder.position_at_end(merge_bb);
                    let merged = self.build_load(self_ret_ty, result_alloca, "merged_result")?;
                    return Ok(Some(merged));
                }
            }
        }

        // For non-self cross-actor calls, we still need the direct path for
        // when current_id matches. But we can't easily get the fields ptr from
        // the handle. So for self-calls on non-self objects, fall through to
        // mailbox (which will work since the worker is different).
        // Actually, a self-call means we're ON the worker thread for this actor.
        // If the object is a different variable (not "self"), it could be
        // another actor handle stored in a field. In that case, the current
        // worker is for a different actor, so it's NOT a self-call.
        //
        // The only real self-call scenario is `self.method()` inside an actor
        // method, which we handle above. So the self_call_bb for non-self
        // objects is effectively unreachable — but we still need valid IR.
        self.build_br(mailbox_bb)?;

        // ── Mailbox path: send to mailbox and await reply ──
        self.builder.position_at_end(mailbox_bb);

        // Pack args into a blob with natural type sizes (R-C6 / CG-H6).
        // Must match compile_actor_dispatch unpack layout.
        let args_blob = self.build_alloca(
            self.context
                .i8_type()
                .array_type(ACTOR_MAILBOX_BLOB_CAPACITY as u32),
            "actor_args_blob",
        )?;

        let method_params: Vec<crate::ast::Type> = self
            .actor_defs
            .get(&obj_type)
            .and_then(|a| a.methods.iter().find(|m| m.name == method_name))
            .map(|m| m.params.iter().map(|p| p.ty.clone()).collect())
            .unwrap_or_default();

        // 0.34.36 (audit §6.6): the packed size is statically known — guard it
        // BEFORE emitting any stores so an oversized call aborts with a clear
        // message instead of silently overrunning the stack blob.
        let packed_args_size: u64 = method_params
            .iter()
            .map(|t| self.actor_abi_slot_size(self.actor_abi_type_for(t)))
            .sum();
        if packed_args_size > ACTOR_MAILBOX_BLOB_CAPACITY {
            self.emit_actor_mailbox_overflow_abort(
                &format!(
                    "actor mailbox overflow: call of {}.{} packs {} bytes of arguments; capacity is {} bytes",
                    obj_type, method_name, packed_args_size, ACTOR_MAILBOX_BLOB_CAPACITY
                ),
                &format!("call_{}_{}", obj_type, method_name),
            )?;
        }

        let mut blob_offset: u64 = 0;
        for (i, arg) in args.iter().enumerate() {
            let val = self.compile_expr(arg, vars)?;
            // actor_abi_type_for (registry-backed): identical lowering to the
            // dispatch unpack and the method signature (audit §6.6). Named
            // records pack as their real struct layout, not bare i64.
            let param_ty = method_params.get(i).map(|t| self.actor_abi_type_for(t));
            let store_ty = param_ty.unwrap_or_else(|| match val {
                BasicValueEnum::IntValue(iv) => BasicTypeEnum::IntType(iv.get_type()),
                BasicValueEnum::FloatValue(fv) => BasicTypeEnum::FloatType(fv.get_type()),
                BasicValueEnum::PointerValue(pv) => BasicTypeEnum::PointerType(pv.get_type()),
                BasicValueEnum::StructValue(sv) => BasicTypeEnum::StructType(sv.get_type()),
                _ => BasicTypeEnum::IntType(i64_ty),
            });
            let slot_size = self.actor_abi_slot_size(store_ty);
            let offset = i64_ty.const_int(blob_offset, false);
            let gep = self
                .gep()
                .build_in_bounds_gep(
                    self.context.i8_type(),
                    args_blob,
                    &[offset],
                    &format!("arg_gep_{}", i),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;

            let cast_ptr = self
                .builder
                .build_bit_cast(
                    gep,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    &format!("arg_cast_{}", i),
                )
                .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                .into_pointer_value();

            // Store natural value layout; widen narrow ints to declared width.
            match (val, store_ty) {
                (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(t)) => {
                    let stored = if iv.get_type().get_bit_width() < t.get_bit_width() {
                        if iv.get_type().get_bit_width() == 1 {
                            self.builder
                                .build_int_z_extend(iv, t, &format!("arg_zext_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("zext error: {}", e))
                                })?
                        } else {
                            self.builder
                                .build_int_s_extend(iv, t, &format!("arg_sext_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("sext error: {}", e))
                                })?
                        }
                    } else if iv.get_type().get_bit_width() > t.get_bit_width() {
                        self.builder
                            .build_int_truncate(iv, t, &format!("arg_trunc_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?
                    } else {
                        iv
                    };
                    self.build_store(cast_ptr, stored)?;
                }
                (BasicValueEnum::FloatValue(fv), BasicTypeEnum::FloatType(_)) => {
                    self.build_store(cast_ptr, fv)?;
                }
                (BasicValueEnum::PointerValue(pv), BasicTypeEnum::PointerType(_)) => {
                    self.build_store(cast_ptr, pv)?;
                }
                (BasicValueEnum::StructValue(sv), BasicTypeEnum::StructType(_)) => {
                    // 0.34.36 (audit §6.6): guard the value's real size against
                    // the declared slot before storing — a value larger than its
                    // slot would write into the next param's bytes.
                    let val_bytes =
                        self.llvm_type_size_bytes(BasicTypeEnum::StructType(sv.get_type()));
                    if val_bytes > slot_size {
                        self.emit_actor_mailbox_overflow_abort(
                            &format!(
                                "actor mailbox overflow: argument {} of {}.{} is {} bytes but its slot is {} bytes",
                                i, obj_type, method_name, val_bytes, slot_size
                            ),
                            &format!("argstore_{}_{}_{}", obj_type, method_name, i),
                        )?;
                    }
                    self.build_store(cast_ptr, sv)?;
                }
                (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st))
                    if Self::is_actor_string_abi_type(BasicTypeEnum::StructType(st)) =>
                {
                    // Raw C string pointer → wrap to {ptr,len} for string params.
                    let wrapped = self.wrap_c_string(pv)?;
                    self.build_store(cast_ptr, wrapped)?;
                }
                (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                    // 0.34.36 (audit §6.6): a record-typed argument arrives as a
                    // pointer to its alloca (record literals are pointer-valued).
                    // Load the aggregate so its BYTES land in the slot — storing
                    // the pointer left the slot holding an address while the
                    // dispatch side read a struct value (pack/unpack divergence).
                    let loaded = self.build_load(
                        BasicTypeEnum::StructType(st),
                        pv,
                        &format!("arg_rec_load_{}", i),
                    )?;
                    self.build_store(cast_ptr, loaded)?;
                }
                (BasicValueEnum::IntValue(iv), _) => {
                    let stored = if iv.get_type().get_bit_width() < 64 {
                        if iv.get_type().get_bit_width() == 1 {
                            self.builder
                                .build_int_z_extend(iv, i64_ty, &format!("arg_zext_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("zext error: {}", e))
                                })?
                        } else {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, &format!("arg_sext_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("sext error: {}", e))
                                })?
                        }
                    } else {
                        iv
                    };
                    self.build_store(cast_ptr, stored)?;
                }
                (BasicValueEnum::FloatValue(fv), _) => {
                    self.build_store(cast_ptr, fv)?;
                }
                (BasicValueEnum::PointerValue(pv), _) => {
                    self.build_store(cast_ptr, pv)?;
                }
                (BasicValueEnum::StructValue(sv), _) => {
                    self.build_store(cast_ptr, sv)?;
                }
                _ => {
                    self.build_store(cast_ptr, i64_ty.const_int(0, false))?;
                }
            }
            blob_offset += slot_size;
        }

        let args_size = i64_ty.const_int(blob_offset, false);

        // Allocate result blob — same capacity as the worker-side blob
        // (MIMI_ACTOR_BLOB_SIZE / ACTOR_MAILBOX_BLOB_CAPACITY); the dispatch
        // traps oversized results before writing (audit §6.6).
        let result_blob = self.build_alloca(
            self.context
                .i8_type()
                .array_type(ACTOR_MAILBOX_BLOB_CAPACITY as u32),
            "actor_result_blob",
        )?;

        // Call mimi_actor_call(handle, method_id, args_ptr, args_size, result_ptr)
        let call_fn = self.get_runtime_fn("mimi_actor_call")?;
        let args_blob_i8ptr = self
            .builder
            .build_bit_cast(args_blob, i8_ptr, "args_blob_i8")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        let result_blob_i8ptr = self
            .builder
            .build_bit_cast(result_blob, i8_ptr, "result_blob_i8")
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();

        let _call_result = self.build_call(
            call_fn,
            &[
                handle_ptr.into(),
                i32_ty.const_int(method_id as u64, false).into(),
                args_blob_i8ptr.into(),
                args_size.into(),
                result_blob_i8ptr.into(),
            ],
            "actor_call_result",
        )?;

        // Load result from result_blob using declared return type layout (CG-H6).
        let result_cast = self
            .builder
            .build_bit_cast(
                result_blob,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "result_cast",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();

        let result_val: BasicValueEnum<'ctx> = match method_ret_ty.as_ref().map(Type::unlocated) {
            Some(crate::ast::Type::Name(n, _)) if n == "f64" => {
                self.build_load(self.context.f64_type(), result_cast, "result_f64")?
            }
            Some(crate::ast::Type::Name(n, _)) if n == "i32" => {
                self.build_load(self.context.i32_type(), result_cast, "result_i32")?
            }
            Some(crate::ast::Type::Name(n, _)) if n == "bool" => {
                self.build_load(self.context.bool_type(), result_cast, "result_bool")?
            }
            Some(ty) => {
                // actor_abi_type_for: registry-backed lowering, identical to the
                // dispatch-side result pack and the method signature (audit §6.6).
                let llvm_ty = self.actor_abi_type_for(ty);
                match llvm_ty {
                    BasicTypeEnum::StructType(st) => self.build_load(
                        BasicTypeEnum::StructType(st),
                        result_cast,
                        "mailbox_struct_ret",
                    )?,
                    BasicTypeEnum::IntType(t) => {
                        self.build_load(t, result_cast, "mailbox_int_ret")?
                    }
                    BasicTypeEnum::FloatType(t) => {
                        self.build_load(t, result_cast, "mailbox_float_ret")?
                    }
                    BasicTypeEnum::PointerType(t) => {
                        self.build_load(t, result_cast, "mailbox_ptr_ret")?
                    }
                    _ => self.build_load(i64_ty, result_cast, "method_result")?,
                }
            }
            None => self.build_load(i64_ty, result_cast, "method_result")?,
        };

        self.build_br(merge_bb)?;

        // ── Merge ──
        self.builder.position_at_end(merge_bb);

        // For self-call path, we need a phi. But we already handled self-call
        // above and returned. So here both paths merge with the mailbox result.
        // Actually, the self-call path for non-self objects branches to mailbox.
        // And for self objects, we already returned. So at this point, the
        // self_call_bb has branched to mailbox_bb. The merge block has two
        // predecessors: mailbox_bb (from the normal path) and... wait, no.
        //
        // Let me reconsider. For the non-self case:
        //   self_call_bb → br mailbox_bb → br merge_bb
        // So merge_bb has one predecessor: mailbox_bb. No phi needed.
        //
        // For the self case, we already returned above, so we don't reach here.
        //
        // The actor method cloned adopted structural returns before its worker
        // scope was flushed. Register those fresh leaves in the caller just as
        // named and closure calls do, so mailbox transport does not become an
        // ownership blind spot.
        let result_val = self.track_derived_structural_return_lifetime_for_type(
            method_ret_ty.as_ref(),
            result_val,
        )?;
        Ok(Some(result_val))
    }
}
