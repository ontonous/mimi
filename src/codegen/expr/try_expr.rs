use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_try_expr(
        &mut self,
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // FLOW-TURN-001: `?` inside a transition with `fails E` lowers to
        // Rejected: return Err((source, error)) instead of process exit.
        if self.in_fails_transition {
            return self.compile_try_rejected(inner, vars);
        }
        // ? operator: compile inner expr as Result/Option/enum,
        // check discriminant, extract T on Ok/Some, exit on Err/None
        let result_val = self.compile_expr(inner, vars)?;

        let i64_ty = self.context.i64_type();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for try".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "try_ok");
        let err_bb = self.context.append_basic_block(function, "try_err");

        // Determine the correct struct type for this Result/Option/enum value.
        // Built-in Result<T,E> uses {i1, T, i64} (3 fields),
        // built-in Option<T> uses {i1, T} (2 fields),
        // user-defined enums use {i32, T} (2 fields, from register_type_def).
        // Match on the unlocated form: parser-produced subexpressions may be
        // wrapped in Expr::Located, and bare-pattern probes silently miss them.
        let inner_type_name = match inner.unlocated() {
            Expr::Ident(name) => self.var_type_names.get(name).cloned(),
            Expr::Call(callee, _) => {
                if let Expr::Ident(fname) = callee.unlocated() {
                    self.func_defs
                        .get(fname)
                        .and_then(|f| f.ret.as_ref())
                        .map(crate::core::fmt_type)
                } else {
                    None
                }
            }
            // full-audit 2026-08-05 §7: `?` on a record FIELD (e.g. `self.res?`)
            // previously fell through with no type probe, so a 3-field Result
            // defaulted to the 2-field Option layout and the Err path fed the
            // OK slot to mimi_try_exit. Recover the declared field type from
            // the owner record so Result-ness (and string errors) resolve.
            Expr::Field(obj, field_name) => {
                let obj_type = self.infer_object_type(obj, vars);
                let base_name = match obj_type.find('<') {
                    Some(pos) => &obj_type[..pos],
                    None => obj_type.as_str(),
                };
                self.type_defs.get(base_name).and_then(|td| match &td.kind {
                    TypeDefKind::Record(fields) => fields
                        .iter()
                        .find(|f| f.name == *field_name)
                        .map(|f| crate::core::fmt_type(&f.ty)),
                    _ => None,
                })
            }
            _ => None,
        };
        let mut is_user_enum = inner_type_name
            .as_ref()
            .map(|tn| self.type_defs.contains_key(tn))
            .unwrap_or(false);
        let mut is_result = inner_type_name
            .as_ref()
            .map(|tn| tn.starts_with("Result<") || tn == "Result")
            .unwrap_or(false);

        // Shape fallback (mirrors compile_try_rejected's field-count probe):
        // when the value is a struct whose layout contradicts or refines the
        // name probe, the LLVM shape is authoritative. 3+ fields → Result
        // {disc, ok, err}; 2 fields with an i32 discriminant → user enum
        // {tag, payload}; 2 fields with an i1 discriminant → Option. This
        // fixes `?` on expressions the name probe cannot see (fields, index
        // results, aliased types) — previously they loaded/exited through the
        // Option layout with a wrong err slot.
        if let BasicTypeEnum::StructType(st) = result_val.get_type() {
            let fields = st.get_field_types();
            if fields.len() >= 3 {
                is_result = true;
                is_user_enum = false;
            } else if fields.len() == 2 {
                if let Some(BasicTypeEnum::IntType(disc)) = fields.first() {
                    if disc.get_bit_width() == 32 {
                        is_user_enum = true;
                        is_result = false;
                    }
                }
            }
        }

        // Build the appropriate struct type for loading
        let struct_ty_to_use = if is_user_enum {
            // User-defined enum: {i32 tag, i64 payload} — all payloads stored as i64
            BasicTypeEnum::StructType(self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(self.context.i32_type()),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            ))
        } else if is_result {
            // Built-in Result<T,E>: {i1 disc, T ok, i64 err}
            BasicTypeEnum::StructType(self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(self.context.bool_type()),
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            ))
        } else {
            // Built-in Option<T>: {i1 disc, T payload}
            BasicTypeEnum::StructType(self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(self.context.bool_type()),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            ))
        };

        // Convert to struct value for uniform extract_value handling
        let struct_val = match result_val {
            BasicValueEnum::PointerValue(pv) => self
                .builder
                .build_load(struct_ty_to_use, pv, "try_load")
                .map_err(|e| CompileError::LlvmError(format!("try load error: {}", e)))?,
            BasicValueEnum::StructValue(sv) => BasicValueEnum::StructValue(sv),
            _ => {
                return Err(
                    "? operator requires a Result/Option type (struct pointer or value)".into(),
                )
            }
        };

        let sv = struct_val.into_struct_value();
        let disc = self
            .builder
            .build_extract_value(sv, 0, "discriminant")
            .map_err(|e| CompileError::LlvmError(format!("extract_value error: {}", e)))?;
        let payload = self
            .builder
            .build_extract_value(sv, 1, "payload")
            .map_err(|e| CompileError::LlvmError(format!("extract_value error: {}", e)))?;
        let err_val = if is_result {
            self.builder
                .build_extract_value(sv, 2, "err_val")
                .map_err(|e| CompileError::LlvmError(format!("extract_value error: {}", e)))?
        } else {
            payload
        };

        // Compare discriminant != 0 (Ok/Some = 1, Err/None = 0).
        // Use the actual discriminant type for the zero constant to avoid
        // i32-vs-i1 type mismatch in the icmp instruction. User-defined enums
        // have i32 tags; built-in Result/Option have i1. Mismatched types
        // produce invalid IR that O0 tolerates but O1 miscompiles → SIGSEGV.
        let disc_int = disc.into_int_value();
        let zero = disc_int.get_type().const_int(0, false);
        let is_err = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, disc_int, zero, "is_err")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;

        self.builder
            .build_conditional_branch(is_err, err_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // Err path: run compensations, print error message, exit(1)
        self.builder.position_at_end(err_bb);
        let mut comp_vars = vars.clone();
        self.compile_compensations(&mut comp_vars)
            .map_err(|e| CompileError::Generic(e.to_string()))?;

        // Determine if the error type is string (Result<T, string>) to display
        // the actual error message instead of a numeric pointer value.
        let is_string_err = is_result
            && inner_type_name
                .as_ref()
                .map(|tn| {
                    tn.rsplit(',')
                        .next()
                        .map(|last| last.trim_end_matches('>').trim() == "string")
                        .unwrap_or(false)
                })
                .unwrap_or(false);

        if is_string_err {
            // String error: the i64 slot contains a ptrtoint-encoded pointer
            // to a heap-allocated string struct {i8*, i64}.
            // Decode it back and call mimi_try_exit_str(ptr, len).
            let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let string_struct_ty = self.context.struct_type(
                &[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            );
            let err_ptr = self
                .builder
                .build_int_to_ptr(
                    err_val.into_int_value(),
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "err_str_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("inttoptr error: {}", e)))?;
            let str_ptr_ptr = self
                .gep()
                .build_struct_gep(
                    BasicTypeEnum::StructType(string_struct_ty),
                    err_ptr,
                    0,
                    "str_ptr_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let str_ptr = self
                .builder
                .build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    str_ptr_ptr,
                    "str_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                .into_pointer_value();
            let str_len_ptr = self
                .gep()
                .build_struct_gep(
                    BasicTypeEnum::StructType(string_struct_ty),
                    err_ptr,
                    1,
                    "str_len_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let str_len = self
                .builder
                .build_load(BasicTypeEnum::IntType(i64_ty), str_len_ptr, "str_len")
                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                .into_int_value();
            let try_exit_str_fn = self
                .module
                .get_function("mimi_try_exit_str")
                .ok_or("mimi_try_exit_str not declared")?;
            self.builder
                .build_call(
                    try_exit_str_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(str_ptr),
                        BasicMetadataValueEnum::IntValue(str_len),
                    ],
                    "try_exit_str",
                )
                .map_err(|e| CompileError::LlvmError(format!("try_exit_str error: {}", e)))?;
        } else {
            // Numeric error: pass the i64 value directly to mimi_try_exit
            let try_exit_fn = self
                .module
                .get_function("mimi_try_exit")
                .ok_or("mimi_try_exit not declared")?;
            let err_int = match err_val {
                BasicValueEnum::IntValue(iv) => iv,
                _ => i64_ty.const_zero(),
            };
            self.builder
                .build_call(
                    try_exit_fn,
                    &[BasicMetadataValueEnum::IntValue(err_int)],
                    "try_exit",
                )
                .map_err(|e| CompileError::LlvmError(format!("try_exit error: {}", e)))?;
        }
        let unreachable = self.context.append_basic_block(function, "unreachable");
        self.builder
            .build_unconditional_branch(unreachable)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(unreachable);
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable terminator: {}", e)))?;

        self.builder.position_at_end(ok_bb);
        Ok(payload)
    }

    /// FLOW-TURN-001: Rejected path for `?` inside a `fails E` transition.
    /// On Err: construct `Err((source, error))` and return it from the transition.
    /// On Ok: extract the payload and continue normally.
    fn compile_try_rejected(
        &mut self,
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let result_val = self.compile_expr(inner, vars)?;

        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for try_rejected".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "try_rej_ok");
        let err_bb = self.context.append_basic_block(function, "try_rej_err");

        // Determine struct type for the inner Result/Option.
        let inner_type_name = match inner {
            Expr::Ident(name) => self.var_type_names.get(name).cloned(),
            Expr::Call(callee, _) => {
                if let Expr::Ident(fname) = callee.unlocated() {
                    self.func_defs
                        .get(fname)
                        .and_then(|f| f.ret.as_ref())
                        .map(crate::core::fmt_type)
                } else {
                    None
                }
            }
            _ => None,
        };
        // P3-4 fix: determine Result vs Option by LLVM struct field count
        // (3 = Result{disc,ok,err}, 2 = Option{disc,payload}) instead of
        // string-probing the type name. String probing breaks on type aliases
        // (e.g., `type MyRes = Result<i32, string>` → name is "MyRes").
        let is_result = match result_val.get_type() {
            BasicTypeEnum::StructType(st) => st.count_fields() >= 3,
            _ => {
                // Fallback: string probe for non-struct values.
                inner_type_name
                    .as_ref()
                    .map(|tn| tn.starts_with("Result<") || tn == "Result")
                    .unwrap_or(false)
            }
        };

        let struct_ty_to_use = if is_result {
            BasicTypeEnum::StructType(self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(bool_ty),
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            ))
        } else {
            BasicTypeEnum::StructType(self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(bool_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            ))
        };

        let struct_val = match result_val {
            BasicValueEnum::PointerValue(pv) => self
                .builder
                .build_load(struct_ty_to_use, pv, "try_rej_load")
                .map_err(|e| CompileError::LlvmError(format!("try_rej load: {}", e)))?,
            BasicValueEnum::StructValue(sv) => BasicValueEnum::StructValue(sv),
            _ => return Err("? operator in fails transition requires a Result/Option type".into()),
        };

        let sv = struct_val.into_struct_value();
        let disc = self
            .builder
            .build_extract_value(sv, 0, "try_rej_disc")
            .map_err(|e| CompileError::LlvmError(format!("extract_value: {}", e)))?;
        let payload = self
            .builder
            .build_extract_value(sv, 1, "try_rej_payload")
            .map_err(|e| CompileError::LlvmError(format!("extract_value: {}", e)))?;
        let err_val = if is_result {
            self.builder
                .build_extract_value(sv, 2, "try_rej_err")
                .map_err(|e| CompileError::LlvmError(format!("extract_value: {}", e)))?
        } else {
            payload
        };

        // discriminant == 0 means Err/None.
        // Use the actual discriminant type (i32 for user enums, i1 for
        // built-in Result/Option) to avoid invalid IR type mismatch.
        let disc_int = disc.into_int_value();
        let zero = disc_int.get_type().const_int(0, false);
        let is_err = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, disc_int, zero, "try_rej_is_err")
            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;

        self.builder
            .build_conditional_branch(is_err, err_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch: {}", e)))?;

        // Err path: construct Err((source, error)) and return it.
        self.builder.position_at_end(err_bb);

        // Get source (self) from vars.
        let (self_ptr, self_ty) = vars.get("self").copied().ok_or_else(|| {
            CompileError::LlvmError("fails transition has no self in scope".into())
        })?;
        let source_val = self.build_load(self_ty, self_ptr, "try_rej_source")?;

        // For struct values, heap-allocate a copy so the pointer survives
        // function return. For int/pointer values, use directly.
        // All arms return `(IntValue, BasicTypeEnum)` for consistency.
        let source_ptr_i64: IntValue<'ctx> = match source_val {
            BasicValueEnum::StructValue(sv) => {
                let llvm_ty = sv.get_type();
                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(llvm_ty));
                let heap =
                    self.malloc_or_abort(i64_ty.const_int(size, false), "try_rej_src_heap")?;
                let typed = self
                    .builder
                    .build_pointer_cast(
                        heap,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "try_rej_src_typed",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("bitcast src: {e}")))?;
                self.build_store(typed, sv)?;
                self.build_ptr_to_int(heap, i64_ty, "try_rej_src_i64")?
            }
            BasicValueEnum::PointerValue(pv) => {
                self.build_ptr_to_int(pv, i64_ty, "try_rej_src_i64")?
            }
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw < 64 {
                    self.builder
                        .build_int_z_extend(iv, i64_ty, "try_rej_src_zext")
                        .map_err(|e| CompileError::LlvmError(format!("src zext: {e}")))?
                } else if bw > 64 {
                    self.builder
                        .build_int_truncate(iv, i64_ty, "try_rej_src_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("src trunc: {e}")))?
                } else {
                    iv
                }
            }
            _ => {
                return Err("try_rej: unsupported source value type".into());
            }
        };

        // For error value: struct values get heap-allocated; int values get
        // sign/zero-extended to i64; pointer values use ptrtoint.
        let err_ptr_i64: IntValue<'ctx> = match err_val {
            BasicValueEnum::StructValue(sv) => {
                let llvm_ty = sv.get_type();
                let size = self.llvm_type_size_bytes(BasicTypeEnum::StructType(llvm_ty));
                let heap =
                    self.malloc_or_abort(i64_ty.const_int(size, false), "try_rej_err_heap")?;
                let typed = self
                    .builder
                    .build_pointer_cast(
                        heap,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "try_rej_err_typed",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("bitcast err: {e}")))?;
                self.build_store(typed, sv)?;
                self.build_ptr_to_int(heap, i64_ty, "try_rej_err_i64")?
            }
            BasicValueEnum::PointerValue(pv) => {
                self.build_ptr_to_int(pv, i64_ty, "try_rej_err_i64")?
            }
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "try_rej_err_sext")
                        .map_err(|e| CompileError::LlvmError(format!("err sext: {e}")))?
                } else if bw > 64 {
                    self.builder
                        .build_int_truncate(iv, i64_ty, "try_rej_err_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("err trunc: {e}")))?
                } else {
                    iv
                }
            }
            _ => {
                return Err("try_rej: unsupported error value type".into());
            }
        };

        // Allocate a {ptr, ptr} tuple on the heap. Each field is a ptrtoint
        // to the heap-allocated struct (or the direct value for ints/ptrs).
        // The caller decodes by inttopping each field and loading the struct.
        let tuple_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let tuple_size_val = i64_ty.const_int(16, false);
        let tuple_heap_ptr = self.malloc_or_abort(tuple_size_val, "try_rej_tuple")?;
        let tuple_typed = self
            .builder
            .build_pointer_cast(
                tuple_heap_ptr,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "try_rej_tuple_cast",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast: {e}")))?;
        let src_gep = self
            .gep()
            .build_struct_gep(tuple_ty, tuple_typed, 0, "try_rej_slot_src")
            .map_err(|e| CompileError::LlvmError(format!("gep src: {e}")))?;
        self.build_store(src_gep, source_ptr_i64)?;
        let err_gep = self
            .gep()
            .build_struct_gep(tuple_ty, tuple_typed, 1, "try_rej_slot_err")
            .map_err(|e| CompileError::LlvmError(format!("gep err: {e}")))?;
        self.build_store(err_gep, err_ptr_i64)?;

        // Build outer Result struct with the FUNCTION'S actual return type
        // instead of a hardcoded {i1, i64, i64}. Flow transitions return
        // {i1, ToState, i64} where ToState is the target state struct (e.g.,
        // Paid {string, i32, string} = 40 bytes). Using the wrong struct type
        // causes the Err path to write only 24 bytes into a 56-byte return
        // slot — the err payload (i64 at offset 48) contains garbage → SIGSEGV.
        let tuple_ptr_i64 = self
            .builder
            .build_ptr_to_int(tuple_typed, i64_ty, "try_rej_tuple_i64")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {e}")))?;
        let result_struct_bte = function.get_type().get_return_type().ok_or_else(|| {
            CompileError::LlvmError("fails transition function has no return type".into())
        })?;
        if !matches!(result_struct_bte, BasicTypeEnum::StructType(_)) {
            return Err("fails transition return type is not a struct".into());
        }
        let result_llvm_st = match result_struct_bte {
            BasicTypeEnum::StructType(st) => st,
            _ => return Err("fails transition return type is not a struct".into()),
        };
        let result_struct_ty = BasicTypeEnum::StructType(result_llvm_st);
        let result_alloca = self.build_alloca(result_struct_ty, "try_rej_result")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(result_struct_ty, result_alloca, 0, "try_rej_res_disc")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(disc_gep, bool_ty.const_int(0, false))?; // Err
                                                                  // Zero-initialize field 1 (ok payload) using its actual LLVM type.
                                                                  // For a struct ok-payload, store const_zero; for int, store 0.
        let ok_field_ty = result_llvm_st
            .get_field_type_at_index(1)
            .ok_or_else(|| CompileError::LlvmError("result struct has no field index 1".into()))?;
        let ok_pad_gep = self
            .gep()
            .build_struct_gep(result_struct_ty, result_alloca, 1, "try_rej_res_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let ok_zero_val: BasicValueEnum<'ctx> = match ok_field_ty {
            BasicTypeEnum::IntType(it) => it.const_zero().into(),
            BasicTypeEnum::StructType(st) => st.const_zero().into(),
            BasicTypeEnum::PointerType(pt) => pt.const_zero().into(),
            BasicTypeEnum::ArrayType(at) => at.const_zero().into(),
            BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
            _ => i64_ty.const_zero().into(),
        };
        self.build_store(ok_pad_gep, ok_zero_val)?;
        let err_store_gep = self
            .gep()
            .build_struct_gep(result_struct_ty, result_alloca, 2, "try_rej_res_err")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(err_store_gep, tuple_ptr_i64)?;

        let rejected_val = self.build_load(result_struct_ty, result_alloca, "try_rej_val")?;

        // Return the Err((source, error)) from the transition function.
        self.emit_all_shared_releases()?;
        self.discard_shared_scope();
        self.flush_heap_scopes_to_boundary()?;
        self.pop_comp_scope();
        self.build_return(Some(&rejected_val))?;

        // Ok path: continue with the extracted payload.
        self.builder.position_at_end(ok_bb);
        Ok(payload)
    }
}
