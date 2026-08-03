//! v0.34.18a (ADR-002 / amendment clause 1): codegen panic→Fault absorption.
//!
//! Sparse mode (amendment clause 1) makes an undeclared `(state, event)` a
//! compile error; the repealed white-paper "auto-complete every undefined edge
//! to Fault" (`@dense` N×M injection) is gone. A transition may still *declare*
//! that it can fault via a multi-target return `-> State | Fault`. Inside such a
//! transition, a runtime panic (division by zero, integer overflow, float
//! not-finite) is absorbed into the `Fault` variant instead of aborting the
//! process — the compiler bottoms out the Fault payload (the author declares the
//! fault *capability*; they do not construct the Fault record).
//!
//! This mirrors the bytecode VM's `absorb_flow_fault` (vm.rs), giving dual-backend
//! parity. The mechanism is pure structured control flow: each trapping operation
//! branches to a Fault record construction + multi-target wrap + return. There is
//! deliberately NO setjmp/longjmp or signal-handler recovery — the runtime removed
//! those as undefined behaviour (runtime/mod.rs:19044), and this design respects
//! that decision.
//!
//! All Fault payload fields are compile-time constants at the trap site
//! (`last_state` = the transition's source state, `unexpected_event` =
//! `"panic:<code>"`), so the record is built directly at the LLVM level without
//! needing the variable map — this lets the hook live inside expression
//! compilation (`compile_int_binop`, a `&mut self` context).

use crate::ast::{Type, TypeDefKind};
use crate::codegen::CodeGenerator;
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;

impl<'ctx> CodeGenerator<'ctx> {
    /// True when compiling a transition whose multi-target return includes
    /// `Fault` — i.e. a transition that declared it can fault (`-> S | Fault`).
    /// Only such transitions absorb panics; everywhere else a trap aborts.
    pub(in crate::codegen) fn in_fallible_multi_target(&self) -> bool {
        self.in_multi_target_transition && self.multi_target_states.iter().any(|s| s == "Fault")
    }

    /// Emit the panic→Fault absorption epilogue at a trap site: build the
    /// constant `Fault` record, wrap it into the multi-target union with the
    /// `Fault` tag, and return it from the transition function.
    ///
    /// Caller must have checked `in_fallible_multi_target()`. `panic_code` is the
    /// diagnostic code embedded as `unexpected_event = "panic:<code>"`
    /// (e.g. `"E0801"` for division by zero / overflow, `"E0813"` for float).
    ///
    /// H3 (audit-codegen): before returning, run the same cleanup sequence as
    /// the normal return path (block.rs) — otherwise every heap allocation the
    /// transition body made before the trap site (string concat, list push,
    /// closure env…) leaks per absorption (valgrind: N absorbs × M heap
    /// objects definitely lost). The fault record itself is stack-allocated
    /// with `.rodata` string constants, so no claim is needed; the multi-target
    /// payload box is registered by the CALLER (method.rs), not here, so
    /// flushing cannot free it. Defers are intentionally NOT run: the bytecode
    /// VM truncates the frame on absorption without running defers either
    /// (dual-backend parity, audit M2).
    pub(in crate::codegen) fn emit_panic_fault_return(
        &mut self,
        panic_code: &str,
    ) -> Result<(), CompileError> {
        let last_state = self.current_from_state.clone();
        let unexpected_event = format!("panic:{}", panic_code);

        let fault_val = self.build_fault_record(&last_state, &unexpected_event)?;

        // C1 fix: tag = 'Fault' ordinal in the flow-wide name-sorted
        // __MultiTarget enum (must match register_flow_multi_target_enums
        // variant ordering, same rule as the normal multi-target return path
        // in func.rs/block.rs). The per-transition subset ordinal would
        // silently alias another state when the transition's target set is a
        // proper subset of the flow union.
        let tag = self
            .multi_target_global_ordinals
            .get(&self.current_flow_name)
            .and_then(|m| m.get("Fault"))
            .copied()
            .ok_or_else(|| {
                CompileError::LlvmError(format!(
                    "emit_panic_fault_return: 'Fault' has no global multi-target ordinal (flow: {:?})",
                    self.current_flow_name
                ))
            })?;
        let fault_ty = self.type_llvm.get("Fault").copied();

        let union_val = self.wrap_multi_target_value(fault_val, tag, fault_ty)?;
        // H3: cleanup before return (parity with block.rs normal return path,
        // minus ensures/defer which the interp absorber also skips).
        self.emit_all_shared_releases()?;
        self.discard_shared_scope();
        self.flush_heap_scopes_to_boundary()?;
        self.discard_defer_scope();
        self.pop_comp_scope();
        self.builder
            .build_return(Some(&union_val))
            .map_err(|e| CompileError::LlvmError(format!("fault return error: {}", e)))?;
        Ok(())
    }

    /// Build a constant `Fault` record value with the flat fields set to the
    /// panic context and the structured `trace` mirroring them. Field set is
    /// read from `type_defs["Fault"]` so the conditional typed-`error` field is
    /// handled (defaulted) automatically.
    fn build_fault_record(
        &self,
        last_state: &str,
        unexpected_event: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let llvm_ty = *self
            .type_llvm
            .get("Fault")
            .ok_or_else(|| CompileError::LlvmError("Fault type not registered".to_string()))?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(CompileError::LlvmError("Fault is not a struct".to_string()));
        };
        let fields = self.record_fields_of_type("Fault")?;

        let alloca = self.build_alloca(sty, "fault_rec")?;
        for (i, field) in fields.iter().enumerate() {
            let field_llvm_ty = sty
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("Fault field {} type", i)))?;
            let val = match field.name.as_str() {
                "last_state" => self.build_const_string(last_state)?,
                "unexpected_event" => self.build_const_string(unexpected_event)?,
                // snapshot="" matches the bytecode absorber (vm.rs absorb_flow_fault
                // → make_fault_value(from_state, "panic:<code>", "")) — L1 parity.
                "snapshot" => self.build_const_string("")?,
                "trace" => self.build_system_trace(last_state, unexpected_event)?,
                // typed `error` field (or anything else) — default value.
                _ => self.build_default_value(&field.ty, field_llvm_ty)?,
            };
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("fault gep: {}", e)))?;
            self.build_store(gep, val)?;
        }
        self.build_load(BasicTypeEnum::StructType(sty), alloca, "fault_val")
    }

    /// Build the structured `SystemTrace` mirroring `flow_matrix::make_fault_value`
    /// field-for-field (bytecode reference): `last_state_name`/`unexpected_event`
    /// from the panic context, `snapshot=""`, a populated `memory_dump`
    /// (`fields="from_state=<S>;event=<E>"`, `count=2`) and `panic_payload`
    /// (`error_type=<E>`, `file=""`, `line=0`, `stack=""`).
    fn build_system_trace(
        &self,
        last_state: &str,
        unexpected_event: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let llvm_ty = *self.type_llvm.get("SystemTrace").ok_or_else(|| {
            CompileError::LlvmError("SystemTrace type not registered".to_string())
        })?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(CompileError::LlvmError(
                "SystemTrace is not a struct".to_string(),
            ));
        };
        let fields = self.record_fields_of_type("SystemTrace")?;
        let alloca = self.build_alloca(sty, "trace_rec")?;
        for (i, field) in fields.iter().enumerate() {
            let field_llvm_ty = sty
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("trace field {} type", i)))?;
            let val = match field.name.as_str() {
                "last_state_name" => self.build_const_string(last_state)?,
                "unexpected_event" => self.build_const_string(unexpected_event)?,
                "snapshot" => self.build_const_string("")?,
                "memory_dump" => {
                    self.build_memory_dump(last_state, unexpected_event, field_llvm_ty)?
                }
                "panic_payload" => self.build_panic_payload(unexpected_event, field_llvm_ty)?,
                _ => self.build_default_value(&field.ty, field_llvm_ty)?,
            };
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("trace gep: {}", e)))?;
            self.build_store(gep, val)?;
        }
        self.build_load(BasicTypeEnum::StructType(sty), alloca, "trace_val")
    }

    /// Build the `MemoryDump` sub-record matching `make_fault_value`:
    /// `fields="from_state=<S>;event=<E>"`, `count=2`.
    fn build_memory_dump(
        &self,
        last_state: &str,
        unexpected_event: &str,
        llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return self.build_default_record("MemoryDump");
        };
        let fields = self.record_fields_of_type("MemoryDump")?;
        let alloca = self.build_alloca(sty, "memory_dump")?;
        let dump = format!("from_state={};event={}", last_state, unexpected_event);
        for (i, field) in fields.iter().enumerate() {
            let field_llvm_ty = sty
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("memdump field {} type", i)))?;
            let val = match field.name.as_str() {
                "fields" => self.build_const_string(&dump)?,
                "count" => self.build_int_const(field_llvm_ty, 2),
                _ => self.build_default_value(&field.ty, field_llvm_ty)?,
            };
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("memdump gep: {}", e)))?;
            self.build_store(gep, val)?;
        }
        self.build_load(BasicTypeEnum::StructType(sty), alloca, "memdump_val")
    }

    /// Build the `PanicPayload` sub-record matching `make_fault_value`:
    /// `error_type=<E>`, `file=""`, `line=0`, `stack=""` (= snapshot).
    fn build_panic_payload(
        &self,
        unexpected_event: &str,
        llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return self.build_default_record("PanicPayload");
        };
        let fields = self.record_fields_of_type("PanicPayload")?;
        let alloca = self.build_alloca(sty, "panic_payload")?;
        for (i, field) in fields.iter().enumerate() {
            let field_llvm_ty = sty
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("panic field {} type", i)))?;
            let val = match field.name.as_str() {
                "error_type" => self.build_const_string(unexpected_event)?,
                "file" => self.build_const_string("")?,
                "line" => self.build_int_const(field_llvm_ty, 0),
                "stack" => self.build_const_string("")?,
                _ => self.build_default_value(&field.ty, field_llvm_ty)?,
            };
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("panic gep: {}", e)))?;
            self.build_store(gep, val)?;
        }
        self.build_load(BasicTypeEnum::StructType(sty), alloca, "panic_val")
    }

    /// Integer constant for a field's LLVM type (falls back to i64).
    fn build_int_const(&self, llvm_ty: BasicTypeEnum<'ctx>, v: u64) -> BasicValueEnum<'ctx> {
        match llvm_ty {
            BasicTypeEnum::IntType(it) => it.const_int(v, false).into(),
            _ => self.context.i64_type().const_int(v, false).into(),
        }
    }

    /// Build a default (zero) value for an arbitrary record type, recursing into
    /// nested records. Used for the `MemoryDump`/`PanicPayload` sub-records and
    /// the typed `error` payload. String fields default to "".
    fn build_default_record(&self, type_name: &str) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let llvm_ty = *self.type_llvm.get(type_name).ok_or_else(|| {
            CompileError::LlvmError(format!("type '{}' not registered", type_name))
        })?;
        let BasicTypeEnum::StructType(sty) = llvm_ty else {
            return Err(CompileError::LlvmError(format!(
                "'{}' is not a struct",
                type_name
            )));
        };
        let fields = self.record_fields_of_type(type_name)?;
        let alloca = self.build_alloca(sty, "default_rec")?;
        for (i, field) in fields.iter().enumerate() {
            let field_llvm_ty = sty
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("field {} type", i)))?;
            let val = self.build_default_value(&field.ty, field_llvm_ty)?;
            let gep = self
                .gep()
                .build_struct_gep(sty, alloca, i as u32, &field.name)
                .map_err(|e| CompileError::LlvmError(format!("default gep: {}", e)))?;
            self.build_store(gep, val)?;
        }
        self.build_load(BasicTypeEnum::StructType(sty), alloca, "default_val")
    }

    /// Build a default value for a single field of the given surface type and
    /// LLVM type.
    fn build_default_value(
        &self,
        ty: &Type,
        llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match ty.unlocated() {
            Type::Name(n, _) => match n.as_str() {
                "string" => self.build_const_string(""),
                "bool" => Ok(self.context.bool_type().const_zero().into()),
                "f32" | "f64" => match llvm_ty {
                    BasicTypeEnum::FloatType(ft) => Ok(ft.const_zero().into()),
                    _ => Ok(self.context.f64_type().const_zero().into()),
                },
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "int" | "unit" => {
                    match llvm_ty {
                        BasicTypeEnum::IntType(it) => Ok(it.const_zero().into()),
                        _ => Ok(self.context.i64_type().const_zero().into()),
                    }
                }
                _ if self.is_record_type_name(n) => self.build_default_record(n),
                _ => Ok(self.zero_of_llvm(llvm_ty)?),
            },
            // Option/Result/List/Tuple/pointer-like — a zero of the LLVM type is
            // a safe payload (recovery arms bind these with `_`).
            _ => Ok(self.zero_of_llvm(llvm_ty)?),
        }
    }

    /// Zero value for an arbitrary LLVM type (fallback for compound fields).
    fn zero_of_llvm(
        &self,
        llvm_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match llvm_ty {
            BasicTypeEnum::IntType(it) => Ok(it.const_zero().into()),
            BasicTypeEnum::FloatType(ft) => Ok(ft.const_zero().into()),
            BasicTypeEnum::PointerType(pt) => Ok(pt.const_null().into()),
            BasicTypeEnum::StructType(st) => Ok(st.const_zero().into()),
            _ => Err(CompileError::LlvmError(
                "cannot build zero for fault field LLVM type".to_string(),
            )),
        }
    }

    /// Build a constant Mimi string struct `{ptr, i64}` from a Rust string.
    fn build_const_string(&self, s: &str) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let global = self
            .builder
            .build_global_string_ptr(s, "fault_str")
            .map_err(|e| CompileError::LlvmError(format!("fault string error: {}", e)))?;
        let len = self.context.i64_type().const_int(s.len() as u64, false);
        self.build_string_struct(global.as_pointer_value(), len)
    }

    /// Field list of a record type from `type_defs`.
    fn record_fields_of_type(
        &self,
        type_name: &str,
    ) -> Result<Vec<crate::ast::Field>, CompileError> {
        let td = self
            .type_defs
            .get(type_name)
            .ok_or_else(|| CompileError::LlvmError(format!("no type_def for '{}'", type_name)))?;
        match &td.kind {
            TypeDefKind::Record(fields) => Ok(fields.clone()),
            _ => Err(CompileError::LlvmError(format!(
                "'{}' is not a record type",
                type_name
            ))),
        }
    }

    /// Whether a type name denotes a record (for default-value recursion).
    fn is_record_type_name(&self, type_name: &str) -> bool {
        matches!(
            self.type_defs.get(type_name).map(|td| &td.kind),
            Some(TypeDefKind::Record(_))
        )
    }
}
