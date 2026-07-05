use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue,
};
use std::collections::HashMap;

/// Shared context for compiling a method call on an Option/Result-like value.
#[derive(Clone)]
struct VariantMethodCtx<'ctx> {
    function: FunctionValue<'ctx>,
    disc: IntValue<'ctx>,
    payload: BasicValueEnum<'ctx>,
    payload_ty: BasicTypeEnum<'ctx>,
    pv: PointerValue<'ctx>,
    variant_sty: StructType<'ctx>,
    is_result: bool,
    /// If the variant value is bound to a variable, its name. Used to detect
    /// `weak.upgrade()` results whose payload is a pointer to the shared heap
    /// even when the inner type is a primitive.
    obj_name: Option<String>,
}

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_constructor(
        &mut self,
        name: &str,
        compiled_args: Vec<BasicValueEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match name {
            "Ok" => self.compile_ok_constructor(compiled_args),
            "Some" => self.compile_some_constructor(compiled_args),
            "Err" => self.compile_err_constructor(compiled_args),
            "None" => self.compile_none_constructor(compiled_args),
            _ => Err(format!("unknown constructor '{}'", name).into()),
        }
    }

    fn compile_ok_constructor(
        &mut self,
        compiled_args: Vec<BasicValueEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if compiled_args.len() != 1 {
            return Err("Ok expects 1 argument".into());
        }
        let val = compiled_args[0];
        let bool_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let disc = bool_ty.const_int(1, false);
        let inner_ty = val.get_type();
        let struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                inner_ty,
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(struct_ty, "ok_val")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 0, "disc")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(disc_gep, disc)?;
        let val_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 1, "payload")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(val_gep, val)?;
        let err_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 2, "err_pad")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_gep, i64_ty.const_int(0, false))?;
        self.build_load(struct_ty, alloca, "loaded")
    }

    fn compile_some_constructor(
        &mut self,
        compiled_args: Vec<BasicValueEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if compiled_args.len() != 1 {
            return Err("Some expects 1 argument".into());
        }
        let val = compiled_args[0];
        let bool_ty = self.context.bool_type();
        let disc = bool_ty.const_int(1, false);
        let inner_ty = val.get_type();
        let struct_ty = self
            .context
            .struct_type(&[BasicTypeEnum::IntType(bool_ty), inner_ty], false);
        let alloca = self.build_alloca(struct_ty, "some_val")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 0, "disc")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(disc_gep, disc)?;
        let val_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 1, "payload")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(val_gep, val)?;
        self.build_load(struct_ty, alloca, "loaded")
    }

    fn compile_err_constructor(
        &mut self,
        compiled_args: Vec<BasicValueEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
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
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "err_sext")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("int sign extend error: {}", e))
                        })?
                        .into()
                } else if bit_width > 64 {
                    self.builder
                        .build_int_truncate(iv, i64_ty, "err_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("int truncate error: {}", e)))?
                        .into()
                } else {
                    iv.into()
                }
            }
            BasicValueEnum::PointerValue(pv) => {
                self.build_ptr_to_int(pv, i64_ty, "err_to_i64")?.into()
            }
            BasicValueEnum::StructValue(sv) => {
                // Check if this is a Mimi string struct {ptr, i64} (from string literal).
                // If so, extract the raw pointer and convert to i64.
                let sv_fields = sv.get_type().get_field_types();
                let is_mimi_string = sv_fields.len() == 2
                    && matches!(&sv_fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(&sv_fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if is_mimi_string {
                    let str_ptr = self
                        .build_extract_value(sv.into(), 0, "err_str_ptr")?
                        .into_pointer_value();
                    self.build_ptr_to_int(str_ptr, i64_ty, "err_str_i64")?
                        .into()
                } else {
                    // Custom enum values are {i32 tag, i64 payload}; Result stores
                    // only the tag in its error slot.
                    let tag = self
                        .build_extract_value(sv.into(), 0, "enum_tag")?
                        .into_int_value();
                    self.builder
                        .build_int_cast(tag, i64_ty, "err_tag_ext")
                        .map_err(|e| CompileError::LlvmError(format!("int cast error: {}", e)))?
                        .into()
                }
            }
            _ => return Err("Err: unsupported error value type".into()),
        };
        let struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(struct_ty, "err_val")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 0, "disc")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(disc_gep, disc)?;
        let ok_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 1, "ok_pad")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ok_gep, i64_ty.const_int(0, false))?;
        let err_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 2, "err_payload")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_gep, err_val)?;
        self.build_load(struct_ty, alloca, "loaded")
    }

    fn compile_none_constructor(
        &mut self,
        compiled_args: Vec<BasicValueEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if !compiled_args.is_empty() {
            return Err("None expects 0 arguments".into());
        }
        let bool_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let disc = bool_ty.const_int(0, false);
        let struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(struct_ty, "none_val")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 0, "disc")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(disc_gep, disc)?;
        let val_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 1, "payload")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(val_gep, i64_ty.const_int(0, false))?;
        self.build_load(struct_ty, alloca, "loaded")
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
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for variant method".to_string())?;

        // Layout: Result<T,E> = {i1 disc, T ok, i64 err}, Option<T> = {i1 disc, T payload}
        let disc_idx: u32 = 0;
        let payload_idx: u32 = 1;
        let variant_sty = if is_result {
            self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i1_ty),
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            )
        } else {
            self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i1_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            )
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
                let tmp = self.build_alloca(sty_enum, "variant_tmp")?;
                self.build_store(tmp, sv)?;
                (tmp, sty_enum)
            }
            _ => {
                return Err(format!(
                    "variant method '{}' requires a struct pointer or value",
                    method
                )
                .into())
            }
        };

        // Extract the actual payload type from the struct's field types
        let payload_ty = if let BasicTypeEnum::StructType(st) = actual_sty_enum {
            let fields = st.get_field_types();
            if (payload_idx as usize) < fields.len() {
                fields[payload_idx as usize]
            } else {
                BasicTypeEnum::IntType(i64_ty)
            }
        } else {
            BasicTypeEnum::IntType(i64_ty)
        };

        let disc_gep = self
            .gep()
            .build_struct_gep(actual_sty_enum, pv, disc_idx, "disc_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let disc = self
            .build_load(BasicTypeEnum::IntType(i1_ty), disc_gep, "disc")?
            .into_int_value();
        let pay_gep = self
            .gep()
            .build_struct_gep(actual_sty_enum, pv, payload_idx, "pay_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let payload = self.build_load(payload_ty, pay_gep, "payload")?;

        let obj_name = if let Expr::Ident(name) = obj {
            Some(name.clone())
        } else {
            None
        };
        let ctx = VariantMethodCtx {
            function,
            disc,
            payload,
            payload_ty,
            pv,
            variant_sty,
            is_result,
            obj_name,
        };

        match method {
            "is_ok" | "is_some" => self.compile_is_predicate(disc, true),
            "is_err" | "is_none" => self.compile_is_predicate(disc, false),
            "unwrap" | "expect" => self.compile_unwrap_expect(ctx),
            "unwrap_or" => self.compile_unwrap_or(args, vars, ctx),
            "ok_or" => self.compile_ok_or(args, vars, ctx),
            "map" => self.compile_variant_map(args, vars, ctx),
            "and_then" => self.compile_variant_and_then(args, vars, ctx),
            "map_err" => self.compile_map_err(args, vars, ctx),
            _ => Err(format!("variant '{}' has no method '{}'", obj_type, method).into()),
        }
    }

    /// Compiles `.is_<variant>()` predicate methods.
    /// When `expect_true` is true, returns the discriminator as a bool (i64);
    /// otherwise returns its logical negation.
    fn compile_is_predicate(
        &self,
        disc: IntValue<'ctx>,
        expect_true: bool,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let bool_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let cond = if expect_true {
            disc
        } else {
            self.builder
                .build_not(disc, "is_not")
                .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?
        };
        // Truncate i8 to i1 (bool), then extend to i64 for uniform representation
        let bool_i1 = self
            .builder
            .build_int_truncate(cond, bool_ty, "is_trunc")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        let bool_val = self
            .builder
            .build_int_z_extend(bool_i1, i64_ty, "is_ext")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(BasicValueEnum::IntValue(bool_val))
    }

    /// Compiles `.unwrap()` / `.expect()` projection methods.
    fn compile_unwrap_expect(
        &mut self,
        ctx: VariantMethodCtx<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ok_bb = self.context.append_basic_block(ctx.function, "unwrap_ok");
        let err_bb = self.context.append_basic_block(ctx.function, "unwrap_err");
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;
        self.builder.position_at_end(err_bb);
        let trap_fn = self
            .module
            .get_function("mimi_try_exit")
            .or_else(|| self.module.get_function("abort"))
            .ok_or("abort not declared")?;
        self.build_call(
            trap_fn,
            &[BasicMetadataValueEnum::IntValue(
                self.context.i64_type().const_int(0, false),
            )],
            "unwrap_trap",
        )?;
        let unreachable = self.context.append_basic_block(ctx.function, "unreachable");
        self.build_br(unreachable)?;
        self.builder.position_at_end(unreachable);
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable terminator: {}", e)))?;
        self.builder.position_at_end(ok_bb);

        // Special case: `weak.upgrade()` returns an Option whose payload is a
        // pointer to the shared heap, even when the inner type is a primitive.
        // For primitive inner types, unwrap() must load the value through the
        // pointer rather than returning the pointer itself.
        if let Some(name) = &ctx.obj_name {
            if self.upgrade_option_vars.contains(name.as_str()) {
                if let Some(Type::Option(inner)) = self.var_types.get(name).cloned() {
                    let loaded = self.load_upgrade_payload(ctx.payload, &inner)?;
                    return Ok(loaded);
                }
            }
        }

        Ok(ctx.payload)
    }

    /// Load the actual value from a `weak.upgrade()` payload pointer.
    fn load_upgrade_payload(
        &self,
        payload: BasicValueEnum<'ctx>,
        inner: &Type,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let ptr_val = payload.into_int_value();
        let ptr = self
            .builder
            .build_int_to_ptr(
                ptr_val,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "upgrade_payload_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?;
        let llvm_ty = crate::codegen::types::mimi_type_to_llvm(self.context, inner)
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
        self.build_load(llvm_ty, ptr, "upgrade_payload_val")
    }

    /// Compiles `.unwrap_or(default)`.
    fn compile_unwrap_or(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        ctx: VariantMethodCtx<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if args.is_empty() {
            return Err("unwrap_or requires a default value".into());
        }
        let default_val = self.compile_expr(&args[0], vars)?;
        let ok_bb = self
            .context
            .append_basic_block(ctx.function, "unwrap_or_ok");
        let done_bb = self
            .context
            .append_basic_block(ctx.function, "unwrap_or_done");
        let result_alloca = self.build_alloca(ctx.payload_ty, "unwrap_or_result")?;
        self.build_store(result_alloca, ctx.payload)?;
        self.build_cond_br(ctx.disc, ok_bb, done_bb)?;
        self.builder.position_at_end(done_bb);
        self.build_store(result_alloca, default_val)?;
        self.build_br(ok_bb)?;
        self.builder.position_at_end(ok_bb);
        self.build_load(ctx.payload_ty, result_alloca, "unwrap_or_val")
    }

    /// Compiles `.ok_or(err)` for Option-like values.
    fn compile_ok_or(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        ctx: VariantMethodCtx<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if args.is_empty() {
            return Err("ok_or requires an error value".into());
        }
        let err_val = self.compile_expr(&args[0], vars)?;
        let ok_bb = self.context.append_basic_block(ctx.function, "ok_or_ok");
        let err_bb = self.context.append_basic_block(ctx.function, "ok_or_err");
        let merge_bb = self.context.append_basic_block(ctx.function, "ok_or_merge");
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let result_sty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i1_ty),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let result_alloca =
            self.build_alloca(BasicTypeEnum::StructType(result_sty), "ok_or_result")?;
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;
        // Ok path: disc=1, ok=payload, err=0
        self.builder.position_at_end(ok_bb);
        let disc_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                0,
                "disc_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(disc_gep, self.context.bool_type().const_int(1, false))?;
        let ok_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                1,
                "ok_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ok_gep, ctx.payload)?;
        let err_pad_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                2,
                "err_pad_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_pad_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;
        // Err path: disc=0, ok=0, err=err_val
        self.builder.position_at_end(err_bb);
        let disc_gep2 = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                0,
                "disc_gep2",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(disc_gep2, self.context.bool_type().const_int(0, false))?;
        let ok_pad_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                1,
                "ok_pad_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ok_pad_gep, i64_ty.const_int(0, false))?;
        let err_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                2,
                "err_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(err_gep, err_val)?;
        self.build_br(merge_bb)?;
        self.builder.position_at_end(merge_bb);
        self.build_load(
            BasicTypeEnum::StructType(result_sty),
            result_alloca,
            "ok_or_val",
        )
    }

    /// Compiles `.map(fn)` for Option/Result-like values.
    fn compile_variant_map(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        ctx: VariantMethodCtx<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if args.is_empty() {
            return Err("map requires a function argument".into());
        }
        let closure_val = self.compile_expr_or_func_ref(&args[0], vars)?;
        let ok_bb = self
            .context
            .append_basic_block(ctx.function, "variant_map_ok");
        let err_bb = self
            .context
            .append_basic_block(ctx.function, "variant_map_err");
        let merge_bb = self
            .context
            .append_basic_block(ctx.function, "variant_map_merge");
        let result_alloca = self.build_alloca(
            BasicTypeEnum::StructType(ctx.variant_sty),
            "variant_map_result",
        )?;
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;
        // Err path: write Err variant {disc=0, ok=0, err=copy_from_source}
        self.builder.position_at_end(err_bb);
        self.emit_variant_err_path(ctx.is_result, ctx.variant_sty, ctx.pv, result_alloca)?;
        self.build_br(merge_bb)?;
        // Ok path: call fn(payload), write Ok variant {disc=1, ok=mapped}
        self.builder.position_at_end(ok_bb);
        let mapped =
            self.compile_call_fn_ref(closure_val, &args[0], ctx.payload, self.context.i64_type())?;
        let d_gep_o = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(ctx.variant_sty),
                result_alloca,
                0,
                "d_gep_o",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(d_gep_o, self.context.bool_type().const_int(1, false))?;
        let o_gep_o = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(ctx.variant_sty),
                result_alloca,
                1,
                "o_gep_o",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(o_gep_o, mapped)?;
        self.build_br(merge_bb)?;
        self.builder.position_at_end(merge_bb);
        self.build_load(
            BasicTypeEnum::StructType(ctx.variant_sty),
            result_alloca,
            "variant_map_val",
        )
    }

    /// Compiles `.and_then(fn)` for Option/Result-like values.
    fn compile_variant_and_then(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        ctx: VariantMethodCtx<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if args.is_empty() {
            return Err("and_then requires a function argument".into());
        }
        let closure_val = self.compile_expr_or_func_ref(&args[0], vars)?;
        let ok_bb = self
            .context
            .append_basic_block(ctx.function, "variant_and_then_ok");
        let err_bb = self
            .context
            .append_basic_block(ctx.function, "variant_and_then_err");
        let merge_bb = self
            .context
            .append_basic_block(ctx.function, "variant_and_then_merge");
        let result_alloca = self.build_alloca(
            BasicTypeEnum::StructType(ctx.variant_sty),
            "variant_and_then_result",
        )?;
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;
        // Err path: write Err variant {disc=0, ok=0, err=copy_from_source}
        self.builder.position_at_end(err_bb);
        self.emit_variant_err_path(ctx.is_result, ctx.variant_sty, ctx.pv, result_alloca)?;
        self.build_br(merge_bb)?;
        // Ok path: call fn(payload), store resulting variant into result_alloca
        self.builder.position_at_end(ok_bb);
        let fn_result =
            self.compile_call_fn_ref(closure_val, &args[0], ctx.payload, self.context.i64_type())?;
        match fn_result {
            BasicValueEnum::StructValue(sv) => {
                self.build_store(result_alloca, sv)?;
            }
            _ => return Err("and_then: function must return a variant struct".into()),
        }
        self.build_br(merge_bb)?;
        self.builder.position_at_end(merge_bb);
        self.build_load(
            BasicTypeEnum::StructType(ctx.variant_sty),
            result_alloca,
            "variant_and_then_val",
        )
    }

    /// Compiles `.map_err(fn)` for Result-like values.
    fn compile_map_err(
        &mut self,
        args: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
        ctx: VariantMethodCtx<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if args.is_empty() {
            return Err("map_err requires a function argument".into());
        }
        let closure_val = self.compile_expr_or_func_ref(&args[0], vars)?;
        let ok_bb = self.context.append_basic_block(ctx.function, "map_err_ok");
        let done_bb = self
            .context
            .append_basic_block(ctx.function, "map_err_done");
        let i64_ty = self.context.i64_type();
        let result_alloca = self.build_alloca(BasicTypeEnum::IntType(i64_ty), "map_err_result")?;
        self.build_store(result_alloca, ctx.payload)?;
        self.build_cond_br(ctx.disc, ok_bb, done_bb)?;
        self.builder.position_at_end(done_bb);
        let err_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(ctx.variant_sty),
                ctx.pv,
                2,
                "err_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let err_payload =
            self.build_load(BasicTypeEnum::IntType(i64_ty), err_gep, "err_payload")?;
        let mapped = self.compile_closure_call(closure_val, &[err_payload], None)?;
        self.build_store(result_alloca, mapped)?;
        self.build_br(ok_bb)?;
        self.builder.position_at_end(ok_bb);
        self.build_load(BasicTypeEnum::IntType(i64_ty), result_alloca, "map_err_val")
    }

    pub(in crate::codegen) fn emit_variant_err_path(
        &self,
        is_result: bool,
        variant_sty: StructType<'ctx>,
        pv: PointerValue<'ctx>,
        result_alloca: PointerValue<'ctx>,
    ) -> Result<(), CompileError> {
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let d_gep_e = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(variant_sty),
                result_alloca,
                0,
                "d_gep_e",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(d_gep_e, i1_ty.const_int(0, false))?;
        let o_gep_e = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(variant_sty),
                result_alloca,
                1,
                "o_gep_e",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(o_gep_e, i64_ty.const_int(0, false))?;
        if is_result {
            let src_err_gep = self
                .gep()
                .build_struct_gep(BasicTypeEnum::StructType(variant_sty), pv, 2, "src_err_gep")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let err_val =
                self.build_load(BasicTypeEnum::IntType(i64_ty), src_err_gep, "err_val")?;
            let dst_err_gep = self
                .gep()
                .build_struct_gep(
                    BasicTypeEnum::StructType(variant_sty),
                    result_alloca,
                    2,
                    "dst_err_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            self.build_store(dst_err_gep, err_val)?;
        }
        Ok(())
    }
}
