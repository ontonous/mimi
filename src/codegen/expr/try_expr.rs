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
        // ? operator: compile inner expr as Result/Option/enum,
        // check discriminant, extract T on Ok/Some, exit on Err/None
        let result_val = self.compile_expr(inner, vars)?;

        let i64_ty = self.context.i64_type();
        let function = self.current_function().ok_or_else(|| "codegen: no current function for try".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "try_ok");
        let err_bb = self.context.append_basic_block(function, "try_err");

        // Determine the correct struct type for this Result/Option/enum value.
        // Built-in Result<T,E> uses {i1, T, i64} (3 fields),
        // built-in Option<T> uses {i1, T} (2 fields),
        // user-defined enums use {i32, T} (2 fields, from register_type_def).
        let inner_type_name = match inner {
            Expr::Ident(name) => self.var_type_names.get(name).cloned(),
            Expr::Call(callee, _) => {
                if let Expr::Ident(fname) = callee.as_ref() {
                    self.func_defs.get(fname)
                        .and_then(|f| f.ret.as_ref())
                        .map(|ret_ty| crate::core::fmt_type(ret_ty))
                } else {
                    None
                }
            }
            _ => None,
        };
        let is_user_enum = inner_type_name.as_ref()
            .map(|tn| self.type_defs.contains_key(tn))
            .unwrap_or(false);
        let is_result = inner_type_name.as_ref()
            .map(|tn| tn.starts_with("Result<") || tn == "Result")
            .unwrap_or(false);

        // Build the appropriate struct type for loading
        let struct_ty_to_use = if is_user_enum {
            // User-defined enum: {i32 tag, i64 payload} — all payloads stored as i64
            BasicTypeEnum::StructType(self.context.struct_type(&[
                BasicTypeEnum::IntType(self.context.i32_type()),
                BasicTypeEnum::IntType(i64_ty),
            ], false))
        } else if is_result {
            // Built-in Result<T,E>: {i1 disc, T ok, i64 err}
            BasicTypeEnum::StructType(self.context.struct_type(&[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ], false))
        } else {
            // Built-in Option<T>: {i1 disc, T payload}
            BasicTypeEnum::StructType(self.context.struct_type(&[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::IntType(i64_ty),
            ], false))
        };

        // Convert to struct value for uniform extract_value handling
        let struct_val = match result_val {
            BasicValueEnum::PointerValue(pv) => {
                self.builder.build_load(
                    struct_ty_to_use, pv, "try_load"
                ).map_err(|e| CompileError::LlvmError(format!("try load error: {}", e)))?
            }
            BasicValueEnum::StructValue(sv) => BasicValueEnum::StructValue(sv),
            _ => return Err("? operator requires a Result/Option type (struct pointer or value)".into()),
        };

        let sv = struct_val.into_struct_value();
        let disc = self.builder.build_extract_value(sv, 0, "discriminant")
            .map_err(|e| CompileError::LlvmError(format!("extract_value error: {}", e)))?;
        let payload = self.builder.build_extract_value(sv, 1, "payload")
            .map_err(|e| CompileError::LlvmError(format!("extract_value error: {}", e)))?;
        let err_val = if is_result {
            self.builder.build_extract_value(sv, 2, "err_val")
                .map_err(|e| CompileError::LlvmError(format!("extract_value error: {}", e)))?
        } else {
            payload
        };

        // Compare discriminant != 0 (Ok/Some = 1, Err/None = 0)
        let disc_int = disc.into_int_value();
        let is_err = if is_user_enum {
            let zero = self.context.i32_type().const_int(0, false);
            self.builder.build_int_compare(
                inkwell::IntPredicate::EQ, disc_int, zero, "is_err"
            ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        } else {
            let zero = self.context.bool_type().const_int(0, false);
            self.builder.build_int_compare(
                inkwell::IntPredicate::EQ, disc_int, zero, "is_err"
            ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        };

        self.builder.build_conditional_branch(is_err, err_bb, ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // Err path: run compensations, print error message, exit(1)
        self.builder.position_at_end(err_bb);
        let mut comp_vars = vars.clone();
        self.compile_compensations(&mut comp_vars).map_err(|e| CompileError::Generic(e.to_string()))?;

        // Determine if the error type is string (Result<T, string>) to display
        // the actual error message instead of a numeric pointer value.
        let is_string_err = is_result && inner_type_name.as_ref()
            .map(|tn| {
                tn.rsplitn(2, ',').next()
                    .map(|last| last.trim_end_matches('>').trim() == "string")
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        if is_string_err {
            // String error: the i64 slot contains a ptrtoint-encoded pointer
            // to a heap-allocated string struct {i8*, i64}.
            // Decode it back and call mimi_try_exit_str(ptr, len).
            let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
            let string_struct_ty = self.context.struct_type(&[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ], false);
            let err_ptr = self.builder.build_int_to_ptr(
                err_val.into_int_value(), string_struct_ty.ptr_type(inkwell::AddressSpace::default()),
                "err_str_ptr",
            ).map_err(|e| CompileError::LlvmError(format!("inttoptr error: {}", e)))?;
            let str_ptr_ptr = self.builder.build_struct_gep(
                BasicTypeEnum::StructType(string_struct_ty), err_ptr, 0, "str_ptr_gep",
            ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let str_ptr = self.builder.build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty), str_ptr_ptr, "str_ptr",
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
            let str_len_ptr = self.builder.build_struct_gep(
                BasicTypeEnum::StructType(string_struct_ty), err_ptr, 1, "str_len_gep",
            ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let str_len = self.builder.build_load(
                BasicTypeEnum::IntType(i64_ty), str_len_ptr, "str_len",
            ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
            let try_exit_str_fn = self.module.get_function("mimi_try_exit_str")
                .ok_or("mimi_try_exit_str not declared")?;
            self.builder.build_call(try_exit_str_fn, &[
                BasicMetadataValueEnum::PointerValue(str_ptr),
                BasicMetadataValueEnum::IntValue(str_len),
            ], "try_exit_str")
                .map_err(|e| CompileError::LlvmError(format!("try_exit_str error: {}", e)))?;
        } else {
            // Numeric error: pass the i64 value directly to mimi_try_exit
            let try_exit_fn = self.module.get_function("mimi_try_exit")
                .ok_or("mimi_try_exit not declared")?;
            let err_int = match err_val {
                BasicValueEnum::IntValue(iv) => iv,
                _ => i64_ty.const_zero(),
            };
            self.builder.build_call(try_exit_fn, &[
                BasicMetadataValueEnum::IntValue(err_int),
            ], "try_exit")
                .map_err(|e| CompileError::LlvmError(format!("try_exit error: {}", e)))?;
        }
        let unreachable = self.context.append_basic_block(function, "unreachable");
        self.builder.build_unconditional_branch(unreachable)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(unreachable);
        self.builder.build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable terminator: {}", e)))?;

        self.builder.position_at_end(ok_bb);
        Ok(payload)
    }

}
