use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_constructor(
        &mut self,
        name: &str,
        compiled_args: Vec<BasicValueEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match name {
            "Ok" => {
                if compiled_args.len() != 1 {
                    return Err("Ok expects 1 argument".into());
                }
                let val = compiled_args[0];
                let bool_ty = self.context.bool_type();
                let i64_ty = self.context.i64_type();
                let disc = bool_ty.const_int(1, false);
                let inner_ty = val.get_type();
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    inner_ty,
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "ok_val")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let val_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "payload")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(val_gep, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let err_gep = self.builder.build_struct_gep(struct_ty, alloca, 2, "err_pad")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(err_gep, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                Ok(result)
            }
            "Some" => {
                if compiled_args.len() != 1 {
                    return Err("Some expects 1 argument".into());
                }
                let val = compiled_args[0];
                let bool_ty = self.context.bool_type();
                let disc = bool_ty.const_int(1, false);
                let inner_ty = val.get_type();
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    inner_ty,
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "some_val")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let val_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "payload")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(val_gep, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                Ok(result)
            }
            "Err" => {
                if compiled_args.len() != 1 {
                    return Err("Err expects 1 argument".into());
                }
                let val = compiled_args[0];
                let bool_ty = self.context.bool_type();
                let i64_ty = self.context.i64_type();
                let disc = bool_ty.const_int(0, false);
                let err_val: BasicValueEnum = match val {
                    BasicValueEnum::IntValue(iv) => {
                        let bit_width = iv.get_type().get_bit_width();
                        if bit_width < 64 {
                            self.builder.build_int_s_extend(iv, i64_ty, "err_sext")
                                .map_err(|e| CompileError::LlvmError(format!("int sign extend error: {}", e)))?
                                .into()
                        } else if bit_width > 64 {
                            self.builder.build_int_truncate(iv, i64_ty, "err_trunc")
                                .map_err(|e| CompileError::LlvmError(format!("int truncate error: {}", e)))?
                                .into()
                        } else {
                            iv.into()
                        }
                    }
                    BasicValueEnum::PointerValue(pv) => {
                        self.builder.build_ptr_to_int(pv, i64_ty, "err_to_i64")
                            .map_err(|e| CompileError::LlvmError(format!("ptrtoint error: {}", e)))?
                            .into()
                    }
                    BasicValueEnum::StructValue(sv) => {
                        // Custom enum values are {i32 tag, i64 payload}; Result stores
                        // only the tag in its error slot.
                        let tag = self.builder.build_extract_value(sv, 0, "enum_tag")
                            .map_err(|e| CompileError::LlvmError(format!("extract tag error: {}", e)))?
                            .into_int_value();
                        self.builder.build_int_cast(tag, i64_ty, "err_tag_ext")
                            .map_err(|e| CompileError::LlvmError(format!("int cast error: {}", e)))?
                            .into()
                    }
                    _ => return Err("Err: unsupported error value type".into()),
                };
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "err_val")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let ok_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "ok_pad")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(ok_gep, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let err_gep = self.builder.build_struct_gep(struct_ty, alloca, 2, "err_payload")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(err_gep, err_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                Ok(result)
            }
            "None" => {
                if compiled_args.len() != 0 {
                    return Err("None expects 0 arguments".into());
                }
                let bool_ty = self.context.bool_type();
                let i64_ty = self.context.i64_type();
                let disc = bool_ty.const_int(0, false);
                let struct_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(bool_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let alloca = self.builder.build_alloca(struct_ty, "none_val")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let disc_gep = self.builder.build_struct_gep(struct_ty, alloca, 0, "disc")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(disc_gep, disc)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let val_gep = self.builder.build_struct_gep(struct_ty, alloca, 1, "payload")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(val_gep, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let result = self.builder.build_load(struct_ty, alloca, "loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                Ok(result)
            }
            _ => Err(format!("unknown constructor '{}'", name).into()),
        }
    }
    pub(in crate::codegen) fn compile_variant_method(
        &mut self,
        obj: &Expr,
        method: &str,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let obj_val = self.compile_expr(obj, vars)?;
        let obj_type = self.infer_object_type(obj, vars);
        let is_result = obj_type.starts_with("Result<") || obj_type == "Result";
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let function = self.current_function().ok_or_else(|| "codegen: no current function for variant method".to_string())?;

        // Layout: Result<T,E> = {i1 disc, T ok, i64 err}, Option<T> = {i1 disc, T payload}
        let disc_idx: u32 = 0;
        let payload_idx: u32 = 1;
        let variant_sty = if is_result {
            self.context.struct_type(&[
                BasicTypeEnum::IntType(i1_ty),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ], false)
        } else {
            self.context.struct_type(&[
                BasicTypeEnum::IntType(i1_ty),
                BasicTypeEnum::IntType(i64_ty),
            ], false)
        };

        // Convert StructValue to PointerValue for uniform handling,
        // and determine the actual struct type for correct GEP offsets.
        // The actual struct layout depends on the payload type T,
        // e.g. {i1, i32, i64} for Result<i32,string> vs {i1, i64, i64} for Result<i64,string>.
        let (pv, actual_sty_enum) = match obj_val {
            BasicValueEnum::PointerValue(pv) => {
                let sty = if let Expr::Ident(name) = obj {
                    vars.get(name.as_str())
                        .map(|entry| entry.1)
                        .unwrap_or(BasicTypeEnum::StructType(variant_sty))
                } else {
                    BasicTypeEnum::StructType(variant_sty)
                };
                (pv, sty)
            }
            BasicValueEnum::StructValue(sv) => {
                let sty = sv.get_type();
                let sty_enum = BasicTypeEnum::StructType(sty);
                let tmp = self.builder.build_alloca(sty_enum, "variant_tmp")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(tmp, sv)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                (tmp, sty_enum)
            }
            _ => return Err(format!("variant method '{}' requires a struct pointer or value", method).into()),
        };
        let disc_gep = self.builder.build_struct_gep(
            actual_sty_enum, pv, disc_idx, "disc_gep"
        ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let disc = self.builder.build_load(BasicTypeEnum::IntType(i1_ty), disc_gep, "disc")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_int_value();
        let pay_gep = self.builder.build_struct_gep(
            actual_sty_enum, pv, payload_idx, "pay_gep"
        ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let payload = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), pay_gep, "payload")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;

        match method {
            "is_ok" | "is_some" => {
                let bool_val = self.builder.build_int_z_extend(disc, self.context.bool_type(), "is_ok_ext")
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
                Ok(BasicValueEnum::IntValue(bool_val))
            }
            "is_err" | "is_none" => {
                let not_disc = self.builder.build_not(disc, "is_err_not")
                    .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?;
                let bool_val = self.builder.build_int_z_extend(not_disc, self.context.bool_type(), "is_err_ext")
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
                Ok(BasicValueEnum::IntValue(bool_val))
            }
            "unwrap" | "expect" => {
                let ok_bb = self.context.append_basic_block(function, "unwrap_ok");
                let err_bb = self.context.append_basic_block(function, "unwrap_err");
                self.builder.build_conditional_branch(disc, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(err_bb);
                let trap_fn = self.module.get_function("mimi_try_exit")
                    .or_else(|| self.module.get_function("abort"))
                    .ok_or("abort not declared")?;
                self.builder.build_call(trap_fn, &[
                    BasicMetadataValueEnum::IntValue(payload.into_int_value()),
                ], "unwrap_trap").map_err(|e| CompileError::LlvmError(format!("trap error: {}", e)))?;
                let unreachable = self.context.append_basic_block(function, "unreachable");
                self.builder.build_unconditional_branch(unreachable)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(unreachable);
                self.builder.build_unreachable()
                    .map_err(|e| CompileError::LlvmError(format!("unreachable terminator: {}", e)))?;
                self.builder.position_at_end(ok_bb);
                Ok(payload)
            }
            "unwrap_or" => {
                if args.is_empty() {
                    return Err("unwrap_or requires a default value".into());
                }
                let default_val = self.compile_expr(&args[0], vars)?;
                let ok_bb = self.context.append_basic_block(function, "unwrap_or_ok");
                let done_bb = self.context.append_basic_block(function, "unwrap_or_done");
                let result_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), "unwrap_or_result")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(result_alloca, payload)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_conditional_branch(disc, ok_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                self.builder.build_store(result_alloca, default_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(ok_bb);
                self.builder.build_load(BasicTypeEnum::IntType(i64_ty), result_alloca, "unwrap_or_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
            "ok_or" => {
                if args.is_empty() {
                    return Err("ok_or requires an error value".into());
                }
                let err_val = self.compile_expr(&args[0], vars)?;
                let ok_bb = self.context.append_basic_block(function, "ok_or_ok");
                let err_bb = self.context.append_basic_block(function, "ok_or_err");
                let merge_bb = self.context.append_basic_block(function, "ok_or_merge");
                let result_sty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i1_ty),
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let result_alloca = self.builder.build_alloca(BasicTypeEnum::StructType(result_sty), "ok_or_result")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_conditional_branch(disc, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Ok path: disc=1, ok=payload, err=0
                self.builder.position_at_end(ok_bb);
                let disc_gep = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_sty), result_alloca, 0, "disc_gep"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(disc_gep, self.context.bool_type().const_int(1, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let ok_gep = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_sty), result_alloca, 1, "ok_gep"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(ok_gep, payload)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let err_pad_gep = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_sty), result_alloca, 2, "err_pad_gep"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(err_pad_gep, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Err path: disc=0, ok=0, err=err_val
                self.builder.position_at_end(err_bb);
                let disc_gep2 = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_sty), result_alloca, 0, "disc_gep2"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(disc_gep2, self.context.bool_type().const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let ok_pad_gep = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_sty), result_alloca, 1, "ok_pad_gep"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(ok_pad_gep, i64_ty.const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let err_gep = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(result_sty), result_alloca, 2, "err_gep"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(err_gep, err_val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(merge_bb);
                self.builder.build_load(BasicTypeEnum::StructType(result_sty), result_alloca, "ok_or_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
            "map" => {
                if args.is_empty() {
                    return Err("map requires a function argument".into());
                }
                let closure_val = self.compile_expr_or_func_ref(&args[0], vars)?;
                let ok_bb = self.context.append_basic_block(function, "variant_map_ok");
                let err_bb = self.context.append_basic_block(function, "variant_map_err");
                let merge_bb = self.context.append_basic_block(function, "variant_map_merge");
                let result_alloca = self.builder.build_alloca(
                    BasicTypeEnum::StructType(variant_sty), "variant_map_result"
                ).map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_conditional_branch(disc, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Err path: write Err variant {disc=0, ok=0, err=copy_from_source}
                self.builder.position_at_end(err_bb);
                self.emit_variant_err_path(is_result, variant_sty, pv, result_alloca)?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Ok path: call fn(payload), write Ok variant {disc=1, ok=mapped}
                self.builder.position_at_end(ok_bb);
                let mapped = self.compile_call_fn_ref(closure_val, &args[0], payload, i64_ty)?;
                let d_gep_o = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(variant_sty), result_alloca, 0, "d_gep_o"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(d_gep_o, self.context.bool_type().const_int(1, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let o_gep_o = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(variant_sty), result_alloca, 1, "o_gep_o"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(o_gep_o, mapped)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(merge_bb);
                self.builder.build_load(BasicTypeEnum::StructType(variant_sty), result_alloca, "variant_map_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
            "and_then" => {
                if args.is_empty() {
                    return Err("and_then requires a function argument".into());
                }
                let closure_val = self.compile_expr_or_func_ref(&args[0], vars)?;
                let ok_bb = self.context.append_basic_block(function, "variant_and_then_ok");
                let err_bb = self.context.append_basic_block(function, "variant_and_then_err");
                let merge_bb = self.context.append_basic_block(function, "variant_and_then_merge");
                let result_alloca = self.builder.build_alloca(
                    BasicTypeEnum::StructType(variant_sty), "variant_and_then_result"
                ).map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_conditional_branch(disc, ok_bb, err_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Err path: write Err variant {disc=0, ok=0, err=copy_from_source}
                self.builder.position_at_end(err_bb);
                self.emit_variant_err_path(is_result, variant_sty, pv, result_alloca)?;
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                // Ok path: call fn(payload), store resulting variant into result_alloca
                self.builder.position_at_end(ok_bb);
                let fn_result = self.compile_call_fn_ref(closure_val, &args[0], payload, i64_ty)?;
                match fn_result {
                    BasicValueEnum::StructValue(sv) => {
                        self.builder.build_store(result_alloca, sv)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                    }
                    _ => return Err("and_then: function must return a variant struct".into()),
                }
                self.builder.build_unconditional_branch(merge_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(merge_bb);
                self.builder.build_load(BasicTypeEnum::StructType(variant_sty), result_alloca, "variant_and_then_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
            "map_err" => {
                if args.is_empty() {
                    return Err("map_err requires a function argument".into());
                }
                if !is_result {
                    return Err("map_err is only available on Result types".into());
                }
                let closure_val = self.compile_expr_or_func_ref(&args[0], vars)?;
                let ok_bb = self.context.append_basic_block(function, "map_err_ok");
                let done_bb = self.context.append_basic_block(function, "map_err_done");
                let result_alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), "map_err_result")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(result_alloca, payload)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_conditional_branch(disc, ok_bb, done_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(done_bb);
                let err_gep = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(variant_sty), pv, 2, "err_gep"
                ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let err_payload = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), err_gep, "err_payload")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                let mapped = self.compile_closure_call(closure_val, err_payload.into_int_value())?;
                self.builder.build_store(result_alloca, mapped)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(ok_bb);
                self.builder.build_load(BasicTypeEnum::IntType(i64_ty), result_alloca, "map_err_val")
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
            _ => Err(format!("variant '{}' has no method '{}'", obj_type, method).into()),
        }
    }
    pub(in crate::codegen) fn emit_variant_err_path(
        &self,
        is_result: bool,
        variant_sty: inkwell::types::StructType<'ctx>,
        pv: inkwell::values::PointerValue<'ctx>,
        result_alloca: inkwell::values::PointerValue<'ctx>,
    ) -> Result<(), CompileError> {
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let d_gep_e = self.builder.build_struct_gep(
            BasicTypeEnum::StructType(variant_sty), result_alloca, 0, "d_gep_e"
        ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(d_gep_e, i1_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let o_gep_e = self.builder.build_struct_gep(
            BasicTypeEnum::StructType(variant_sty), result_alloca, 1, "o_gep_e"
        ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(o_gep_e, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        if is_result {
            let src_err_gep = self.builder.build_struct_gep(
                BasicTypeEnum::StructType(variant_sty), pv, 2, "src_err_gep"
            ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let err_val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), src_err_gep, "err_val")
                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            let dst_err_gep = self.builder.build_struct_gep(
                BasicTypeEnum::StructType(variant_sty), result_alloca, 2, "dst_err_gep"
            ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.builder.build_store(dst_err_gep, err_val)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        }
        Ok(())
    }
}
