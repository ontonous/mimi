//! Typed native owner ABI for ordinary `(bool, Any)` snapshots.
//!
//! The wire is a generation-bearing owner token. It is never a legacy
//! `ValueHandle`; all reads and recursive product glue retain the sidecar's
//! exact v1 descriptor contract before touching a payload.
use super::*;
use crate::core::mir::types::MirGlueOperation;
use crate::core::mir::{MirManagedAnyContract, MirManagedAnyOperation};

const ANY_PREFIX: &str = "mimi_mir_managed_any_i32_string_v1_";

pub(super) fn validate_managed_any_type(
    catalog: &MirTypeCatalog,
    ty: &crate::core::ResolvedTypeId,
) -> Result<(), String> {
    let canonical = crate::core::mir::types::managed_any_i32_string_v1_type_id();
    let desc = catalog
        .get(ty)
        .ok_or_else(|| "ManagedAny TypeDesc is absent".to_string())?;
    if ty != &canonical
        || desc.id != canonical
        || desc.kind != MirTypeKind::ManagedAnyI32StringV1
        || desc.layout != MirLayout::ManagedAnyI32StringV1
        || desc.ownership != MirOwnership::Move
        || desc.abi != MirAbiClass::OpaqueHandle
    {
        return Err(
            "ManagedAny identity, kind, layout, ownership or ABI is not canonical v1".into(),
        );
    }
    for op in [
        MirGlueOperation::MoveOut,
        MirGlueOperation::Clone,
        MirGlueOperation::Drop,
    ] {
        catalog.validate_glue(ty, op)?;
    }
    Ok(())
}

pub(super) fn validate_managed_any_operation(
    program: &MirProgram,
    function: &MirFunction,
    result: &Option<MirValueId>,
    source: &MirValueId,
    operation: MirManagedAnyOperation,
    contract: Option<&MirManagedAnyContract>,
    subject: &str,
) -> Result<(), NativeMirError> {
    program
        .type_catalog()
        .validate_managed_any_contract(function, source, result.as_ref(), operation, contract)
        .map_err(|message| NativeMirError::new(subject, message))
}

impl<'a, 'ctx> NativeMirEmitter<'a, 'ctx> {
    pub(super) fn declare_managed_any_helpers(&self) {
        use crate::core::mir::MirInstructionKind as I;

        let legacy_any_type = crate::core::mir::types::managed_any_i32_string_v1_type_id();
        let legacy_operations_are_used = self.program.functions().values().any(|function| {
            function
                .values
                .values()
                .any(|value| value.ty == legacy_any_type)
                || function
                    .blocks
                    .values()
                    .flat_map(|block| &block.instructions)
                    .any(|instruction| {
                        matches!(
                            &instruction.kind,
                            I::OrdinaryMapOwnedGetI32StringV1New { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1Set { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1Remove { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1Size { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1Contains { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1GetI32 { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1GetString { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1GetAny { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1Values { .. }
                                | I::OrdinaryMapOwnedGetI32StringV1Drop { .. }
                                | I::ManagedAny { .. }
                        )
                    })
        });
        if !legacy_operations_are_used {
            return;
        }

        let c = self.generator.context;
        let i64 = c.i64_type();
        let i32 = c.i32_type();
        let ptr = c.ptr_type(inkwell::AddressSpace::default());
        let i = BasicMetadataTypeEnum::IntType(i64);
        let k = BasicMetadataTypeEnum::IntType(i32);
        let p = BasicMetadataTypeEnum::PointerType(ptr);
        let helpers = [
            (
                "mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any",
                i32.fn_type(&[i, p, i, p, p], false),
            ),
            (
                "mimi_mir_managed_any_i32_string_v1_clone",
                i32.fn_type(&[i, k, p], false),
            ),
            (
                "mimi_mir_managed_any_i32_string_v1_pack_i32",
                i32.fn_type(&[k, p], false),
            ),
            (
                "mimi_mir_managed_any_i32_string_v1_drop",
                i32.fn_type(&[i, k], false),
            ),
            (
                "mimi_mir_managed_any_i32_string_v1_read_i32",
                i32.fn_type(&[i, k, k, p], false),
            ),
            (
                "mimi_mir_managed_any_i32_string_v1_copy_string",
                i32.fn_type(&[i, k, k, p, p], false),
            ),
        ];
        for (name, ty) in helpers {
            if self.generator.module.get_function(name).is_none() {
                self.generator
                    .module
                    .add_function(name, ty, Some(Linkage::External));
            }
        }
    }
}

impl<'a, 'ctx> NativeMirFunctionEmitter<'a, 'ctx> {
    fn emit_managed_any_status(
        &mut self,
        status: inkwell::values::IntValue<'ctx>,
        message: &str,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        let ok = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.generator.context.i32_type().const_zero(),
                "mir_any_status_ok",
            )
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(ok, message, subject)
    }
    fn emit_managed_any_call(
        &mut self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
        subject: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, NativeMirError> {
        let f = self
            .generator
            .get_runtime_fn(name)
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        call_try_basic_value(
            &self
                .generator
                .builder
                .build_call(f, args, "mir_managed_any_status")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?,
        )
        .map(|v| v.into_int_value())
        .ok_or_else(|| NativeMirError::new(subject, "ManagedAny runtime helper returned void"))
    }
    pub(super) fn emit_ordinary_map_get_any(
        &mut self,
        result: &MirValueId,
        root: &MirValueId,
        key: &MirValueId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        let root = self.emit_live_ordinary_map_owned_get_root(root, subject)?;
        let (data, len) = self.ordinary_map_owned_get_string_parts(key, "GetAny key", subject)?;
        let result_ty = self.value_type(result, subject)?;
        self.program
            .type_catalog()
            .validate_ordinary_map_owned_get_any_result_type(&result_ty)
            .map_err(|message| NativeMirError::new(subject, message))?;
        let c = self.generator.context;
        let found = self
            .generator
            .builder
            .build_alloca(c.i32_type(), "mir_any_found_out")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        let owner = self
            .generator
            .builder
            .build_alloca(c.i64_type(), "mir_any_owner_out")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        self.generator
            .builder
            .build_store(found, c.i32_type().const_zero())
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        self.generator
            .builder
            .build_store(owner, c.i64_type().const_zero())
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        let status = self.emit_managed_any_call(
            "mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any",
            &[
                root.into(),
                data.into(),
                len.into(),
                found.into(),
                owner.into(),
            ],
            subject,
        )?;
        self.emit_managed_any_status(status, "[E0800] ordinary Map Any snapshot failed", subject)?;
        let f = self
            .generator
            .builder
            .build_load(c.i32_type(), found, "mir_any_found")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?
            .into_int_value();
        let token = self
            .generator
            .builder
            .build_load(c.i64_type(), owner, "mir_any_owner")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?
            .into_int_value();
        let valid_found = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                f,
                c.i32_type().const_int(1, false),
                "mir_any_found_valid",
            )
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        let valid_owner = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                token,
                c.i64_type().const_zero(),
                "mir_any_owner_valid",
            )
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        let valid = self
            .generator
            .builder
            .build_and(valid_found, valid_owner, "mir_any_snapshot_valid")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            valid,
            "[E0800] malformed ordinary Map Any snapshot ABI",
            subject,
        )?;
        let boolean = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                f,
                c.i32_type().const_zero(),
                "mir_any_found_bool",
            )
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        let mut tuple = native_basic_type(c, self.program.type_catalog(), &result_ty)?
            .into_struct_type()
            .get_undef();
        tuple = self
            .generator
            .builder
            .build_insert_value(tuple, boolean, 0, "mir_any_tuple_found")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?
            .into_struct_value();
        tuple = self
            .generator
            .builder
            .build_insert_value(tuple, token, 1, "mir_any_tuple_owner")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?
            .into_struct_value();
        Ok(tuple.into())
    }
    pub(super) fn emit_managed_any_clone_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<BasicValueEnum<'ctx>, NativeMirError> {
        validate_managed_any_type(self.program.type_catalog(), ty)
            .map_err(|m| NativeMirError::new(subject, m))?;
        let c = self.generator.context;
        let out = self
            .generator
            .builder
            .build_alloca(c.i64_type(), "mir_any_clone_out")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        self.generator
            .builder
            .build_store(out, c.i64_type().const_zero())
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        let status = self.emit_managed_any_call(
            &format!("{ANY_PREFIX}clone"),
            &[
                value.into_int_value().into(),
                c.i32_type().const_int(1, false).into(),
                out.into(),
            ],
            subject,
        )?;
        self.emit_managed_any_status(status, "[E0800] ManagedAny clone failed", subject)?;
        let token = self
            .generator
            .builder
            .build_load(c.i64_type(), out, "mir_any_cloned_owner")
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?
            .into_int_value();
        let valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::NE,
                token,
                c.i64_type().const_zero(),
                "mir_any_clone_valid",
            )
            .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            valid,
            "[E0800] malformed ManagedAny clone ABI",
            subject,
        )?;
        Ok(token.into())
    }
    pub(super) fn emit_managed_any_drop_value(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        validate_managed_any_type(self.program.type_catalog(), ty)
            .map_err(|m| NativeMirError::new(subject, m))?;
        let version = self.generator.context.i32_type().const_int(1, false);
        let status = self.emit_managed_any_call(
            &format!("{ANY_PREFIX}drop"),
            &[value.into_int_value().into(), version.into()],
            subject,
        )?;
        self.emit_managed_any_status(status, "[E0800] ManagedAny drop failed", subject)
    }

    /// Borrow and print one Checker-certified `Any` value. Probe both closed
    /// runtime tags before writing anything: a failed i32 probe is expected
    /// for String owners, while a failed String probe means E0800. The MIR
    /// receipt proves the source identity and the separate following Drop
    /// settles only the owned call-argument snapshot.
    fn emit_managed_any_println(
        &mut self,
        value: BasicValueEnum<'ctx>,
        ty: &crate::core::ResolvedTypeId,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        validate_managed_any_type(self.program.type_catalog(), ty)
            .map_err(|message| NativeMirError::new(subject, message))?;

        let c = self.generator.context;
        let i32 = c.i32_type();
        let i64 = c.i64_type();
        let owner = value.into_int_value();
        let i32_out = self
            .generator
            .builder
            .build_alloca(i32, "mir_any_println_i32_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(i32_out, i32.const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let i32_status = self.emit_managed_any_call(
            &format!("{ANY_PREFIX}read_i32"),
            &[
                owner.into(),
                i32.const_int(1, false).into(),
                i32.const_int(1, false).into(),
                i32_out.into(),
            ],
            subject,
        )?;
        let i32_valid = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                i32_status,
                i32.const_zero(),
                "mir_any_println_i32_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        let print_i32 = c.append_basic_block(self.llvm_function, "mir_any_println_i32");
        let probe_string = c.append_basic_block(self.llvm_function, "mir_any_println_probe_string");
        let done = c.append_basic_block(self.llvm_function, "mir_any_println_done");
        self.generator
            .builder
            .build_conditional_branch(i32_valid, print_i32, probe_string)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        // The successful i32 probe has already checked owner liveness,
        // descriptor identity/version, and the I32 tag before printf runs.
        self.generator.builder.position_at_end(print_i32);
        let i32_value = self
            .generator
            .builder
            .build_load(i32, i32_out, "mir_any_println_i32")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let i64_value = self
            .generator
            .builder
            .build_int_s_extend(i32_value, i64, "mir_any_println_i32_sext")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let format = self
            .generator
            .builder
            .build_global_string_ptr("%ld\n", "mir_any_println_i32_format")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let printf = self
            .generator
            .get_runtime_fn("printf")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                printf,
                &[
                    BasicMetadataValueEnum::PointerValue(format.as_pointer_value()),
                    BasicMetadataValueEnum::IntValue(i64_value),
                ],
                "mir_any_println_i32_call",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unconditional_branch(done)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;

        // Only a failed i32 probe reaches this block. The String helper then
        // validates the same owner and exact descriptor before producing an
        // owned byte copy; all failure paths trap before either output call.
        self.generator.builder.position_at_end(probe_string);
        let pointer_type = c.ptr_type(inkwell::AddressSpace::default());
        let string_out = self
            .generator
            .builder
            .build_alloca(pointer_type, "mir_any_println_string_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let length_out = self
            .generator
            .builder
            .build_alloca(i64, "mir_any_println_string_len_out")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(string_out, pointer_type.const_null())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_store(length_out, i64.const_zero())
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let string_status = self.emit_managed_any_call(
            &format!("{ANY_PREFIX}copy_string"),
            &[
                owner.into(),
                i32.const_int(2, false).into(),
                i32.const_int(1, false).into(),
                string_out.into(),
                length_out.into(),
            ],
            subject,
        )?;
        self.emit_managed_any_status(string_status, "[E0800] ManagedAny println failed", subject)?;
        let bytes = self
            .generator
            .builder
            .build_load(pointer_type, string_out, "mir_any_println_string")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_pointer_value();
        let length = self
            .generator
            .builder
            .build_load(i64, length_out, "mir_any_println_string_len")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?
            .into_int_value();
        let nonnull = self
            .generator
            .builder
            .build_is_not_null(bytes, "mir_any_println_string_nonnull")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let nonnegative_length = self
            .generator
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                length,
                i64.const_zero(),
                "mir_any_println_string_length_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let valid_string_abi = self
            .generator
            .builder
            .build_and(
                nonnull,
                nonnegative_length,
                "mir_any_println_string_abi_valid",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.emit_ordinary_map_owned_get_root_guard(
            valid_string_abi,
            "[E0800] malformed ManagedAny println String ABI",
            subject,
        )?;

        let print_bytes = self
            .generator
            .get_runtime_fn("mimi_print_bytes")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                print_bytes,
                &[
                    BasicMetadataValueEnum::PointerValue(bytes),
                    BasicMetadataValueEnum::IntValue(length),
                ],
                "mir_any_println_string_bytes",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let newline = self
            .generator
            .builder
            .build_global_string_ptr("\n", "mir_any_println_string_newline")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                print_bytes,
                &[
                    BasicMetadataValueEnum::PointerValue(newline.as_pointer_value()),
                    BasicMetadataValueEnum::IntValue(i64.const_int(1, false)),
                ],
                "mir_any_println_string_newline_call",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        let free = self
            .generator
            .get_runtime_fn("mimi_string_free")
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_call(
                free,
                &[BasicMetadataValueEnum::PointerValue(bytes)],
                "mir_any_println_string_free",
            )
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator
            .builder
            .build_unconditional_branch(done)
            .map_err(|error| NativeMirError::new(subject, error.to_string()))?;
        self.generator.builder.position_at_end(done);
        Ok(())
    }

    pub(super) fn emit_managed_any_operation(
        &mut self,
        result: &Option<MirValueId>,
        source: &MirValueId,
        operation: MirManagedAnyOperation,
        contract: Option<&MirManagedAnyContract>,
        subject: &str,
    ) -> Result<(), NativeMirError> {
        validate_managed_any_operation(
            self.program,
            self.function,
            result,
            source,
            operation,
            contract,
            subject,
        )?;
        if operation == MirManagedAnyOperation::PackI32 {
            let source_value = self.value(source, subject)?.into_int_value();
            let result = result.as_ref().ok_or_else(|| {
                NativeMirError::new(subject, "ManagedAny PackI32 result disappeared")
            })?;
            let c = self.generator.context;
            let out = self
                .generator
                .builder
                .build_alloca(c.i64_type(), "mir_any_pack_i32_out")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            self.generator
                .builder
                .build_store(out, c.i64_type().const_zero())
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            let status = self.emit_managed_any_call(
                &format!("{ANY_PREFIX}pack_i32"),
                &[source_value.into(), out.into()],
                subject,
            )?;
            self.emit_managed_any_status(status, "[E0800] ManagedAny i32 pack failed", subject)?;
            let token = self
                .generator
                .builder
                .build_load(c.i64_type(), out, "mir_any_packed_owner")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?
                .into_int_value();
            let valid = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::NE,
                    token,
                    c.i64_type().const_zero(),
                    "mir_any_pack_valid",
                )
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            self.emit_ordinary_map_owned_get_root_guard(
                valid,
                "[E0800] malformed ManagedAny i32 pack ABI",
                subject,
            )?;
            self.values.insert(result.clone(), token.into());
            return Ok(());
        }
        let value = self.value(source, subject)?;
        let source_ty = self.value_type(source, subject)?;
        match operation {
            MirManagedAnyOperation::PackI32 => {
                return Err(NativeMirError::new(
                    subject,
                    "ManagedAny PackI32 unexpectedly reached owner lookup after the pack fast path",
                ));
            }
            MirManagedAnyOperation::Drop => {
                return self.emit_managed_any_drop_value(value, &source_ty, subject);
            }
            MirManagedAnyOperation::Clone => {
                let clone = self.emit_managed_any_clone_value(value, &source_ty, subject)?;
                let result = result.as_ref().ok_or_else(|| {
                    NativeMirError::new(subject, "ManagedAny clone result disappeared")
                })?;
                self.values.insert(result.clone(), clone);
                return Ok(());
            }
            MirManagedAnyOperation::Println => {
                self.emit_managed_any_println(value, &source_ty, subject)?;
                let result = result.as_ref().ok_or_else(|| {
                    NativeMirError::new(subject, "ManagedAny println result disappeared")
                })?;
                // Unit has no physical LLVM payload; mirror the existing
                // builtin println placeholder convention for downstream MIR.
                self.values.insert(
                    result.clone(),
                    self.generator.context.i64_type().const_zero().into(),
                );
                return Ok(());
            }
            _ => {}
        }
        let result = result
            .as_ref()
            .ok_or_else(|| NativeMirError::new(subject, "ManagedAny unpack result disappeared"))?;
        let c = self.generator.context;
        let version = c.i32_type().const_int(1, false);
        let token = value.into_int_value();
        let unpacked = if operation == MirManagedAnyOperation::UnpackI32 {
            let out = self
                .generator
                .builder
                .build_alloca(c.i32_type(), "mir_any_i32_out")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            self.generator
                .builder
                .build_store(out, c.i32_type().const_zero())
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            let status = self.emit_managed_any_call(
                &format!("{ANY_PREFIX}read_i32"),
                &[
                    token.into(),
                    c.i32_type().const_int(1, false).into(),
                    version.into(),
                    out.into(),
                ],
                subject,
            )?;
            self.emit_managed_any_status(status, "[E0800] ManagedAny i32 unpack failed", subject)?;
            self.generator
                .builder
                .build_load(c.i32_type(), out, "mir_any_i32")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?
        } else {
            let ptr = c.ptr_type(inkwell::AddressSpace::default());
            let out_ptr = self
                .generator
                .builder
                .build_alloca(ptr, "mir_any_string_ptr_out")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            let out_len = self
                .generator
                .builder
                .build_alloca(c.i64_type(), "mir_any_string_len_out")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            self.generator
                .builder
                .build_store(out_ptr, ptr.const_null())
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            self.generator
                .builder
                .build_store(out_len, c.i64_type().const_zero())
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            let status = self.emit_managed_any_call(
                &format!("{ANY_PREFIX}copy_string"),
                &[
                    token.into(),
                    c.i32_type().const_int(2, false).into(),
                    version.into(),
                    out_ptr.into(),
                    out_len.into(),
                ],
                subject,
            )?;
            self.emit_managed_any_status(
                status,
                "[E0800] ManagedAny String unpack failed",
                subject,
            )?;
            let data = self
                .generator
                .builder
                .build_load(ptr, out_ptr, "mir_any_string_ptr")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?
                .into_pointer_value();
            let len = self
                .generator
                .builder
                .build_load(c.i64_type(), out_len, "mir_any_string_len")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?
                .into_int_value();
            let nonnull = self
                .generator
                .builder
                .build_is_not_null(data, "mir_any_string_nonnull")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            let nonnegative = self
                .generator
                .builder
                .build_int_compare(
                    IntPredicate::SGE,
                    len,
                    c.i64_type().const_zero(),
                    "mir_any_string_length_valid",
                )
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            let valid = self
                .generator
                .builder
                .build_and(nonnull, nonnegative, "mir_any_string_valid")
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?;
            self.emit_ordinary_map_owned_get_root_guard(
                valid,
                "[E0800] malformed ManagedAny String ABI",
                subject,
            )?;
            // Runtime owns the fresh allocation in the execution domain before
            // publishing it. Re-registering the same exact allocation is idempotent.
            self.emit_ordinary_map_domain_own_string(data, len, subject)?;
            self.generator
                .build_string_struct(data, len)
                .map_err(|e| NativeMirError::new(subject, e.to_string()))?
        };
        self.values.insert(result.clone(), unpacked);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::codegen::CodeGenerator;
    use crate::core::mir::reference::{MirProgram, MirReferenceInterpreter, MirRuntimeValue};
    use crate::core::mir::MirInstructionKind as I;
    use crate::interp::bytecode::{compile_mir_program, BytecodeVM};
    use crate::interp::Value;
    use inkwell::context::Context;

    const SNAPSHOT_SOURCE: &str = r#"
    func pass<T>(value: T) -> T { value }
    func main() -> i32 {
        match map_new() {
            Ok(empty) => {
                match map_set(empty, "k\0🧪", "detached") {
                    Ok(root) => {
                        let snapshot = map_get(root, "k\0🧪")
                        drop(empty)
                        drop(root)
                        let found = snapshot.0
                        let payload = snapshot.1
                        let transferred = pass(payload)
                        drop(snapshot)
                        println(found)
                        println(transferred)
                        drop(transferred)
                        match map_new() {
                            Ok(second_empty) => {
                                match map_set(second_empty, "zero", 0) {
                                    Ok(zero) => {
                                        match map_set(zero, "empty", "") {
                                            Ok(empty_text) => {
                                                let missing = map_get(empty_text, "missing")
                                                let present_zero = map_get(empty_text, "zero")
                                                let present_empty = map_get(empty_text, "empty")
                                                drop(second_empty)
                                                drop(zero)
                                                drop(empty_text)
                                                let missing_found = missing.0
                                                let missing_any = missing.1
                                                let missing_zero = if missing_found { missing_any } else { 0 }
                                                println(missing_found)
                                                drop(missing_any)
                                                drop(missing)
                                                println(missing_zero)
                                                let zero_found = present_zero.0
                                                let zero_any = present_zero.1
                                                let present_zero_value = if zero_found { zero_any } else { 0 }
                                                println(zero_found)
                                                drop(zero_any)
                                                drop(present_zero)
                                                println(present_zero_value)
                                                let empty_found = present_empty.0
                                                let empty_any = present_empty.1
                                                println(empty_found)
                                                println(empty_any)
                                                drop(empty_any)
                                                drop(present_empty)
                                                0
                                            },
                                            Err(status) => {
                                                drop(second_empty)
                                                drop(zero)
                                                status
                                            },
                                        }
                                    },
                                    Err(status) => {
                                        drop(second_empty)
                                        status
                                    },
                                }
                            },
                            Err(status) => { status },
                        }
                    },
                    Err(status) => {
                        drop(empty)
                        status
                    },
                }
            },
            Err(status) => { status },
        }
    }
"#;

    const ANY_STRING_EQUALITY_SOURCE: &str = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "key\0🧪", "owned\0🧪值") {
                Ok(root) => {
                    let pair = map_get(root, "key\0🧪")
                    let payload = pair.1
                    let equal_left = payload == "owned\0🧪值"
                    let equal_right = "owned\0🧪值" == payload
                    let unequal_left = payload != "owned\0🧪值x"
                    let unequal_right = "owned\0🧪值x" != payload
                    println(equal_left)
                    println(equal_right)
                    println(unequal_left)
                    println(unequal_right)
                    drop(pair)
                    drop(payload)
                    drop(empty)
                    drop(root)
                    match map_new() {
                        Ok(number_empty) => {
                            match map_set(number_empty, "number", 7) {
                                Ok(number_root) => {
                                    let number_pair = map_get(number_root, "number")
                                    let number_payload = number_pair.1
                                    let wrong_equal = number_payload == "7"
                                    let wrong_unequal = "7" != number_payload
                                    println(wrong_equal)
                                    println(wrong_unequal)
                                    drop(number_pair)
                                    drop(number_payload)
                                    drop(number_empty)
                                    drop(number_root)
                                    0
                                },
                                Err(status) => {
                                    drop(number_empty)
                                    status
                                },
                            }
                        },
                        Err(status) => { status },
                    }
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#;

    const MANAGED_ANY_PRINTLN_SOURCE: &str = r#"
func carry<T>(value: T) -> T { value }
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "k\0🧪", "owned\0🧪值") {
                Ok(root) => {
                    let pair = map_get(root, "k\0🧪")
                    drop(empty)
                    drop(root)
                    let found = pair.0
                    let payload = pair.1
                    let carried = carry(payload)
                    let equal = if found { carried == "owned\0🧪值" } else { false }
                    println(found)
                    println(equal)
                    println(carried)
                    drop(pair)
                    drop(payload)
                    drop(carried)
                    match map_new() {
                        Ok(number_empty) => {
                            match map_set(number_empty, "zero", 0) {
                                Ok(number_root) => {
                                    let number_pair = map_get(number_root, "zero")
                                    drop(number_empty)
                                    drop(number_root)
                                    let number_found = number_pair.0
                                    let zero_payload = number_pair.1
                                    let zero_carried = carry(zero_payload)
                                    let zero = if number_found { zero_carried } else { 0 }
                                    println(number_found)
                                    println(zero)
                                    drop(number_pair)
                                    drop(zero_payload)
                                    drop(zero_carried)
                                    0
                                },
                                Err(status) => {
                                    drop(number_empty)
                                    status
                                },
                            }
                        },
                        Err(status) => { status },
                    }
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#;

    const MANAGED_ANY_PRINTLN_I32_SOURCE: &str = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => {
            match map_set(empty, "zero", 42) {
                Ok(root) => {
                    let pair = map_get(root, "zero")
                    drop(empty)
                    drop(root)
                    println(pair.1)
                    drop(pair)
                    0
                },
                Err(status) => {
                    drop(empty)
                    status
                },
            }
        },
        Err(status) => { status },
    }
}
"#;

    fn checked_and_program(source: &str) -> (crate::core::CheckedProgram, MirProgram) {
        let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
        let file = crate::parser::Parser::new(tokens)
            .parse_file()
            .expect("parse");
        let checked = crate::core::check_program(&file).expect("checked ordinary Any source");
        let program = MirProgram::from_checked_program(&checked).expect("sealed ordinary Any MIR");
        (checked, program)
    }

    fn program(source: &str) -> MirProgram {
        checked_and_program(source).1
    }
    fn strict_memcheck(generator: &CodeGenerator<'_>, expected_exit: i32, expected_output: &str) {
        if std::process::Command::new("valgrind")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!(
                "Valgrind unavailable: ordinary Any snapshot memory evidence remains unverified"
            );
            return;
        }
        let observed = crate::tests::link_and_observe_canonical_mir_with_config(
            generator,
            &crate::tests::E2EConfig {
                use_valgrind: true,
                retain_tmp_if_requested: true,
                valgrind_args: vec![
                    "--leak-check=full".into(),
                    "--show-leak-kinds=all".into(),
                    "--errors-for-leak-kinds=definite,indirect,possible".into(),
                    "--error-exitcode=97".into(),
                ],
                ..crate::tests::E2EConfig::default()
            },
        )
        .expect("strict ordinary Any native memory process");
        assert_eq!(
            observed.exit_code,
            Some(expected_exit),
            "{}",
            observed.stderr
        );
        assert_eq!(observed.stdout, expected_output);
        let all_freed = observed
            .stderr
            .contains("All heap blocks were freed -- no leaks are possible");
        for class in ["definitely lost", "indirectly lost", "possibly lost"] {
            assert!(
                all_freed
                    || observed
                        .stderr
                        .contains(&format!("{class}: 0 bytes in 0 blocks")),
                "{}",
                observed.stderr
            );
        }
        assert!(
            observed.stderr.contains("ERROR SUMMARY: 0 errors"),
            "{}",
            observed.stderr
        );
        eprintln!("{}", observed.stderr);
    }
    #[test]
    fn ordinary_map_any_get_snapshot_escape_is_one_mir_across_reference_vm_native() {
        let (checked, program) = checked_and_program(SNAPSHOT_SOURCE);
        let route_receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
        let digest = program.canonical_digest();
        super::super::validate_mir_native(&program).expect("snapshot native preflight");
        let observed = MirReferenceInterpreter::new(&program)
            .with_route_receipt(&route_receipt)
            .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
            .expect("reference owned snapshot escape");
        assert_eq!(observed.value, MirRuntimeValue::Int(0));
        assert_eq!(
            observed.output,
            "true\ndetached\nfalse\n0\ntrue\n0\ntrue\n\n"
        );
        let bytecode =
            crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_get_result_branch_v3_route(
                &checked,
                &program,
            )
            .expect("snapshot AST-free bytecode");
        assert!(bytecode.ast.is_none());
        let mut vm = BytecodeVM::new(bytecode);
        assert_eq!(
            vm.run_value().expect("VM owned snapshot escape"),
            Value::Int(0)
        );
        assert_eq!(vm.take_stdout(), observed.output);
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "ordinary_map_any_snapshot_escape");
        generator
            .compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
                &checked,
                &program,
                &route_receipt,
            )
            .expect("same MIR native owner ABI");
        generator
            .module
            .verify()
            .expect("snapshot native module verifies");
        let ir = generator.module.print_to_string().to_string();
        for name in [
            "mimi_mir_ordinary_map_owned_get_i32_string_v1_get_any",
            "mimi_mir_managed_any_i32_string_v1_pack_i32",
            "mimi_mir_managed_any_i32_string_v1_clone",
            "mimi_mir_managed_any_i32_string_v1_drop",
            "mimi_mir_managed_any_i32_string_v1_copy_string",
            "mimi_mir_managed_any_i32_string_v1_read_i32",
        ] {
            assert!(
                ir.contains(&format!("call i32 @{name}")),
                "missing typed owner call {name}"
            );
        }
        assert!(!ir.contains("call i64 @mimi_map_get"));
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("same MIR native process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, observed.output);
        assert_eq!(native.stderr, "");
        strict_memcheck(&generator, 0, &observed.output);
        assert_eq!(
            program.canonical_digest(),
            digest,
            "a consumer changed the shared MIR"
        );
    }

    #[test]
    fn managed_any_string_equality_is_length_aware_borrowed_and_wrong_kind_is_false() {
        let (checked, program) = checked_and_program(ANY_STRING_EQUALITY_SOURCE);
        let route_receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
        let expected = "true\ntrue\ntrue\ntrue\nfalse\ntrue\n";
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "managed_any_string_equality");
        generator
            .compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
                &checked,
                &program,
                &route_receipt,
            )
            .expect("exact ManagedAny/String equality native profile");
        generator
            .module
            .verify()
            .expect("ManagedAny/String equality module verifies");
        let ir = generator.module.print_to_string().to_string();
        assert!(
            ir.contains("call i32 @memcmp"),
            "comparison uses explicit bytes"
        );
        assert!(
            ir.contains("mir_any_string_copy_kind_mismatch"),
            "wrong payload kind is a comparison result"
        );
        assert!(
            ir.contains("ManagedAny String equality copy failed"),
            "non-TypeMismatch runtime statuses take the E0800 failure path"
        );
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("ManagedAny/String equality native process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout, expected);
        assert_eq!(native.stderr, "");
        strict_memcheck(&generator, 0, expected);
    }

    #[test]
    fn managed_any_println_checks_tag_then_preserves_exact_payload_bytes() {
        let expected = "true\ntrue\nowned\0🧪值\ntrue\n0\n";
        let (checked, program) = checked_and_program(MANAGED_ANY_PRINTLN_SOURCE);
        let route_receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
        super::super::validate_mir_native(&program).expect("exact Any println native preflight");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "managed_any_println");
        generator
            .compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
                &checked,
                &program,
                &route_receipt,
            )
            .expect("receipted Any println native profile");
        generator
            .module
            .verify()
            .expect("ManagedAny println module verifies");
        let emitted_ir = generator.module.print_to_string().to_string();
        let native = crate::tests::link_and_observe_canonical_mir(&generator)
            .expect("same MIR native Any println process");
        assert_eq!(native.exit_code, Some(0), "{}", native.stderr);
        assert_eq!(native.stdout.as_bytes(), expected.as_bytes());
        assert_eq!(native.stderr, "");
        strict_memcheck(&generator, 0, expected);
        for helper in [
            "mimi_mir_managed_any_i32_string_v1_read_i32",
            "mimi_mir_managed_any_i32_string_v1_copy_string",
            "mimi_print_bytes",
            "mimi_string_free",
            "printf",
        ] {
            assert!(
                emitted_ir.contains(&format!("@{helper}")),
                "missing checked Any println helper {helper}"
            );
        }
        assert!(emitted_ir.contains("mir_any_println_probe_string"));
        assert!(emitted_ir.contains("ManagedAny println failed"));
        assert!(emitted_ir.contains("mir_any_println_string_length_valid"));
    }

    #[test]
    fn managed_any_println_i32_owner_v2_requires_production_readiness_before_llvm() {
        const {
            assert!(!crate::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
        };
        let program = program(MANAGED_ANY_PRINTLN_I32_SOURCE);
        super::super::validate_mir_native(&program)
            .expect("structurally valid owner-v2 i32 println MIR");
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "managed_any_println_i32_readiness");
        let before_compile = generator.module.print_to_string().to_string();
        let errors = generator
            .compile_mir_native(&program)
            .expect_err("owner-v2 i32 println cannot bypass closed production readiness");
        assert!(errors.iter().any(|error| {
            error.code.as_deref() == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                && error.message.contains(
                    "ordinary Map/Any owner-v2 native emission is diagnostic-only while V2 production readiness is disabled",
                )
        }), "{errors:?}");
        assert_eq!(
            generator.module.print_to_string().to_string(),
            before_compile,
            "closed readiness must reject before adding LLVM declarations or definitions"
        );
    }

    #[test]
    fn managed_any_println_missing_or_forged_receipt_fails_before_llvm_effects() {
        let i32_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
        for mutation in [
            "missing_receipt",
            "checker_source_type",
            "source_type",
            "descriptor_version",
            "expected_kind",
        ] {
            let mut program = program(MANAGED_ANY_PRINTLN_SOURCE);
            let mut main = program
                .functions()
                .values()
                .find(|function| function.owner == crate::core::NodeId("function:main".into()))
                .expect("fixture main")
                .clone();
            let contract = main
                .blocks
                .values_mut()
                .flat_map(|block| block.instructions.iter_mut())
                .find_map(|instruction| match &mut instruction.kind {
                    I::ManagedAny {
                        operation: crate::core::mir::MirManagedAnyOperation::Println,
                        contract,
                        ..
                    } => Some(contract),
                    _ => None,
                })
                .expect("receipt-backed ManagedAny println");
            if mutation == "missing_receipt" {
                *contract = None;
            } else {
                let contract = contract.as_mut().expect("println contract");
                match mutation {
                    "checker_source_type" => contract.checker_source_type = Some(i32_type.clone()),
                    "source_type" => {
                        contract.source_type = contract
                            .checker_source_type
                            .clone()
                            .expect("exact Checker source type")
                    }
                    "descriptor_version" => contract.descriptor_version = 2,
                    "expected_kind" => contract.expected_kind = Some(1),
                    _ => unreachable!("listed mutation"),
                }
            }
            program.replace_function_for_test_only(main);

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "managed_any_println_forgery");
            let preexisting_declarations =
                ["main", "printf", "mimi_print_bytes", "mimi_string_free"]
                    .map(|name| (name, generator.module.get_function(name).is_some()));
            generator
                .compile_mir_native(&program)
                .expect_err("missing or forged Any println receipt must fail preflight");
            for (name, existed_before_compile) in preexisting_declarations {
                assert!(
                    generator.module.get_function(name).is_some() == existed_before_compile,
                    "{mutation} must be rejected before adding LLVM declaration {name}"
                );
            }
        }
    }

    #[test]
    fn managed_any_string_equality_rejects_forged_type_or_operator_before_llvm() {
        let managed_type = crate::core::mir::types::managed_any_i32_string_v1_type_id();
        let string_type = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::String);
        for mutation in ["operand_type", "operator"] {
            let mut program = program(ANY_STRING_EQUALITY_SOURCE);
            let mut main = program
                .functions()
                .values()
                .find(|function| function.owner == crate::core::NodeId("function:main".into()))
                .expect("fixture main")
                .clone();
            let (comparison_instruction, string_operand) = main
                .blocks
                .values()
                .flat_map(|block| block.instructions.iter())
                .find_map(|instruction| match &instruction.kind {
                    I::Binary {
                        left,
                        right,
                        op: crate::core::ir::ResolvedBinaryOp::Equal,
                        ..
                    } if main
                        .values
                        .get(left)
                        .is_some_and(|value| value.ty == managed_type)
                        && main
                            .values
                            .get(right)
                            .is_some_and(|value| value.ty == string_type) =>
                    {
                        Some((instruction.id.clone(), right.clone()))
                    }
                    _ => None,
                })
                .expect("sealed Any/String comparison");
            if mutation == "operand_type" {
                main.values
                    .get_mut(&string_operand)
                    .expect("String comparison operand")
                    .ty = crate::core::ir::primitive_type_id(crate::core::PrimitiveType::I32);
            } else {
                let instruction = main
                    .blocks
                    .values_mut()
                    .flat_map(|block| block.instructions.iter_mut())
                    .find(|instruction| instruction.id == comparison_instruction)
                    .expect("sealed Any/String comparison");
                if let I::Binary { op, .. } = &mut instruction.kind {
                    *op = crate::core::ir::ResolvedBinaryOp::Less;
                }
            }
            program.replace_function_for_test_only(main);

            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "managed_any_string_equality_forgery");
            generator
                .compile_mir_native(&program)
                .expect_err("forged Any/String comparison must fail native preflight");
            assert!(
                generator.module.get_function("main").is_none(),
                "{mutation} rejection must precede LLVM function declarations"
            );
        }
    }

    #[test]
    fn ordinary_map_any_unsealed_operation_rejects_before_main_effects() {
        for mutation in [
            "missing",
            "version",
            "origin",
            "kind",
            "source",
            "permissions_missing",
            "permissions_view",
        ] {
            let mut program = program(SNAPSHOT_SOURCE);
            let mut function = if mutation.starts_with("permissions_") {
                // `pass<T>` specializes to an owned Any parameter; changing this table can
                // otherwise turn a transferred callee owner into a loan.
                program
                    .functions()
                    .values()
                    .find(|function| {
                        function.parameters.len() == 1
                            && function.parameters.first().is_some_and(|parameter| {
                                function.values.get(parameter).is_some_and(|value| {
                                    program.type_catalog().contains_managed_any(&value.ty)
                                })
                            })
                            && function.owner != crate::core::NodeId("function:main".into())
                    })
                    .expect("managed Any parameter callee")
                    .clone()
            } else {
                program
                    .functions()
                    .values()
                    .find(|f| {
                        f.blocks.values().any(|b| {
                            b.instructions
                                .iter()
                                .any(|i| matches!(i.kind, I::ManagedAny { .. }))
                        })
                    })
                    .expect("managed snapshot frame")
                    .clone()
            };
            if mutation == "permissions_missing" {
                assert_eq!(function.parameters.len(), 1, "exact owned Any entry");
                assert!(
                    function.parameter_permissions.is_some(),
                    "Checker installed complete permission table"
                );
                function.parameter_permissions = None;
            } else if mutation == "permissions_view" {
                assert_eq!(function.parameters.len(), 1, "exact owned Any entry");
                let permission = function
                    .parameter_permissions
                    .as_mut()
                    .expect("Checker installed complete permission table")
                    .get_mut(0)
                    .expect("owned Any parameter permission");
                assert_eq!(
                    *permission, None,
                    "first slice admits the owned parameter entry"
                );
                *permission = Some(crate::core::ir::Permission::View);
            } else {
                let (source, contract) = function
                    .blocks
                    .values_mut()
                    .flat_map(|b| &mut b.instructions)
                    .find_map(|i| match &mut i.kind {
                        I::ManagedAny {
                            source, contract, ..
                        } => Some((source, contract)),
                        _ => None,
                    })
                    .expect("receipt-backed managed operation");
                match mutation {
                    "missing" => *contract = None,
                    "version" => contract.as_mut().expect("contract").descriptor_version = 2,
                    "origin" => {
                        contract.as_mut().expect("contract").origin =
                            crate::core::NodeId("forged:origin".into())
                    }
                    "kind" => contract.as_mut().expect("contract").expected_kind = Some(9),
                    _ => {
                        *source = crate::core::mir::MirValueId::new("forged:source")
                            .expect("value identity")
                    }
                }
            }
            program.replace_function_for_test_only(function);
            let reference = MirReferenceInterpreter::new(&program);
            reference
                .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
                .expect_err("reference must preflight every sealed receipt before effects");
            assert_eq!(reference.captured_output(), "", "{mutation}");
            compile_mir_program(&program).expect_err("VM adapter must reject the same forgery");
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "ordinary_map_any_forgery");
            generator
                .compile_mir_native(&program)
                .expect_err("native must reject before declarations");
            assert!(
                generator.module.get_function("main").is_none(),
                "{mutation}"
            );
        }
    }

    #[test]
    fn ordinary_map_any_wrong_kind_and_callee_traps_release_escaped_owners() {
        for (helper, main_tail, expected_code) in [
            (
                "",
                "if pair.0 {\n        drop(pair)\n        return transferred\n        0\n    } else {\n        drop(transferred)\n        drop(pair)\n        0\n    }",
                "E0800",
            ),
            (
                "func divide_trap() -> i32 { let zero = 0; 1 / zero }",
                "let trap_result = divide_trap()\n    drop(transferred)\n    drop(pair)\n    trap_result",
                "E0801",
            ),
            (
                "func fail<T>(value: T) -> i32 { let zero = 0; 1 / zero }",
                "let trap_result = fail(transferred)\n    drop(pair)\n    trap_result",
                "E0801",
            ),
            (
                "func overflow_trap() -> i32 { let largest = 2147483647; largest + 1 }",
                "let trap_result = overflow_trap()\n    drop(transferred)\n    drop(pair)\n    trap_result",
                "E0802",
            ),
            (
                "func fail<T>(value: T) -> i32 { let largest = 2147483647; largest + 1 }",
                "let trap_result = fail(transferred)\n    drop(pair)\n    trap_result",
                "E0802",
            ),
        ] {
            let source = format!(
                r#"
{helper}
func pass<T>(value: T) -> T {{ value }}
func main() -> i32 {{
    match map_new() {{
        Ok(empty) => {{
            match map_set(empty, "key", "caller-owned") {{
                Ok(root) => {{
                    let pair = map_get(root, "key")
                    let transferred = pass(pair.1)
                    drop(empty)
                    drop(root)
                    println(77)
                    {main_tail}
                }},
                Err(status) => {{
                    drop(empty)
                    status
                }},
            }}
        }},
        Err(status) => {{ status }},
    }}
}}
"#
            );
            let (checked_program, program) = checked_and_program(&source);
            let route_receipt = program
                .route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
            let reference =
                MirReferenceInterpreter::new(&program).with_route_receipt(&route_receipt);
            reference
                .execute_with_output(&crate::core::NodeId("function:main".into()), &[])
                .expect_err("reference kind/callee trap");
            assert_eq!(reference.captured_output(), "77\n");
            let mut vm = BytecodeVM::new(
                crate::interp::bytecode::compile_mir_program_with_ordinary_map_owned_get_result_branch_v3_route(
                    &checked_program,
                    &program,
                )
                .expect("trap bytecode"),
            );
            vm.run_value().expect_err("VM kind/callee trap");
            assert_eq!(vm.take_stdout(), "77\n");
            let context = Context::create();
            let mut generator = CodeGenerator::new(&context, "ordinary_map_any_owned_callee_trap");
            generator
                .compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
                    &checked_program,
                    &program,
                    &route_receipt,
                )
                .expect("typed trap module");
            generator.module.verify().expect("trap module verifies");
            let native = crate::tests::link_and_observe_canonical_mir(&generator)
                .expect("real native trap process");
            assert_eq!(native.exit_code, Some(1), "{}", native.stderr);
            assert_eq!(native.stdout, "77\n");
            assert!(native.stderr.contains(expected_code), "{}", native.stderr);
            strict_memcheck(&generator, 1, "77\n");
        }
    }
    #[test]
    fn ordinary_map_any_checked_native_gate_requires_exact_conversion_origin_coverage() {
        fn checked(source: &str) -> crate::core::CheckedProgram {
            let tokens = crate::lexer::Lexer::new(source).tokenize().expect("lex");
            let file = crate::parser::Parser::new(tokens)
                .parse_file()
                .expect("parse");
            crate::core::check_program(&file).expect("checked source")
        }
        let checked_source = checked(SNAPSHOT_SOURCE);
        let program = MirProgram::from_checked_program(&checked_source).expect("managed snapshots");
        let receipt =
            program.route_receipt(crate::core::mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "ordinary_map_any_checked_gate");
        generator
            .compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
                &checked_source,
                &program,
                &receipt,
            )
            .expect("only exact checked conversion coverage authorizes native owner reads");
        generator
            .module
            .verify()
            .expect("exact native graph verifies");
        let foreign = checked(
            r#"
func get_or_default<T>(m: Record, key: string, default: T) -> T {
    let result = map_get(m, key)
    if result.0 { result.1 } else { default }
}

func main() -> i32 {
    match map_new() {
        Ok(root) => {
            let wrong = get_or_default(root, "key", 0)
            drop(wrong)
            drop(root)
            0
        },
        Err(status) => { status },
    }
}
"#,
        );
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "ordinary_map_any_foreign_checked_gate");
        let preflight_errors = generator
            .preflight_native_dynamic_any_unpack(&foreign)
            .expect_err("uncovered bool conversion must preserve native E0722");
        assert!(
            preflight_errors
                .iter()
                .any(|e| e.code.as_deref() == Some("E0722")),
            "{preflight_errors:#?}"
        );
        let errors = generator
            .compile_checked_mir_native_with_ordinary_map_owned_get_result_branch_v3_route(
                &foreign, &program, &receipt,
            )
            .expect_err("uncovered CheckedProgram must be rejected by ResultBranchV3 native gate");
        assert!(
            errors.iter().any(|e| e.code.as_deref()
                == Some(crate::core::mir::MIR_ROUTE_COVERAGE_ERROR_CODE)),
            "{errors:#?}"
        );
        assert!(
            generator.module.get_function("main").is_none(),
            "uncovered source conversion started LLVM emission"
        );
    }
}
