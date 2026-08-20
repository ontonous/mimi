use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_literal_expr(
        &mut self,
        lit: &Lit,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match lit {
            Lit::Int(n) => Ok(self.context.i64_type().const_int(*n as u64, true).into()),
            Lit::Float(f) => Ok(self.context.f64_type().const_float(*f).into()),
            Lit::Bool(b) => Ok(self.context.bool_type().const_int(*b as u64, false).into()),
            Lit::Unit => Ok(self.context.i64_type().const_int(0, false).into()),
            Lit::String(s) => {
                // String ABI: a string value is the canonical {ptr, i64} struct
                // (ptr to null-terminated data, byte length) — the same layout
                // used by concatenation, `to_string`, f-strings, etc. (see
                // CodeGenerator::build_string_struct). Returning a bare i8* here
                // contradicted that ABI and corrupted any downstream string op.
                let global = self
                    .builder
                    .build_global_string_ptr(s, "str")
                    .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                let ptr = global.as_pointer_value();
                let len = self.context.i64_type().const_int(s.len() as u64, false);
                self.build_string_struct(ptr, len)
            }
            Lit::FString(parts) => Ok(self.compile_fstring(parts, vars)?),
        }
    }

    pub(in crate::codegen) fn compile_fstring(
        &mut self,
        parts: &[crate::ast::FStringPart],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let _i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        if parts.is_empty() {
            let global = self
                .builder
                .build_global_string_ptr("", "fstr_empty")
                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
            let ptr = global.as_pointer_value();
            let len = self.context.i64_type().const_int(0, false);
            return self.build_string_struct(ptr, len);
        }

        // Optimization: if all parts are text, return a single global string
        let all_text: Option<String> = parts
            .iter()
            .map(|p| match p {
                crate::ast::FStringPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        if let Some(text) = all_text {
            let global = self
                .builder
                .build_global_string_ptr(&text, "fstr_literal")
                .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
            let ptr = global.as_pointer_value();
            let len = self.context.i64_type().const_int(text.len() as u64, false);
            return self.build_string_struct(ptr, len);
        }

        // For f-strings with interpolation: dynamically compute buffer size, then fill.
        //
        // AUDIT FIX (full-audit-2026-08-05 §7, literal.rs:373-424): the legacy
        // assembly was strlen/strcpy/strcat-based — C-string semantics on
        // length-based data. A string part carrying an embedded NUL terminated
        // the composition at the NUL (VM concatenation is length-based,
        // interp/bytecode/compiler.rs:2568-2612 ConcatStr), and the final
        // length came from strlen(buf) instead of the tracked total. This
        // rewrite tracks (ptr, len) per part, allocates the exact total, and
        // copies with memcpy at tracked offsets so NUL bytes survive.
        // Float interpolation switched from snprintf "%f" (fixed 6 decimals,
        // diverges: 1.5 → "1.500000") to mimi_to_string_f64 — the same Rust
        // shortest round-trip Display the VM uses (Value::Float → `{}`).
        //
        // B3: Use snprintf instead of sprintf for buffer safety.
        // B4: allocations go through malloc_or_abort (no bare malloc).
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        // CG-C3: snprintf returns i32, not i8*.
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let i32_ty = self.context.i32_type();
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module
                .add_function("snprintf", ty, Some(inkwell::module::Linkage::External))
        });

        // Phase 1: Compile each part, tracking (ptr, runtime len) so phase 2
        // can copy length-based (no strcat/strlen over composed data).
        enum CompiledPart<'ctx> {
            Text(String),
            Interp {
                ptr: PointerValue<'ctx>,
                len: IntValue<'ctx>,
            },
        }
        let mut compiled_parts: Vec<CompiledPart<'ctx>> = Vec::new();
        // +1 for the trailing NUL handed to C-string consumers downstream.
        let mut total_size = i64_ty.const_int(1, false);
        for (i, part) in parts.iter().enumerate() {
            match part {
                crate::ast::FStringPart::Text(t) => {
                    total_size = self
                        .builder
                        .build_int_add(
                            total_size,
                            i64_ty.const_int(t.len() as u64, false),
                            &format!("fstr_text_sz_{}", i),
                        )
                        .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                    compiled_parts.push(CompiledPart::Text(t.clone()));
                }
                crate::ast::FStringPart::Interp(expr) => {
                    let val = self.compile_expr(expr, vars)?;
                    // Bool interps must render "true"/"false", not "%ld" 1/0.
                    if let Some(bool_str) = self.maybe_bool_to_string(expr, val) {
                        let ptr = match bool_str {
                            BasicValueEnum::PointerValue(pv) => pv,
                            _ => {
                                return Err(CompileError::Generic(
                                    "fstring bool: expected pointer".into(),
                                ))
                            }
                        };
                        let len = self
                            .build_call(
                                strlen_fn,
                                &[BasicMetadataValueEnum::PointerValue(ptr)],
                                &format!("fstr_bool_strlen_{}", i),
                            )?
                            .try_as_basic_value_opt()
                            .ok_or("strlen returned void")?
                            .into_int_value();
                        total_size = self
                            .builder
                            .build_int_add(total_size, len, &format!("fstr_bool_sz_{}", i))
                            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                        compiled_parts.push(CompiledPart::Interp { ptr, len });
                        continue;
                    }
                    match val {
                        BasicValueEnum::IntValue(iv) => {
                            let bw = iv.get_type().get_bit_width();
                            // i1 values are bools even when var_type_names misses
                            // the binding (e.g. `let b = true` without explicit type).
                            if bw == 1 {
                                let true_g = self
                                    .builder
                                    .build_global_string_ptr("true", &format!("fstr_true_{}", i))
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("string error: {}", e))
                                    })?
                                    .as_pointer_value();
                                let false_g = self
                                    .builder
                                    .build_global_string_ptr("false", &format!("fstr_false_{}", i))
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("string error: {}", e))
                                    })?
                                    .as_pointer_value();
                                let zero = iv.get_type().const_int(0, false);
                                let cond = self
                                    .builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::NE,
                                        iv,
                                        zero,
                                        &format!("fstr_bool_nz_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("cmp error: {}", e))
                                    })?;
                                let ptr = self
                                    .builder
                                    .build_select(
                                        cond,
                                        BasicValueEnum::PointerValue(true_g),
                                        BasicValueEnum::PointerValue(false_g),
                                        &format!("fstr_bool_sel_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("select error: {}", e))
                                    })?
                                    .into_pointer_value();
                                let len = self
                                    .build_call(
                                        strlen_fn,
                                        &[BasicMetadataValueEnum::PointerValue(ptr)],
                                        &format!("fstr_i1_strlen_{}", i),
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("strlen returned void")?
                                    .into_int_value();
                                total_size = self
                                    .builder
                                    .build_int_add(total_size, len, &format!("fstr_i1_sz_{}", i))
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("add error: {}", e))
                                    })?;
                                compiled_parts.push(CompiledPart::Interp { ptr, len });
                                continue;
                            }
                            let ext_iv = if bw < 64 {
                                self.builder
                                    .build_int_s_extend(
                                        iv,
                                        self.context.i64_type(),
                                        &format!("fstr_ext_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("sext error: {}", e))
                                    })?
                            } else {
                                iv
                            };
                            let temp_buf = self.malloc_or_abort(
                                i64_ty.const_int(32, false),
                                &format!("fstr_temp_{}", i),
                            )?;
                            self.register_heap_alloc(temp_buf);
                            let fmt = self
                                .builder
                                .build_global_string_ptr("%ld", &format!("fstr_fmt_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("string error: {}", e))
                                })?;
                            self.build_call(
                                snprintf_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(temp_buf),
                                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(32, false)),
                                    BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                    BasicMetadataValueEnum::IntValue(ext_iv),
                                ],
                                &format!("fstr_snprintf_{}", i),
                            )?;
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(temp_buf)],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::Interp { ptr: temp_buf, len });
                        }
                        BasicValueEnum::FloatValue(fv) => {
                            // MEM-C1 note superseded: see AUDIT FIX below (float path now
                            // uses mimi_to_string_f64; no fixed-size %f buffer needed).
                            // AUDIT FIX (full-audit-2026-08-05 §7): snprintf "%f"
                            // diverges from the VM, which stringifies floats via
                            // Rust shortest round-trip Display (Value::Float →
                            // write!("{}", v); 1.5 → "1.5", not "1.500000").
                            // mimi_to_string_f64 is the same runtime Display the
                            // println emitter uses (builtins/io.rs); it returns a
                            // NUL-free heap C string, so strlen is safe here.
                            let to_f64_fn = self.get_runtime_fn("mimi_to_string_f64")?;
                            let float_ptr = self
                                .build_call(
                                    to_f64_fn,
                                    &[BasicMetadataValueEnum::FloatValue(fv)],
                                    &format!("fstr_f64_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("mimi_to_string_f64 returned void")?
                                .into_pointer_value();
                            // Heap-owned by this f-string evaluation; freed at
                            // scope exit (allocator-compatible: mimi_alloc is
                            // libc::malloc in release builds).
                            self.register_heap_alloc(float_ptr);
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(float_ptr)],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::Interp {
                                ptr: float_ptr,
                                len,
                            });
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            // Raw C-string value: length only recoverable via
                            // strlen (length-carrying strings travel as
                            // StructValue below, where embedded NULs survive).
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(pv)],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::Interp { ptr: pv, len });
                        }
                        BasicValueEnum::StructValue(sv) => {
                            // String struct {i8*, i64} — use the authoritative len
                            // field (NOT strlen), so embedded NUL bytes survive
                            // the composition exactly like the VM's ConcatStr.
                            let sv_fields = sv.get_type().get_field_types();
                            let is_string_shape = matches!(
                                sv_fields.as_slice(),
                                [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(t)]
                                    if t.get_bit_width() == 64
                            );
                            if !is_string_shape {
                                // Non-string struct (e.g. a list) — its fields are
                                // not {ptr, len}; fall through to the placeholder
                                // instead of misreading them.
                                let unknown = self
                                    .builder
                                    .build_global_string_ptr(
                                        "<unsupported>",
                                        &format!("fstr_unsup_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("string error: {}", e))
                                    })?;
                                let unknown_len = self
                                    .build_call(
                                        strlen_fn,
                                        &[BasicMetadataValueEnum::PointerValue(
                                            unknown.as_pointer_value(),
                                        )],
                                        &format!("fstr_strlen_{}", i),
                                    )?
                                    .try_as_basic_value_opt()
                                    .ok_or("strlen returned void")?
                                    .into_int_value();
                                total_size = self
                                    .builder
                                    .build_int_add(
                                        total_size,
                                        unknown_len,
                                        &format!("fstr_isz_{}", i),
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("add error: {}", e))
                                    })?;
                                compiled_parts.push(CompiledPart::Interp {
                                    ptr: unknown.as_pointer_value(),
                                    len: unknown_len,
                                });
                                continue;
                            }
                            let data_ptr = self
                                .build_extract_value(sv.into(), 0, "fstr_str_data")?
                                .into_pointer_value();
                            let len = self
                                .build_extract_value(sv.into(), 1, "fstr_str_len")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::Interp { ptr: data_ptr, len });
                        }
                        _ => {
                            let unknown = self
                                .builder
                                .build_global_string_ptr(
                                    "<unsupported>",
                                    &format!("fstr_unsup_{}", i),
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("string error: {}", e))
                                })?;
                            let len = self
                                .build_call(
                                    strlen_fn,
                                    &[BasicMetadataValueEnum::PointerValue(
                                        unknown.as_pointer_value(),
                                    )],
                                    &format!("fstr_strlen_{}", i),
                                )?
                                .try_as_basic_value_opt()
                                .ok_or("strlen returned void")?
                                .into_int_value();
                            total_size = self
                                .builder
                                .build_int_add(total_size, len, &format!("fstr_isz_{}", i))
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("add error: {}", e))
                                })?;
                            compiled_parts.push(CompiledPart::Interp {
                                ptr: unknown.as_pointer_value(),
                                len,
                            });
                        }
                    }
                }
            }
        }

        // Phase 2: allocate the exact-size buffer and fill via memcpy at
        // tracked offsets (AUDIT FIX — no strcpy/strcat/strlen over composed
        // data, so embedded NUL bytes survive; VM parity).
        let buf = self.malloc_or_abort(total_size, "fstr_buf")?;
        self.register_heap_alloc(buf);
        let memcpy_fn = self.get_runtime_fn("memcpy")?;
        let i8_ty = self.context.i8_type();
        let mut offset = i64_ty.const_int(0, false);
        for (i, part) in compiled_parts.iter().enumerate() {
            let (src_ptr, part_len): (PointerValue<'ctx>, IntValue<'ctx>) = match part {
                CompiledPart::Text(t) => {
                    if t.is_empty() {
                        continue;
                    }
                    let global = self
                        .builder
                        .build_global_string_ptr(t, &format!("fstr_part_{}", i))
                        .map_err(|e| CompileError::LlvmError(format!("string error: {}", e)))?;
                    // Exact byte count: the global carries a trailing NUL that
                    // must NOT be copied into the composition.
                    (
                        global.as_pointer_value(),
                        i64_ty.const_int(t.len() as u64, false),
                    )
                }
                CompiledPart::Interp { ptr, len } => (*ptr, *len),
            };
            let dst = self.build_in_bounds_gep(
                BasicTypeEnum::IntType(i8_ty),
                buf,
                &[offset],
                &format!("fstr_dst_{}", i),
            )?;
            // SAFETY: `dst` is buf + offset with offset + part_len <= total_size
            // (total_size accumulated every part's exact length plus the
            // terminator byte); `src_ptr` is valid for `part_len` bytes by
            // construction in phase 1 (globals are t.len() bytes long, temp
            // buffers and runtime strings carry their measured length).
            self.builder
                .build_call(
                    memcpy_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(dst),
                        BasicMetadataValueEnum::PointerValue(src_ptr),
                        BasicMetadataValueEnum::IntValue(part_len),
                    ],
                    &format!("fstr_memcpy_{}", i),
                )
                .map_err(|e| CompileError::LlvmError(format!("memcpy: {}", e)))?;
            offset = self
                .builder
                .build_int_add(offset, part_len, &format!("fstr_off_{}", i))
                .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        }

        // Phase 3: trailing NUL for C-string consumers downstream (puts, %s),
        // then wrap into the canonical {i8*, i64} struct. The len field is the
        // TRACKED total — never strlen(buf) — so NUL bytes inside the string
        // do not truncate the value.
        let nul_gep = self.build_in_bounds_gep(
            BasicTypeEnum::IntType(i8_ty),
            buf,
            &[offset],
            "fstr_nul_gep",
        )?;
        self.build_store(nul_gep, i8_ty.const_int(0, false))?;
        self.build_string_struct(buf, offset)
    }
}
