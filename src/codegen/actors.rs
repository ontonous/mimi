use crate::ast::*;
use crate::codegen::call_try_basic_value;
use crate::codegen::types;
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

    /// Emit a runtime guard: abort with `msg` if `actual` < `required`
    /// (unsigned). Protects dispatch-side unpack reads: `args_size` crosses a
    /// thread boundary via the mailbox, so its validity is checked at runtime,
    /// not assumed. 0.34.36 (audit §6.6).
    fn emit_actor_mailbox_size_check(
        &mut self,
        actual: inkwell::values::IntValue<'ctx>,
        required: u64,
        msg: &str,
        tag: &str,
    ) -> MimiResult<()> {
        let i64_ty = self.context.i64_type();
        let too_small = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                actual,
                i64_ty.const_int(required, false),
                &format!("{}_too_small", tag),
            )
            .map_err(|e| CompileError::LlvmError(format!("icmp error: {}", e)))?;
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("no current function for actor blob guard".into())
        })?;
        let abort_bb = self
            .context
            .append_basic_block(function, &format!("{}_args_underflow", tag));
        let cont_bb = self
            .context
            .append_basic_block(function, &format!("{}_args_ok", tag));
        self.build_cond_br(too_small, abort_bb, cont_bb)?;
        self.builder.position_at_end(abort_bb);
        let abort_fn = self.get_or_declare_abort_fn();
        let msg_ptr = self
            .builder
            .build_global_string_ptr(msg, &format!("{}_args_msg", tag))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        self.build_call(
            abort_fn,
            &[BasicMetadataValueEnum::PointerValue(
                msg_ptr.as_pointer_value(),
            )],
            &format!("{}_args_abort", tag),
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
                fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64)
            }
            _ => false,
        }
    }

    pub(super) fn compile_actor(&mut self, actor: &crate::ast::ActorDef) -> MimiResult<()> {
        // Register this actor type name for method-call dispatch routing.
        self.actor_names.insert(actor.name.clone());

        // Cache the actor definition so mailbox call-sites can recover the
        // declared method return type.
        self.actor_defs.insert(actor.name.clone(), actor.clone());

        // Assign method IDs (used as method_id in dispatch + mimi_actor_call).
        for (i, method) in actor.methods.iter().enumerate() {
            let key = format!("{}::{}", actor.name, method.name);
            self.actor_method_ids.insert(key, i as i32);
        }

        // 1. Generate constructor: ActorName(fields...) -> Actor struct
        //    (kept for backwards compat; spawn is the primary entry point)
        self.compile_actor_constructor(actor)?;

        // 2. Compile all actor methods (unchanged: {Name}__{method}__method)
        for method in &actor.methods {
            self.compile_actor_method(actor, method)?;
        }

        // 3. Generate the dispatch function: {Name}__dispatch
        self.compile_actor_dispatch(actor)?;

        // 4. Generate lifecycle-aware spawn adapters.
        self.compile_actor_spawn(actor, false)?;
        self.compile_actor_spawn(actor, true)?;

        Ok(())
    }

    /// Generate the constructor function: ActorName(field1, field2, ...) -> Actor struct.
    /// This is the legacy constructor; real actors use _spawn().
    fn compile_actor_constructor(&mut self, actor: &crate::ast::ActorDef) -> MimiResult<()> {
        let mut param_types = Vec::new();
        for f in &actor.fields {
            // Registry-backed lowering (audit §6.6): the struct field slots
            // come from register_actor_def's llvm_type_for, so constructor
            // params must agree with them.
            let ty = self.actor_abi_type_for(&f.ty);
            param_types.push(ty);
        }

        let metadata_params: Vec<_> = param_types
            .iter()
            .map(|t| types::basic_to_metadata(self.context, *t))
            .collect();

        let actor_ty = *self.type_llvm.get(&actor.name).ok_or_else(|| {
            CompileError::TypeNotFound(format!("actor type '{}' not found", actor.name))
        })?;

        let fn_type = match actor_ty {
            BasicTypeEnum::StructType(sty) => sty.fn_type(&metadata_params, false),
            _ => return Err(CompileError::ActorNotStruct(actor.name.to_string())),
        };

        let constructor_name = format!("{}_new", actor.name);
        let function = self.module.add_function(&constructor_name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let alloca = match actor_ty {
            BasicTypeEnum::StructType(sty) => self.build_alloca(sty, &actor.name)?,
            _ => return Err(CompileError::LlvmError("actor type error".to_string())),
        };

        for (i, param) in function.get_params().iter().enumerate() {
            if let Some(BasicTypeEnum::StructType(sty)) = self.type_llvm.get(&actor.name) {
                let gep = self
                    .gep()
                    .build_struct_gep(*sty, alloca, i as u32, &actor.fields[i].name)
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.build_store(gep, *param)?;
            }
        }

        let ret_val = self.build_load(actor_ty, alloca, &actor.name)?;
        self.build_return(Some(&ret_val))?;

        Ok(())
    }

    /// Generate the dispatch function: {Name}__dispatch
    ///
    /// void dispatch(i32 method_id, i8* self_fields_ptr, i8* args_blob,
    ///               i64 args_size, i8* result_blob, i64* result_size_out)
    ///
    /// This function is called on the actor's worker thread. It switches on
    /// method_id, unpacks args from the blob, calls the method function, and
    /// packs the return value into result_blob.
    fn compile_actor_dispatch(&mut self, actor: &crate::ast::ActorDef) -> MimiResult<()> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();

        let dispatch_name = format!("{}__dispatch", actor.name);
        let i64_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let fn_type = void_ty.fn_type(
            &[
                inkwell::types::BasicMetadataTypeEnum::IntType(i32_ty),
                inkwell::types::BasicMetadataTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicMetadataTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicMetadataTypeEnum::IntType(i64_ty),
                inkwell::types::BasicMetadataTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicMetadataTypeEnum::PointerType(i64_ptr),
            ],
            false,
        );

        let function = self.module.add_function(&dispatch_name, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        let method_id = function
            .get_nth_param(0)
            .ok_or_else(|| {
                CompileError::LlvmError("dispatch: missing method_id param".to_string())
            })?
            .into_int_value();
        let self_fields_ptr = function
            .get_nth_param(1)
            .ok_or_else(|| {
                CompileError::LlvmError("dispatch: missing self_fields_ptr param".to_string())
            })?
            .into_pointer_value();
        let args_blob = function
            .get_nth_param(2)
            .ok_or_else(|| {
                CompileError::LlvmError("dispatch: missing args_blob param".to_string())
            })?
            .into_pointer_value();
        // args_size = param 3. 0.34.36 (audit §6.6): previously ignored —
        // unpack offsets were trusted blindly. It now bounds the unpack reads
        // per case (cross-thread data must be validated, not assumed).
        let args_size = function
            .get_nth_param(3)
            .ok_or_else(|| {
                CompileError::LlvmError("dispatch: missing args_size param".to_string())
            })?
            .into_int_value();
        let result_blob = function
            .get_nth_param(4)
            .ok_or_else(|| {
                CompileError::LlvmError("dispatch: missing result_blob param".to_string())
            })?
            .into_pointer_value();
        let result_size_out = function
            .get_nth_param(5)
            .ok_or_else(|| {
                CompileError::LlvmError("dispatch: missing result_size_out param".to_string())
            })?
            .into_pointer_value();

        // Generate a switch on method_id.
        let default_bb = self
            .context
            .append_basic_block(function, "dispatch_default");
        let merge_bb = self.context.append_basic_block(function, "dispatch_end");

        let mut switch_cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut case_bbs: Vec<(usize, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();

        for (i, _method) in actor.methods.iter().enumerate() {
            let case_bb = self
                .context
                .append_basic_block(function, &format!("dispatch_case_{}", i));
            switch_cases.push((i32_ty.const_int(i as u64, false), case_bb));
            case_bbs.push((i, case_bb));
        }

        self.builder
            .build_switch(method_id, default_bb, &switch_cases)
            .map_err(|e| CompileError::LlvmError(format!("switch error: {}", e)))?;

        // Build each case.
        for (i, case_bb) in case_bbs {
            self.builder.position_at_end(case_bb);

            let method = &actor.methods[i];
            let mangled = format!("{}__{}__method", actor.name, method.name);
            let method_fn = self.module.get_function(&mangled).ok_or_else(|| {
                CompileError::LlvmError(format!("method fn {} not found", mangled))
            })?;

            // Build args: self_fields_ptr (as the self pointer) + unpacked params.
            let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> = Vec::new();
            call_args.push(self_fields_ptr.into());

            // 0.34.36 (audit §6.6): bound the unpack before reading. Compute
            // the exact packed size this method's params occupy (same slot
            // layout the call site packs with — single source
            // `actor_abi_type_for` + `actor_abi_slot_size`). Statically
            // oversized packs trap; a runtime check validates the args_size
            // that crossed the mailbox before any read.
            let required_args_size: u64 = method
                .params
                .iter()
                .map(|p| self.actor_abi_slot_size(self.actor_abi_type_for(&p.ty)))
                .sum();
            if required_args_size > ACTOR_MAILBOX_BLOB_CAPACITY {
                self.emit_actor_mailbox_overflow_abort(
                    &format!(
                        "actor mailbox overflow: {}.{} packs {} bytes of arguments; capacity is {} bytes",
                        actor.name, method.name, required_args_size, ACTOR_MAILBOX_BLOB_CAPACITY
                    ),
                    &format!("dispatch_{}_{}", actor.name, method.name),
                )?;
            } else {
                self.emit_actor_mailbox_size_check(
                    args_size,
                    required_args_size,
                    &format!(
                        "actor mailbox args underflow: {}.{} expects {} packed bytes, got fewer",
                        actor.name, method.name, required_args_size
                    ),
                    &format!("dispatch_{}_{}", actor.name, method.name),
                )?;
            }

            // Unpack params from args_blob with natural type sizes (R-C6).
            // Call site packs each param as its LLVM layout, 8-byte aligned.
            // Previously every slot was 8 bytes while struct loads used natural
            // sizes (e.g. string {ptr,len} = 16), reading past the slot.
            let mut offset: u64 = 0;
            for param in &method.params {
                // actor_abi_type_for (registry-backed) — must agree with the
                // call-site pack type, audit §6.6.
                let param_ty = self.actor_abi_type_for(&param.ty);
                let slot_size = self.actor_abi_slot_size(param_ty);
                let gep = self
                    .gep()
                    .build_in_bounds_gep(
                        self.context.i8_type(),
                        args_blob,
                        &[i64_ty.const_int(offset, false)],
                        "arg_gep",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;

                let loaded = match param_ty {
                    BasicTypeEnum::IntType(t) => {
                        let cast_ptr = self
                            .builder
                            .build_bit_cast(
                                gep,
                                self.context.ptr_type(inkwell::AddressSpace::default()),
                                "arg_cast",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                            .into_pointer_value();
                        self.build_load(t, cast_ptr, "arg_val")?
                    }
                    BasicTypeEnum::FloatType(t) => {
                        let cast_ptr = self
                            .builder
                            .build_bit_cast(
                                gep,
                                self.context.ptr_type(inkwell::AddressSpace::default()),
                                "arg_fcast",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                            .into_pointer_value();
                        self.build_load(t, cast_ptr, "arg_fval")?
                    }
                    BasicTypeEnum::PointerType(t) => {
                        let cast_ptr = self
                            .builder
                            .build_bit_cast(gep, t, "arg_pcast")
                            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                            .into_pointer_value();
                        self.build_load(t, cast_ptr, "arg_pval")?
                    }
                    BasicTypeEnum::StructType(t) => {
                        let cast_ptr = self
                            .builder
                            .build_bit_cast(
                                gep,
                                self.context.ptr_type(inkwell::AddressSpace::default()),
                                "arg_scast",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                            .into_pointer_value();
                        self.build_load(t, cast_ptr, "arg_sval")?
                    }
                    _ => {
                        let cast_ptr = self
                            .builder
                            .build_bit_cast(
                                gep,
                                self.context.ptr_type(inkwell::AddressSpace::default()),
                                "arg_icast",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                            .into_pointer_value();
                        self.build_load(i64_ty, cast_ptr, "arg_ival")?
                    }
                };
                call_args.push(loaded.into());
                offset += slot_size;
            }

            let call = self.build_call(method_fn, &call_args, "dispatch_method_call")?;
            let ret_val = call_try_basic_value(&call).unwrap_or(i64_ty.const_int(0, false).into());

            // Pack return value into result_blob (first 8 bytes).
            // For void methods, write 0.
            let result_cast = self
                .builder
                .build_bit_cast(
                    result_blob,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "result_cast",
                )
                .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                .into_pointer_value();

            // Store the return value (extended to i64 if needed).
            let ret_as_i64 = match ret_val {
                BasicValueEnum::IntValue(iv) => {
                    let bw = iv.get_type().get_bit_width();
                    if bw < 64 {
                        // A1: use s_extend for signed integers (width > 1),
                        // z_extend for bool (i1 — sign bit would make true = -1).
                        if bw == 1 {
                            self.builder
                                .build_int_z_extend(iv, i64_ty, "ret_zext")
                                .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                                .into()
                        } else {
                            self.builder
                                .build_int_s_extend(iv, i64_ty, "ret_sext")
                                .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?
                                .into()
                        }
                    } else {
                        iv.into()
                    }
                }
                BasicValueEnum::FloatValue(fv) => {
                    // Store float as bits in the i64 slot. 0.34.36 (audit
                    // §6.14): bitcast requires EQUAL bit widths — the old
                    // `bitcast f32 -> i64` is invalid IR. Widen via the
                    // same-width integer first (bitcast f32->i32), then zext
                    // into the i64 slot. f64 bitcasts straight across.
                    let fbits = fv.get_type().get_bit_width();
                    if fbits == 64 {
                        self.builder
                            .build_bit_cast(fv, i64_ty, "ret_f2i")
                            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                    } else if fbits < 64 {
                        // fbits in {16, 32} for real floats — nonzero.
                        let nz = std::num::NonZeroU32::new(fbits as u32).ok_or_else(|| {
                            CompileError::LlvmError(format!("float slot width: {} is zero", fbits))
                        })?;
                        let int_ty = self.context.custom_width_int_type(nz).map_err(|e| {
                            CompileError::LlvmError(format!("float slot width: {}", e))
                        })?;
                        let as_int = self
                            .builder
                            .build_bit_cast(fv, int_ty, "ret_f2i_narrow")
                            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                            .into_int_value();
                        self.builder
                            .build_int_z_extend(as_int, i64_ty, "ret_f_zext")
                            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                            .into()
                    } else {
                        // Wider than 64 (e.g. f128) cannot fit the scalar
                        // slot: trap instead of silently truncating.
                        self.emit_actor_mailbox_overflow_abort(
                            &format!(
                                "actor mailbox: {}-bit float return from {}.{} does not fit the 64-bit result slot",
                                fbits, actor.name, method.name
                            ),
                            &format!("retf_{}_{}", actor.name, method.name),
                        )?;
                        i64_ty.const_int(0, false).into()
                    }
                }
                BasicValueEnum::PointerValue(pv) => self
                    .builder
                    .build_ptr_to_int(pv, i64_ty, "ret_p2i")
                    .map_err(|e| CompileError::LlvmError(format!("ptr2int error: {}", e)))?
                    .into(),
                BasicValueEnum::StructValue(sv) => {
                    // Store struct by copying into result_blob
                    let sty = sv.get_type();
                    // result_size = sizeof(struct).
                    // v0.28.30: StructType::size_of() may return None for
                    // structs containing opaque pointers. 0.34.36 (audit
                    // §6.14): the old field-sum hard-coded floats to 8 bytes
                    // and arrays to len*8 — wrong for f32 fields (4 bytes)
                    // and non-i64 array elements. Use the recursive portable
                    // layout computer instead.
                    let total_size: u64 = self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
                    // 0.34.36 (audit §6.6): the worker's result blob is
                    // MIMI_ACTOR_BLOB_SIZE bytes (runtime/actor.rs) — trap on
                    // an oversized store instead of corrupting the heap vec.
                    if total_size > ACTOR_MAILBOX_BLOB_CAPACITY {
                        self.emit_actor_mailbox_overflow_abort(
                            &format!(
                                "actor mailbox overflow: {}.{} returns a {}-byte record; result capacity is {} bytes",
                                actor.name, method.name, total_size, ACTOR_MAILBOX_BLOB_CAPACITY
                            ),
                            &format!("retstruct_{}_{}", actor.name, method.name),
                        )?;
                    }
                    let result_scast = self
                        .builder
                        .build_bit_cast(
                            result_blob,
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                            "result_scast",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                        .into_pointer_value();
                    self.build_store(result_scast, sv)?;
                    let struct_size = self.context.i64_type().const_int(total_size, false);
                    self.build_store(result_size_out, struct_size)?;
                    self.build_br(merge_bb)?;
                    continue;
                }
                _ => i64_ty.const_int(0, false).into(),
            };

            self.build_store(result_cast, ret_as_i64)?;
            // Write result size = 8 (one i64 slot).
            self.build_store(result_size_out, i64_ty.const_int(8, false))?;

            self.build_br(merge_bb)?;
        }

        // Default case: unknown method_id.
        self.builder.position_at_end(default_bb);
        self.build_store(result_size_out, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;

        // Merge block.
        self.builder.position_at_end(merge_bb);
        self.build_return(None)?;

        Ok(())
    }

    fn compile_actor_spawn(
        &mut self,
        actor: &crate::ast::ActorDef,
        detached: bool,
    ) -> MimiResult<()> {
        let actor_ty = *self.type_llvm.get(&actor.name).ok_or_else(|| {
            CompileError::TypeNotFound(format!("actor type '{}' not found", actor.name))
        })?;

        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // _spawn() -> i8* (actor handle)
        let spawn_fn_type = i8_ptr.fn_type(&[], false);
        let spawn_name = if detached {
            format!("{}_spawn_detached", actor.name)
        } else {
            format!("{}_spawn", actor.name)
        };
        let function = self.module.add_function(&spawn_name, spawn_fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        self.begin_function_heap_scope();

        // Allocate actor struct on heap (not stack) for cross-thread safety.
        // C5: alloca is invalid after the spawning function returns, but the
        // worker thread reads these fields after spawn returns. Use malloc
        // (via malloc_or_abort) to keep memory alive for the worker.
        let struct_size_val_full = match actor_ty {
            BasicTypeEnum::StructType(sty) => {
                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(sty));
                i64_ty.const_int(size.max(8), false)
            }
            _ => i64_ty.const_int(0, false),
        };
        let raw_ptr =
            self.malloc_or_abort(struct_size_val_full, &format!("{}_fields", actor.name))?;
        // Cast the heap pointer to the struct type for GEP field access.
        let alloca = match actor_ty {
            // LLVM 18 uses opaque pointers, so malloc's pointer is already the
            // representation required by the typed struct GEP below.
            BasicTypeEnum::StructType(_) => raw_ptr,
            _ => return Err(CompileError::LlvmError("actor type error".to_string())),
        };

        let empty_vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        if let BasicTypeEnum::StructType(sty) = actor_ty {
            for (i, field) in actor.fields.iter().enumerate() {
                let gep = self
                    .gep()
                    .build_struct_gep(sty, alloca, i as u32, &field.name)
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let mut val = if let Some(init) = &field.init {
                    self.compile_expr(init, &empty_vars)?
                } else {
                    // actor_abi_type_for: the actor struct's field slots are
                    // registered through the registry (register_actor_def), so
                    // the zero initializer must use the same lowering — an
                    // i64 zero stored into a struct slot was invalid IR
                    // (audit §6.6).
                    let ty = self.actor_abi_type_for(&field.ty);
                    match ty {
                        BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
                        BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
                        BasicTypeEnum::PointerType(t) => t.const_null().into(),
                        BasicTypeEnum::StructType(t) => t.const_zero().into(),
                        BasicTypeEnum::ArrayType(t) => t.const_zero().into(),
                        _ => self.context.i64_type().const_int(0, false).into(),
                    }
                };
                // String literals return a raw C string pointer; normalize to struct.
                if let Some(init) = &field.init {
                    val = self.normalize_string_value(val, init)?;
                }
                // List and record literals return a pointer to a stack-allocated
                // struct. Load the struct value before storing into the actor field
                // (same pattern as compile_let in block.rs:209-224).
                if let BasicValueEnum::PointerValue(pv) = val {
                    let field_tys = sty.get_field_types();
                    let want_struct = i < field_tys.len()
                        && matches!(&field_tys[i], BasicTypeEnum::StructType(_));
                    if want_struct {
                        let field_llvm_ty = field_tys[i];
                        let loaded = self
                            .builder
                            .build_load(field_llvm_ty, pv, &format!("{}_load", field.name))
                            .map_err(|e| {
                                CompileError::LlvmError(format!("actor field load: {}", e))
                            })?;
                        val = loaded;
                    }
                }
                self.build_store(gep, val)?;
            }
        }

        // Use the size computed above for the spawn call.
        let struct_size_val = struct_size_val_full;

        // Get the dispatch function pointer.
        let dispatch_name = format!("{}__dispatch", actor.name);
        let dispatch_fn = self.module.get_function(&dispatch_name).ok_or_else(|| {
            CompileError::LlvmError(format!("dispatch fn {} not found", dispatch_name))
        })?;

        // Call mimi_actor_spawn(fields_ptr, fields_size, dispatch_fn) -> i8*
        // C5: raw_ptr is already heap-allocated (via malloc_or_abort), so it is
        // safe for cross-thread access. No stack-alloca + bitcast needed.
        let spawn_rt = self.get_runtime_fn(if detached {
            "mimi_actor_spawn_detached"
        } else {
            "mimi_actor_spawn"
        })?;

        let handle = self.build_call(
            spawn_rt,
            &[
                raw_ptr.into(),
                struct_size_val.into(),
                dispatch_fn.as_global_value().as_pointer_value().into(),
            ],
            "actor_handle",
        )?;

        let handle_val = call_try_basic_value(&handle).unwrap_or(i8_ptr.const_null().into());

        // Prefer CheckedProgram mailbox depth; fall back to Surface FlowDef annotations.
        let mailbox_depth = self
            .resolved_mailbox_depths
            .as_ref()
            .and_then(|map| {
                map.get(&actor.name).copied().or_else(|| {
                    map.iter().find_map(|(qualified, depth)| {
                        qualified
                            .rsplit("::")
                            .next()
                            .filter(|bare| *bare == actor.name)
                            .map(|_| *depth)
                    })
                })
            })
            .or_else(|| {
                self.flow_defs.get(&actor.name).and_then(|flow| {
                    flow.annotations.iter().find_map(|ann| match &ann.kind {
                        crate::ast::FlowAnnotationKind::MailboxDepth(d) => Some(*d),
                        _ => None,
                    })
                })
            });
        if let Some(d) = mailbox_depth {
            if let Ok(set_fn) = self.get_runtime_fn("mimi_actor_set_mailbox_depth") {
                let depth_val = self.context.i64_type().const_int(d as u64, false);
                let hv = match handle_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => i8_ptr.const_null(),
                };
                self.build_call(set_fn, &[hv.into(), depth_val.into()], "set_mailbox_depth")?;
            }
        }

        // v0.29.25: register method names so broadcast can resolve by name.
        if !actor.methods.is_empty() {
            if let Ok(set_names) = self.get_runtime_fn("mimi_actor_set_method_names") {
                let n = actor.methods.len();
                let arr_ty = i8_ptr.array_type(n as u32);
                let names_arr = self
                    .builder
                    .build_alloca(arr_ty, "method_names_arr")
                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                for (i, m) in actor.methods.iter().enumerate() {
                    let name_gv = self
                        .builder
                        .build_global_string_ptr(&m.name, &format!(".mn_{}_{}", actor.name, m.name))
                        .map_err(|e| CompileError::LlvmError(format!("gstr: {}", e)))?;
                    // SAFETY: `arr_ty` is an array type allocated by build_alloca
                    // above (valid pointer, valid type). The indices [0, i] are
                    // in-bounds because `i` ranges from 0 to actor.methods.len()-1
                    // and the array has n elements where n = actor.methods.len().
                    let gep = unsafe {
                        self.builder
                            .build_in_bounds_gep(
                                arr_ty,
                                names_arr,
                                &[
                                    self.context.i64_type().const_int(0, false),
                                    self.context.i64_type().const_int(i as u64, false),
                                ],
                                "name_slot",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?
                    };
                    self.builder
                        .build_store(gep, name_gv.as_pointer_value())
                        .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                }
                let names_ptr = self
                    .builder
                    .build_bit_cast(names_arr, i8_ptr, "names_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?
                    .into_pointer_value();
                let count = self.context.i64_type().const_int(n as u64, false);
                let hptr = match handle_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => i8_ptr.const_null(),
                };
                self.build_call(
                    set_names,
                    &[hptr.into(), names_ptr.into(), count.into()],
                    "set_method_names",
                )?;
            }
        }

        self.build_return(Some(&handle_val))?;
        self.end_function_heap_scope();

        Ok(())
    }

    pub(super) fn compile_actor_method(
        &mut self,
        actor: &crate::ast::ActorDef,
        method: &FuncDef,
    ) -> MimiResult<()> {
        let (ret_type, mut vars) = self.build_actor_method_function(actor, method)?;
        let last_val = self.compile_actor_method_body(method, &mut vars)?;
        let last_expr = method.body.last().and_then(|s| {
            if let Stmt::Expr(e) = s.unlocated() {
                Some(e)
            } else {
                None
            }
        });
        let result = self.emit_actor_method_epilogue(&mut vars, ret_type, last_val, last_expr);
        self.end_function_heap_scope();
        result
    }

    /// Build the LLVM function for an actor method, push scopes, and bind `self`
    /// and the method parameters.
    fn build_actor_method_function(
        &mut self,
        actor: &crate::ast::ActorDef,
        method: &FuncDef,
    ) -> Result<(BasicTypeEnum<'ctx>, HashMap<String, VarEntry<'ctx>>), CompileError> {
        let actor_ty = *self.type_llvm.get(&actor.name).ok_or_else(|| {
            CompileError::TypeNotFound(format!("actor type '{}' not found", actor.name))
        })?;

        let mangled = format!("{}__{}__method", actor.name, method.name);

        let actor_ptr_ty = match actor_ty {
            BasicTypeEnum::StructType(_) => {
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default()))
            }
            _ => return Err(CompileError::ActorNotStruct(actor.name.to_string())),
        };

        let mut param_metadata = vec![types::basic_to_metadata(self.context, actor_ptr_ty)];
        let mut param_llvm = vec![actor_ptr_ty];
        for p in &method.params {
            // actor_abi_type_for: registry-backed lowering shared with the
            // mailbox pack/unpack (audit §6.6) — the dispatch unpacks with
            // exactly these types, so the signature must agree.
            let ty = self.actor_abi_type_for(&p.ty);
            param_llvm.push(ty);
            param_metadata.push(types::basic_to_metadata(self.context, ty));
        }

        let ret_llvm = match &method.ret {
            Some(ty) => self.actor_abi_type_for(ty),
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let fn_type = match ret_llvm {
            BasicTypeEnum::IntType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&param_metadata, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&param_metadata, false),
            _ => self.context.i64_type().fn_type(&param_metadata, false),
        };

        let function = self.module.add_function(&mangled, fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.push_cap_scope();
        self.push_comp_scope();
        // 0.34.36 (L1 parity, audit §6.7): actor methods previously dropped
        // `defer` entirely (no scope, no Defer arm). The bytecode VM runs
        // defers, so push the method-level defer scope here and pop it on
        // every exit path (Return arms + epilogue).
        self.push_defer_scope();
        self.begin_function_heap_scope();

        let mut vars: HashMap<String, VarEntry> = HashMap::new();

        // Bind self: allocate space for the actor pointer and store the parameter.
        let self_alloca = self.build_alloca(actor_ptr_ty, "self")?;
        self.build_store(
            self_alloca,
            function.get_nth_param(0).ok_or_else(|| {
                CompileError::LlvmError("codegen: missing self param in actor method".to_string())
            })?,
        )?;
        vars.insert("self".to_string(), (self_alloca, actor_ptr_ty));
        self.var_type_names
            .insert("self".to_string(), actor.name.clone());

        // Bind method params — must use the SAME lowering as the signature
        // above (actor_abi_type_for), or the alloca/store types mismatch.
        let param_offset = 1;
        for (i, param) in method.params.iter().enumerate() {
            let ty = self.actor_abi_type_for(&param.ty);
            let alloca = self.build_alloca(ty, &param.name)?;
            self.build_store(
                alloca,
                function
                    .get_nth_param((i + param_offset) as u32)
                    .ok_or_else(|| {
                        CompileError::LlvmError(format!(
                            "codegen: missing param {} in actor method",
                            i + param_offset
                        ))
                    })?,
            )?;
            vars.insert(param.name.clone(), (alloca, ty));
            // Register the parameter's TYPE NAME so infer_object_type can
            // resolve `p.x` field access on record-typed actor params.
            // func/export.rs does this for regular function params
            // (export.rs:206-209); actor methods were missing it, so
            // `p.x` resolved the base to the variable name "p" instead of
            // "Point" → E0707 "cannot access field on type 'p'"
            // (actor_mailbox_record_param regression, audit 2026-08-05 wave-2).
            match param.ty.unlocated() {
                crate::ast::Type::Name(n, args) if args.is_empty() => {
                    self.var_type_names.insert(param.name.clone(), n.clone());
                }
                crate::ast::Type::Name(n, _) => {
                    self.var_type_names.insert(param.name.clone(), n.clone());
                }
                other => {
                    if let Some(tn) = self.get_full_type_name(other) {
                        self.var_type_names.insert(param.name.clone(), tn);
                    }
                }
            }
        }

        Ok((ret_llvm, vars))
    }

    /// Compile the body statements of an actor method, returning the value that
    /// should be returned if the method falls through.
    fn compile_actor_method_body(
        &mut self,
        method: &FuncDef,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // 0.35.23 deep-eval: refresh the borrow-param guard set for this
        // method body (claim_returned_lists skips caller-owned list storage;
        // stale entries from a previous legacy body could mask a legit claim).
        self.borrow_param_names = method
            .params
            .iter()
            .filter(|p| p.borrow.is_some())
            .map(|p| p.name.clone())
            .collect();
        let ret_type = self
            .current_fn_ret_type()
            .unwrap_or_else(|| BasicTypeEnum::IntType(self.context.i64_type()));
        // 0.34.36 (audit §6.6): a struct/array return needs a zero of ITS OWN
        // type — the old i64 default built an ill-typed `ret` for record
        // returns now that the signature uses registry-resolved layouts.
        let default_val = match ret_type {
            BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
            BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
            BasicTypeEnum::StructType(t) => t.const_zero().into(),
            BasicTypeEnum::ArrayType(t) => t.const_zero().into(),
            _ => self.context.i64_type().const_int(0, false).into(),
        };
        let mut last_val: BasicValueEnum = default_val;
        for stmt in &method.body {
            if self.compile_actor_method_stmt(stmt, vars, &mut last_val, ret_type)? {
                return Ok(last_val);
            }
        }
        Ok(last_val)
    }

    /// Compile a single actor-method statement.
    /// Returns `true` if the statement is a `return` that terminates the method.
    fn compile_actor_method_stmt(
        &mut self,
        stmt: &Stmt,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
        last_val: &mut BasicValueEnum<'ctx>,
        ret_type: BasicTypeEnum<'ctx>,
    ) -> Result<bool, CompileError> {
        // Run compensations before exit()
        if let Stmt::Expr(expr) = stmt.unlocated() {
            if let Expr::Call(callee, _) = expr.unlocated() {
                if let Expr::Ident(name) = callee.unlocated() {
                    if name == "exit" {
                        self.compile_compensations(vars)?;
                    }
                }
            }
        }

        match stmt.unlocated() {
            Stmt::Expr(expr) => {
                *last_val = self.compile_expr(expr, vars)?;
                *last_val = self.adjust_int_val(*last_val, ret_type)?;
            }
            Stmt::Return(Some(expr)) => {
                let mut val = self.compile_expr(expr, vars)?;
                val = self.adjust_int_val(
                    val,
                    self.current_fn_ret_type()
                        .unwrap_or_else(|| BasicTypeEnum::IntType(self.context.i64_type())),
                )?;
                val = self.claim_string_return_value(val, ret_type, Some(expr), vars)?;
                // L6: claim a returned custom-enum payload box (caller
                // re-registers via EnumBox). Mirrors func.rs emit_return.
                self.claim_returned_enum_box(val, ret_type)?;
                val = self.load_return_value_if_needed(val)?;
                self.claim_returned_lists(Some(expr), vars);
                val = self.claim_returned_list_literals(val, Some(expr))?;
                // 0.34.41 第二档: ensures with `result` binding (was unbound
                // here — "undefined variable 'result'" family).
                self.compile_ensures_asserts(Some(val), ret_type, vars)?;
                self.pop_shared_scope()?;
                self.flush_heap_scopes_to_boundary()?;
                // 0.34.36: run the method's defers on the early-return path
                // (VM parity) — before the terminator is emitted.
                self.pop_defer_scope(vars)?;
                self.pop_comp_scope();
                self.pop_cap_scope();
                self.build_return(Some(&val))?;
                return Ok(true);
            }
            Stmt::Return(None) => {
                self.compile_ensures_asserts(None, ret_type, vars)?;
                self.pop_shared_scope()?;
                self.flush_heap_scopes_to_boundary()?;
                self.pop_defer_scope(vars)?;
                self.pop_comp_scope();
                self.pop_cap_scope();
                self.build_return(None)?;
                return Ok(true);
            }
            Stmt::Let {
                pat,
                init: Some(init),
                ty,
                ..
            } => {
                // Shared ref copy: let v = shared_var
                if let PatternKind::Variable(name) = &pat.kind {
                    if let Expr::Ident(src_name) = init.unlocated() {
                        if self.shared_var_names.contains(src_name.as_str()) {
                            self.compile_shared_ref_copy(name, src_name, vars)?;
                            return Ok(false);
                        }
                    }
                }
                // Shared var clone: let v = shared_var.clone()
                if let PatternKind::Variable(name) = &pat.kind {
                    if let Expr::Call(callee, cargs) = init.unlocated() {
                        if cargs.is_empty() {
                            if let Expr::Field(obj, method_name) = callee.unlocated() {
                                if method_name == "clone" {
                                    if let Expr::Ident(src_name) = obj.unlocated() {
                                        if self.shared_var_names.contains(src_name.as_str()) {
                                            self.compile_shared_ref_copy(name, src_name, vars)?;
                                            return Ok(false);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let mut val = self.compile_expr(init, vars)?;
                if let Some(decl_ty) = ty {
                    // Registry-backed lookup (audit §6.6); keep the
                    // value-type fallback when the name is unknown.
                    let target = self
                        .llvm_type_for(decl_ty)
                        .unwrap_or_else(|| val.get_type());
                    val = self.adjust_int_val(val, target)?;
                }
                val = self.normalize_string_value(val, init)?;
                if let PatternKind::Variable(name) = &pat.kind {
                    // 0.35.23 deep-eval: turbofish-typed bindings
                    // (from_json::<List<string>> etc.) — the block.rs let
                    // paths track these, the actor-method path did not, so
                    // `for x in parsed { if x == nick }` bound `x` as bare
                    // i64 and the string equality failed E0700 (mimichat
                    // RoomManager::join).
                    if let Expr::Turbofish(_func_name, turbo_type_args, _) = init.unlocated() {
                        if let Some(ta) = turbo_type_args.first() {
                            if let Type::Name(tn, args) = ta.unlocated() {
                                if tn == "List" && !args.is_empty() {
                                    if let Some(full) = self.get_full_type_name(ta) {
                                        self.var_type_names.insert(name.clone(), full);
                                    }
                                    self.var_types.insert(name.clone(), ta.clone());
                                    self.register_list_elem_type(name, ta);
                                } else if (tn == "Map" || tn == "Set") && !args.is_empty() {
                                    if let Some(full) = self.get_full_type_name(ta) {
                                        self.var_type_names.insert(name.clone(), full);
                                    } else {
                                        self.var_type_names.insert(name.clone(), tn.clone());
                                    }
                                    self.var_types.insert(name.clone(), ta.clone());
                                } else {
                                    self.var_type_names.insert(name.clone(), tn.clone());
                                    self.var_types.insert(name.clone(), ta.clone());
                                }
                            }
                        }
                    }
                    if let Expr::Field(_, _) = init.unlocated() {
                        let field_ty = self.infer_object_type(init, vars);
                        if !field_ty.is_empty() {
                            self.var_type_names.insert(name.clone(), field_ty);
                        }
                    }
                    if let Expr::Record { ty: Some(tn), .. } = init.unlocated() {
                        self.var_type_names.insert(name.clone(), tn.clone());
                    } else if let Expr::Call(callee, _) = init.unlocated() {
                        if let Expr::Field(obj, method_name) = callee.unlocated() {
                            if method_name == "spawn" || method_name == "spawn_detached" {
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
                            } else if method_name == "upgrade" {
                                self.track_weak_upgrade_type(name, obj);
                            }
                        } else if let Expr::Ident(func_name) = callee.unlocated() {
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
                                    // Builtin functions: use infer_object_type for return type
                                    if crate::codegen::builtins::is_builtin(func_name) {
                                        let obj_type = self.infer_object_type(init, vars);
                                        if !obj_type.is_empty()
                                            && obj_type.as_str() != func_name.as_str()
                                        {
                                            self.var_type_names.insert(name.clone(), obj_type);
                                        }
                                    } else if let Some(fdef) = self.func_defs.get(func_name) {
                                        if let Some(ret_ty) = &fdef.ret {
                                            match ret_ty.unlocated() {
                                                Type::ImplTrait(traits) => {
                                                    self.var_type_names.insert(
                                                        name.clone(),
                                                        format!("impl {}", traits.join(" + ")),
                                                    );
                                                }
                                                Type::Name(tn, _) => {
                                                    let resolved =
                                                        self.substitute_type_params(ret_ty);
                                                    let type_name = if let Some(full) =
                                                        self.get_full_type_name(&resolved)
                                                    {
                                                        full
                                                    } else {
                                                        tn.clone()
                                                    };
                                                    self.var_type_names
                                                        .insert(name.clone(), type_name);
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Track the "any" type for tuple elements from map_get
                if let PatternKind::Tuple(elements) = &pat.kind {
                    if let Expr::Call(callee, _) = init.unlocated() {
                        if let Expr::Ident(func_name) = callee.unlocated() {
                            if func_name == "map_get" && elements.len() == 2 {
                                // map_get returns (bool, any); mark the second element as "any"
                                if let PatternKind::Variable(name) = &elements[1].kind {
                                    self.var_type_names.insert(name.clone(), "any".to_string());
                                }
                            }
                        }
                    }
                }
                // Track list element type for nested List<List<T>> indexing
                if let PatternKind::Variable(name) = &pat.kind {
                    if let Some(decl_ty) = &ty {
                        // 0.35.23 deep-eval: annotated bindings must also
                        // register var_type_names (block.rs parity) —
                        // `let mut members: List<string> = []` left members
                        // untyped in the actor-method path, so `for x in
                        // members { if x == nick }` bound `x` as bare i64 and
                        // the string equality failed E0700 (mimichat
                        // RoomManager::is_member / remove_from_all).
                        if let Some(full) = self.get_full_type_name(decl_ty) {
                            self.var_type_names.insert(name.clone(), full);
                        } else if let Type::Name(tn, args) = decl_ty.unlocated() {
                            if args.is_empty() {
                                self.var_type_names.insert(name.clone(), tn.clone());
                            }
                        }
                        self.register_list_elem_type(name, decl_ty);
                    }
                }
                self.compile_pattern_bind(pat, val, vars)?;
            }
            Stmt::Assign { target, value } => {
                self.compile_assign_stmt(target, value, vars)?;
            }
            Stmt::If { cond, then_, else_ } => {
                let cond_val = self.compile_expr(cond, vars)?;
                let cond_bool = if let BasicValueEnum::IntValue(iv) = cond_val {
                    iv
                } else {
                    return Err(CompileError::TypeMismatch(
                        "if condition must be boolean".to_string(),
                    ));
                };
                let function = self.current_function().ok_or_else(|| {
                    CompileError::LlvmError(
                        "codegen: no current function for if in actor method".to_string(),
                    )
                })?;
                let then_bb = self.context.append_basic_block(function, "then");
                let else_bb = self.context.append_basic_block(function, "else");
                let merge_bb = self.context.append_basic_block(function, "ifcont");
                self.build_cond_br(cond_bool, then_bb, else_bb)?;
                self.builder.position_at_end(then_bb);
                let mut then_vars = vars.clone();
                let then_val = self.compile_block_last_val(then_, &mut then_vars)?;
                // 0.35.23 deep-eval: branch-value normalization (block.rs /
                // resolved compile_if_expr parity). `if found { to_string(
                // val) } else { "" }` yields a {ptr,i64} struct in one arm
                // and a raw C-string pointer in the other — with mismatched
                // types the phi below is skipped and the actor method's tail
                // value silently became i64 0 (get_nick returned a NULL
                // string ptr → strcmp(NULL, …) SIGSEGV in mimichat
                // test_user_manager_basic). Wrap raw string pointers so both
                // arms share the canonical {ptr,len} layout.
                let then_val = self.normalize_block_last_string(then_val, then_)?;
                let then_reaches = !self.block_has_terminator();
                if then_reaches {
                    self.build_br(merge_bb)?;
                }
                let then_bb_end = then_reaches
                    .then(|| self.builder.get_insert_block())
                    .flatten();
                self.builder.position_at_end(else_bb);
                let (else_val, else_reaches) = if let Some(else_block) = else_ {
                    let mut else_vars = vars.clone();
                    let v = self.compile_block_last_val(else_block, &mut else_vars)?;
                    let v = self.normalize_block_last_string(v, else_block)?;
                    let reaches = !self.block_has_terminator();
                    if reaches {
                        self.build_br(merge_bb)?;
                    }
                    (Some(v), reaches)
                } else {
                    let reaches = !self.block_has_terminator();
                    if reaches {
                        self.build_br(merge_bb)?;
                    }
                    // Synthesize a default value so the merge phi receives an
                    // incoming entry for the else edge. A phi with zero entries
                    // is invalid IR (its entry count must equal the block's
                    // predecessor count); LLVM O0 tolerates it, but O1/O2
                    // verifier passes crash on it (SIGSEGV in mimi build).
                    // Mirrors the func.rs if-statement handling.
                    let default_val = match then_val.get_type() {
                        BasicTypeEnum::IntType(t) => t.const_int(0, false).into(),
                        BasicTypeEnum::FloatType(t) => t.const_float(0.0).into(),
                        _ => self.context.i64_type().const_int(0, false).into(),
                    };
                    (Some(default_val), reaches)
                };
                let else_bb_end = else_reaches
                    .then(|| self.builder.get_insert_block())
                    .flatten();
                self.builder.position_at_end(merge_bb);
                if then_val.get_type()
                    == else_val
                        .as_ref()
                        .map(|v| v.get_type())
                        .unwrap_or(then_val.get_type())
                {
                    let phi = self
                        .builder
                        .build_phi(then_val.get_type(), "if_result")
                        .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
                    if let Some(bb) = then_bb_end {
                        phi.add_incoming(&[(&then_val as &dyn inkwell::values::BasicValue, bb)]);
                    }
                    if let (Some(bb), Some(ev)) = (else_bb_end, else_val) {
                        phi.add_incoming(&[(&ev as &dyn inkwell::values::BasicValue, bb)]);
                    }
                    *last_val = phi.as_basic_value();
                }
            }
            Stmt::For {
                var,
                iterable,
                body,
            } => {
                self.compile_for_stmt(var, iterable, body, vars)?;
            }
            Stmt::While { cond, body } => {
                let function = self.current_function().ok_or_else(|| {
                    CompileError::LlvmError(
                        "codegen: no current function for while loop in actor method".to_string(),
                    )
                })?;
                let loop_bb = self.context.append_basic_block(function, "loop");
                let body_bb = self.context.append_basic_block(function, "loopbody");
                let merge_bb = self.context.append_basic_block(function, "loopcont");
                self.build_br(loop_bb)?;
                self.builder.position_at_end(loop_bb);
                let cond_val = self.compile_expr(cond, vars)?;
                let cond_bool = cond_val.into_int_value();
                self.build_cond_br(cond_bool, body_bb, merge_bb)?;
                self.builder.position_at_end(body_bb);
                // CG-H4: always restore outer break/continue targets.
                let old_break = self.loop_break.take();
                let old_continue = self.loop_continue.take();
                self.loop_break = Some(merge_bb);
                self.loop_continue = Some(loop_bb);
                let body_result = self.compile_block(body, vars);
                let br_result = if body_result.is_ok() && !self.block_has_terminator() {
                    self.build_br(loop_bb)
                } else {
                    Ok(())
                };
                self.loop_break = old_break;
                self.loop_continue = old_continue;
                body_result?;
                br_result?;
                self.builder.position_at_end(merge_bb);
            }
            Stmt::WhileLet { pat, init, body } => {
                self.compile_while_let_stmt(pat, init, body, vars)?;
            }
            Stmt::Loop(body) => {
                let function = self.current_function().ok_or_else(|| {
                    CompileError::LlvmError(
                        "codegen: no current function for loop in actor method".to_string(),
                    )
                })?;
                let loop_bb = self.context.append_basic_block(function, "loop");
                let body_bb = self.context.append_basic_block(function, "loopbody");
                let merge_bb = self.context.append_basic_block(function, "loopcont");
                self.build_br(loop_bb)?;
                self.builder.position_at_end(loop_bb);
                let true_val = self.context.bool_type().const_int(1, false);
                self.build_cond_br(true_val, body_bb, merge_bb)?;
                self.builder.position_at_end(body_bb);
                // CG-H4: always restore outer break/continue targets.
                let old_break = self.loop_break.take();
                let old_continue = self.loop_continue.take();
                self.loop_break = Some(merge_bb);
                self.loop_continue = Some(loop_bb);
                let body_result = self.compile_block(body, vars);
                let br_result = if body_result.is_ok() && !self.block_has_terminator() {
                    self.build_br(loop_bb)
                } else {
                    Ok(())
                };
                self.loop_break = old_break;
                self.loop_continue = old_continue;
                body_result?;
                br_result?;
                self.builder.position_at_end(merge_bb);
            }
            Stmt::Parasteps(block) => {
                self.enter_parasteps();
                self.compile_block(block, vars)?;
                self.leave_parasteps()?;
            }
            Stmt::Drop(expr) => {
                self.compile_expr(expr, vars)?;
            }
            Stmt::Defer(block) => {
                // 0.34.36 (L1 parity, audit §6.7): register the defer block for
                // LIFO execution on scope exit. Previously fell through and was
                // silently dropped; the bytecode VM runs it.
                self.register_defer(block);
            }
            Stmt::OnFailure(block) => {
                // Execution-point registration (cross-agent contract 0.34.36):
                // compensation fires only for faults after this statement —
                // no block pre-scan, keep inline.
                self.register_comp(block);
            }
            Stmt::Arena(block) => {
                self.compile_arena_block(block, vars, "arena")?;
            }
            Stmt::SharedLet {
                kind,
                name,
                ty,
                init,
            } => {
                self.compile_shared_let_stmt(kind, name, ty, init, vars)?;
            }
            Stmt::Func(f) => {
                if f.is_comptime {
                    // Comptime functions: skip codegen (interpreter-only)
                } else {
                    self.func_defs
                        .entry(f.name.clone())
                        .or_insert_with(|| f.clone());
                    let saved_block = self.builder.get_insert_block();
                    let saved_type_map = self.type_map.clone();
                    let saved_var_types = std::mem::take(&mut self.var_types);
                    let saved_var_type_names = std::mem::take(&mut self.var_type_names);
                    let saved_list_elem = std::mem::take(&mut self.list_elem_llvm_types);
                    self.compile_func_legacy(f)?;
                    self.var_types = saved_var_types;
                    self.var_type_names = saved_var_type_names;
                    self.list_elem_llvm_types = saved_list_elem;
                    self.type_map = saved_type_map;
                    if let Some(bb) = saved_block {
                        self.builder.position_at_end(bb);
                    }
                }
            }
            Stmt::Requires(_, _)
            | Stmt::Ensures(_, _)
            | Stmt::Invariant(_, _)
            | Stmt::Math(_)
            | Stmt::Ellipsis => {}
            Stmt::Block(block) => {
                self.compile_block(block, vars)?;
            }
            _ => {}
        }

        Ok(false)
    }

    /// Emit the epilogue of an actor method: scope cleanup, contract checks,
    /// and the implicit return.
    fn emit_actor_method_epilogue(
        &mut self,
        vars: &mut HashMap<String, VarEntry<'ctx>>,
        ret_type: BasicTypeEnum<'ctx>,
        last_val: BasicValueEnum<'ctx>,
        last_expr: Option<&Expr>,
    ) -> MimiResult<()> {
        if self.block_has_terminator() {
            // An early `return` already popped the method's defer scope on its
            // own path; nothing left to balance here.
            return Ok(());
        }
        let last_val = self.adjust_int_val(last_val, ret_type)?;
        // Same string-struct detection as emit_implicit_return (func.rs:1777-1809).
        // When the return type is string struct {ptr,i64} but last_val is a raw C
        // string pointer (from literal), wrap it into a proper struct before loading.
        let last_val = match (last_val, ret_type) {
            (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) => {
                let field_types = st.get_field_types();
                let is_string_struct = field_types.len() == 2
                    && matches!(&field_types[0], BasicTypeEnum::PointerType(_))
                    && matches!(&field_types[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if is_string_struct {
                    self.wrap_c_string(pv)?
                } else {
                    self.load_return_value_if_needed(BasicValueEnum::PointerValue(pv))?
                }
            }
            _ => self.load_return_value_if_needed(last_val)?,
        };
        // 0.39.135 (L1 parity): claim heap-backed scalar-string returns
        // *before* free_heap_scopes — mirrors func.rs emit_implicit_return.
        // Without this, an implicit tail expression like `"hi " + name`
        // left the concat buffer in the scope's heap-alloc registry; the
        // epilogue freed it at the boundary and returned a dangling
        // {ptr,len} — mailbox callers observed an empty/garbage string
        // while the VM printed the real value (kernel-card e18 divergence).
        let last_val = self.claim_string_return_value(last_val, ret_type, last_expr, vars)?;
        // Claim returned List variables' data buffers & literals before flushing heap scopes.
        self.claim_returned_lists(last_expr, vars);
        let last_val = self.claim_returned_list_literals(last_val, last_expr)?;

        self.check_unconsumed_caps()?;
        self.release_all_shared()?;
        self.flush_heap_scopes_to_boundary()?;
        // 0.34.36 (L1 parity, audit §6.7): run the method-level defers on the
        // fall-through exit (the VM executes them before return).
        self.pop_defer_scope(vars)?;
        self.pop_comp_scope();
        self.pop_cap_scope();

        if !self.block_has_terminator() {
            let ensures = self.ensures_stmts.clone();
            for ensures_expr in &ensures {
                self.compile_contract_assert(
                    ensures_expr,
                    vars,
                    super::scope::ContractPhase::Ensures,
                )?;
            }
            self.build_return(Some(&last_val))?;
        }
        Ok(())
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
        // The result is the mailbox result.
        Ok(Some(result_val))
    }
}
