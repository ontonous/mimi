use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
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
        let is_user_enum = inner_type_name
            .as_ref()
            .map(|tn| self.type_defs.contains_key(tn))
            .unwrap_or(false);
        let is_result = inner_type_name
            .as_ref()
            .map(|tn| tn.starts_with("Result<") || tn == "Result")
            .unwrap_or(false);

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

        // Compare discriminant != 0 (Ok/Some = 1, Err/None = 0)
        let disc_int = disc.into_int_value();
        let is_err = if is_user_enum {
            let zero = self.context.i32_type().const_int(0, false);
            self.builder
                .build_int_compare(inkwell::IntPredicate::EQ, disc_int, zero, "is_err")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        } else {
            let zero = self.context.bool_type().const_int(0, false);
            self.builder
                .build_int_compare(inkwell::IntPredicate::EQ, disc_int, zero, "is_err")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        };

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

        // discriminant == 0 means Err/None
        let disc_int = disc.into_int_value();
        let zero = bool_ty.const_int(0, false);
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

        // Build the error tuple (source, error) as a 2-element struct.
        let source_as_i64 = match source_val {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::PointerValue(pv) => self
                .builder
                .build_ptr_to_int(pv, i64_ty, "try_rej_src_i64")
                .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?,
            other => {
                // For struct values (records), store to alloca and ptrtoint.
                let alloca = self.build_alloca(other.get_type(), "try_rej_src_tmp")?;
                self.build_store(alloca, other)?;
                self.builder
                    .build_ptr_to_int(alloca, i64_ty, "try_rej_src_i64")
                    .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?
            }
        };
        let err_as_i64 = match err_val {
            BasicValueEnum::IntValue(iv) => iv,
            BasicValueEnum::PointerValue(pv) => self
                .builder
                .build_ptr_to_int(pv, i64_ty, "try_rej_err_i64")
                .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?,
            other => {
                let alloca = self.build_alloca(other.get_type(), "try_rej_err_tmp")?;
                self.build_store(alloca, other)?;
                self.builder
                    .build_ptr_to_int(alloca, i64_ty, "try_rej_err_i64")
                    .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?
            }
        };

        // P1-1 fix: allocate tuple struct {i64 source, i64 error} on HEAP.
        // Stack alloca would dangle after return — the Result struct's err
        // field stores ptrtoint(tuple), and the caller dereferences it.
        // Heap allocation survives the function return. Not registered with
        // heap_allocs: the caller owns the error payload (acceptable leak
        // on error paths; tuple is 16 bytes).
        let tuple_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        // Tuple is {i64, i64} = 16 bytes on all supported targets.
        let tuple_size_val = i64_ty.const_int(16, false);
        let tuple_heap_ptr = self.malloc_or_abort(tuple_size_val, "try_rej_tuple")?;
        let tuple_alloca = self
            .builder
            .build_pointer_cast(
                tuple_heap_ptr,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "try_rej_tuple_cast",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?;
        let src_gep = self
            .gep()
            .build_struct_gep(tuple_ty, tuple_alloca, 0, "try_rej_tuple_src")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(src_gep, source_as_i64)?;
        let err_gep = self
            .gep()
            .build_struct_gep(tuple_ty, tuple_alloca, 1, "try_rej_tuple_err")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(err_gep, err_as_i64)?;

        // Build outer Result struct: {i1 disc=0, i64 ok_pad=0, i64 err=ptr_to_tuple}
        let tuple_ptr_i64 = self
            .builder
            .build_ptr_to_int(tuple_alloca, i64_ty, "try_rej_tuple_i64")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?;
        let result_struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let result_alloca = self.build_alloca(result_struct_ty, "try_rej_result")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(result_struct_ty, result_alloca, 0, "try_rej_res_disc")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(disc_gep, bool_ty.const_int(0, false))?; // Err
        let ok_pad_gep = self
            .gep()
            .build_struct_gep(result_struct_ty, result_alloca, 1, "try_rej_res_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(ok_pad_gep, i64_ty.const_int(0, false))?;
        let err_store_gep = self
            .gep()
            .build_struct_gep(result_struct_ty, result_alloca, 2, "try_rej_res_err")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(err_store_gep, tuple_ptr_i64)?;

        let rejected_val = self.build_load(result_struct_ty, result_alloca, "try_rej_val")?;

        // Return the Err((source, error)) from the transition function.
        self.emit_all_shared_releases()?;
        self.discard_shared_scope();
        self.free_heap_allocs()?;
        self.pop_comp_scope();
        self.build_return(Some(&rejected_val))?;

        // Ok path: continue with the extracted payload.
        self.builder.position_at_end(ok_bb);
        Ok(payload)
    }
}
