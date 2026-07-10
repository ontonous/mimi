use crate::ast::*;
use crate::codegen::{call_try_basic_value, CodeGenerator, VarEntry};
use crate::error::CompileError;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue,
};
use std::collections::HashMap;

/// Parse a concrete Result<T,E> or Option<T> type string and return
/// the inner payload type's name. Returns None for non-variant types.
fn variant_payload_type_name(type_str: &str) -> Option<String> {
    if let Some(inner) = type_str.strip_prefix("Result<") {
        let mut depth = 0u32;
        let mut comma_pos = None;
        for (i, ch) in inner.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                ',' if depth == 0 => {
                    comma_pos = Some(i);
                    break;
                }
                _ => {}
            }
        }
        Some(
            comma_pos
                .map(|pos| inner[..pos].trim())
                .unwrap_or("i64")
                .to_string(),
        )
    } else if let Some(inner) = type_str.strip_prefix("Option<") {
        let mut depth = 0u32;
        let mut end_pos = None;
        for (i, ch) in inner.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    if depth == 0 {
                        end_pos = Some(i);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        Some(
            end_pos
                .map(|pos| inner[..pos].to_string())
                .unwrap_or_else(|| inner.to_string()),
        )
    } else {
        None
    }
}

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
                // Check if this is a Mimi string struct {ptr, i64}.
                // If so, heap-allocate a {ptr, len} struct, store it there,
                // and store the pointer as i64. The `?` operator in
                // try_expr.rs reconstructs via inttoptr + GEP + load.
                let sv_fields = sv.get_type().get_field_types();
                let is_mimi_string = sv_fields.len() == 2
                    && matches!(&sv_fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(&sv_fields[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if is_mimi_string {
                    // CG-C3: store both ptr AND length into the heap-allocated
                    // string struct {ptr, i64}. The ? operator in try_expr.rs
                    // reconstructs from both fields, so no length is lost.
                    let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let string_struct_ty = self.context.struct_type(
                        &[
                            BasicTypeEnum::PointerType(i8_ptr_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    );
                    let heap_ty = BasicTypeEnum::StructType(string_struct_ty);
                    let malloc_fn = self.get_runtime_fn("malloc")?;
                    let alloc_size = i64_ty.const_int(16, false);
                    let heap_ptr = call_try_basic_value(&self.build_call(
                        malloc_fn,
                        &[BasicMetadataValueEnum::IntValue(alloc_size)],
                        "err_str_malloc",
                    )?)
                    .ok_or_else(|| {
                        CompileError::LlvmError("malloc for err string returned void".into())
                    })?
                    .into_pointer_value();
                    let str_ptr_gep = self
                        .gep()
                        .build_struct_gep(heap_ty, heap_ptr, 0, "err_str_ptr_gep")
                        .map_err(|e| CompileError::LlvmError(format!("err str gep: {}", e)))?;
                    self.build_store(
                        str_ptr_gep,
                        self.build_extract_value(sv.into(), 0, "err_str_ptr")?,
                    )?;
                    let str_len_gep = self
                        .gep()
                        .build_struct_gep(heap_ty, heap_ptr, 1, "err_str_len_gep")
                        .map_err(|e| CompileError::LlvmError(format!("err len gep: {}", e)))?;
                    self.build_store(
                        str_len_gep,
                        self.build_extract_value(sv.into(), 1, "err_str_len")?,
                    )?;
                    self.build_ptr_to_int(heap_ptr, i64_ty, "err_str_heap_i64")?
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

    /// Given a type string like `"Result<string, i64>"` or `"Option<string>"`,
    /// parse the payload type name and build the correct LLVM struct layout.
    /// Returns `None` when the type string cannot be parsed (e.g. raw
    /// `"Result"` or `"Option"` without generics).
    fn compute_variant_struct_type(&self, type_str: &str) -> Option<BasicTypeEnum<'ctx>> {
        let payload_name = variant_payload_type_name(type_str)?;
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let payload_ty = Type::Name(payload_name, vec![]);
        let payload_llvm = self.llvm_type_for(&payload_ty)?;
        if type_str.starts_with("Result<") {
            Some(BasicTypeEnum::StructType(self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i1_ty),
                    payload_llvm,
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            )))
        } else {
            Some(BasicTypeEnum::StructType(self.context.struct_type(
                &[BasicTypeEnum::IntType(i1_ty), payload_llvm],
                false,
            )))
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
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for variant method".to_string())?;

        // Layout: Result<T,E> = {i1 disc, T ok, i64 err}, Option<T> = {i1 disc, T payload}
        let disc_idx: u32 = 0;
        let payload_idx: u32 = 1;

        // Compute the CORRECT struct layout from the Mimi-level type string
        // (e.g. "Result<string, i64>" → {i1, {ptr,i64}, i64}).
        // This is needed because constructors (Err/None) may create structs
        // with narrower layouts ({i1,i64,i64}) when the payload is string.
        let inferred_sty_enum = self
            .compute_variant_struct_type(&obj_type)
            .unwrap_or_else(|| {
                if is_result {
                    BasicTypeEnum::StructType(self.context.struct_type(
                        &[
                            BasicTypeEnum::IntType(i1_ty),
                            BasicTypeEnum::IntType(i64_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    ))
                } else {
                    BasicTypeEnum::StructType(self.context.struct_type(
                        &[
                            BasicTypeEnum::IntType(i1_ty),
                            BasicTypeEnum::IntType(i64_ty),
                        ],
                        false,
                    ))
                }
            });

        // Convert StructValue to PointerValue for uniform handling.
        // Use the INFERRED struct type for correct GEP offsets even when
        // the runtime value has a narrower layout (e.g. Err constructor
        // creates {i1,i64,i64} instead of {i1,{ptr,i64},i64}).
        let (pv, actual_sty_enum) = match obj_val {
            BasicValueEnum::PointerValue(pv) => {
                let sty = if let Expr::Ident(name) = obj {
                    vars.get(name.as_str())
                        .map(|entry| entry.1)
                        .unwrap_or(inferred_sty_enum)
                } else {
                    inferred_sty_enum
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

        // Extract the payload type from the struct's field types.
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
            variant_sty: match inferred_sty_enum {
                BasicTypeEnum::StructType(st) => st,
                _ if is_result => self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                ),
                _ => self.context.struct_type(
                    &[
                        BasicTypeEnum::IntType(self.context.bool_type()),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                ),
            },
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
        // Use the default value's type for the alloca — this is the authoritative
        // type (enforced by the type checker). For Ok(payload) payload has the
        // same type; for Err/None with narrow structs, the payload type may be i64
        // while default is {ptr,i64}, but we only store the default in that branch.
        let alloca_ty = default_val.get_type();
        let ok_bb = self
            .context
            .append_basic_block(ctx.function, "unwrap_or_ok");
        let err_bb = self
            .context
            .append_basic_block(ctx.function, "unwrap_or_err");
        let merge_bb = self
            .context
            .append_basic_block(ctx.function, "unwrap_or_done");
        let result_alloca = self.build_alloca(alloca_ty, "unwrap_or_result")?;
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;

        self.builder.position_at_end(ok_bb);
        // Ok branch: store the payload (always the correct type for real Ok values)
        self.build_store(result_alloca, ctx.payload)?;
        self.build_br(merge_bb)?;

        self.builder.position_at_end(err_bb);
        // Err branch: store the default value
        self.build_store(result_alloca, default_val)?;
        self.build_br(merge_bb)?;

        self.builder.position_at_end(merge_bb);
        self.build_load(alloca_ty, result_alloca, "unwrap_or_val")
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
        // Use the inferred payload type from ctx.variant_sty (the correct struct
        // layout from compute_variant_struct_type) rather than ctx.payload_ty
        // (which is narrow i64 when the Option was created by None).
        let expected_fields = ctx.variant_sty.get_field_types();
        let real_payload_ty = if expected_fields.len() > 1 {
            expected_fields[1]
        } else {
            ctx.payload_ty
        };
        // Build Result<T,E> with the correct payload type for field 1.
        let result_sty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i1_ty),
                real_payload_ty,
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
        // Err path: disc=0, ok=zero/undef, err=err_val
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
        let zero_payload = Self::zero_value_for_type(ctx.payload_ty, i64_ty);
        self.build_store(ok_pad_gep, zero_payload)?;
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
        let mapped_ty = self
            .infer_fn_return_llvm_type(&args[0])
            .unwrap_or(ctx.payload_ty);
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let result_sty = if ctx.is_result {
            self.context.struct_type(
                &[
                    BasicTypeEnum::IntType(i1_ty),
                    mapped_ty,
                    BasicTypeEnum::IntType(i64_ty),
                ],
                false,
            )
        } else {
            self.context
                .struct_type(&[BasicTypeEnum::IntType(i1_ty), mapped_ty], false)
        };
        let ok_bb = self
            .context
            .append_basic_block(ctx.function, "variant_map_ok");
        let err_bb = self
            .context
            .append_basic_block(ctx.function, "variant_map_err");
        let merge_bb = self
            .context
            .append_basic_block(ctx.function, "variant_map_merge");
        let result_alloca =
            self.build_alloca(BasicTypeEnum::StructType(result_sty), "variant_map_result")?;
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;
        // Err path: write Err variant {disc=0, ok=undef, err=copy_from_source}
        self.builder.position_at_end(err_bb);
        self.emit_variant_err_path(ctx.is_result, result_sty, ctx.pv, result_alloca)?;
        self.build_br(merge_bb)?;
        // Ok path: call fn(payload), write Ok variant {disc=1, ok=mapped}
        self.builder.position_at_end(ok_bb);
        let mapped =
            self.compile_map_closure_call(closure_val, &args[0], ctx.payload, mapped_ty)?;
        let d_gep_o = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                0,
                "d_gep_o",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(d_gep_o, self.context.bool_type().const_int(1, false))?;
        let o_gep_o = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                1,
                "o_gep_o",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(o_gep_o, mapped)?;
        self.build_br(merge_bb)?;
        self.builder.position_at_end(merge_bb);
        self.build_load(
            BasicTypeEnum::StructType(result_sty),
            result_alloca,
            "variant_map_val",
        )
    }

    /// Call a function/closure value that takes a single payload argument and
    /// returns a value of type `ret_ty`.  Handles both named functions (via
    /// `build_call`) and closure/function-pointer values (via
    /// `compile_closure_call`).
    fn compile_map_closure_call(
        &mut self,
        fn_val: BasicValueEnum<'ctx>,
        fn_expr: &Expr,
        payload: BasicValueEnum<'ctx>,
        ret_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match fn_val {
            BasicValueEnum::PointerValue(_) => {
                if let Expr::Ident(name) = fn_expr {
                    if let Some(func) = self.module.get_function(name) {
                        let meta = crate::codegen::types::basic_value_to_metadata_value(
                            &payload,
                            self.context.i64_type(),
                        );
                        let call = self
                            .builder
                            .build_call(func, &[meta], "map_call")
                            .map_err(|e| CompileError::LlvmError(format!("map call: {}", e)))?;
                        return Ok(call_try_basic_value(&call).unwrap_or(
                            BasicValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                        ));
                    }
                }
                self.compile_closure_call(fn_val, &[payload], Some(ret_ty))
            }
            BasicValueEnum::StructValue(_) => {
                self.compile_closure_call(fn_val, &[payload], Some(ret_ty))
            }
            _ => Err("map: expected a closure or function reference".into()),
        }
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
        let fn_val = self.compile_expr_or_func_ref(&args[0], vars)?;
        let fn_ret_llvm = self.infer_fn_return_llvm_type(&args[0]);
        let result_sty = fn_ret_llvm
            .and_then(|rt| {
                if let BasicTypeEnum::StructType(st) = rt {
                    Some(st)
                } else {
                    None
                }
            })
            .unwrap_or(ctx.variant_sty);
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
            BasicTypeEnum::StructType(result_sty),
            "variant_and_then_result",
        )?;
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;
        // Err path: write Err variant {disc=0, ok=undef, err=copy_from_source}
        self.builder.position_at_end(err_bb);
        self.emit_variant_err_path(ctx.is_result, result_sty, ctx.pv, result_alloca)?;
        self.build_br(merge_bb)?;
        // Ok path: call fn(payload), store resulting variant into result_alloca
        self.builder.position_at_end(ok_bb);
        let fn_result =
            self.compile_and_then_closure_call(fn_val, &args[0], ctx.payload, fn_ret_llvm)?;
        match fn_result {
            BasicValueEnum::StructValue(sv) => {
                self.build_store(result_alloca, sv)?;
            }
            _ => return Err("and_then: function must return a variant struct".into()),
        }
        self.build_br(merge_bb)?;
        self.builder.position_at_end(merge_bb);
        self.build_load(
            BasicTypeEnum::StructType(result_sty),
            result_alloca,
            "variant_and_then_val",
        )
    }

    /// Call a function/closure that takes a single payload argument and returns
    /// a variant struct (used by `and_then`).  Handles both named functions
    /// (via `build_call`) and closure/function-pointer values (via
    /// `compile_closure_call`).
    fn compile_and_then_closure_call(
        &mut self,
        fn_val: BasicValueEnum<'ctx>,
        fn_expr: &Expr,
        payload: BasicValueEnum<'ctx>,
        ret_ty: Option<BasicTypeEnum<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match fn_val {
            BasicValueEnum::PointerValue(_) => {
                if let Expr::Ident(name) = fn_expr {
                    if let Some(func) = self.module.get_function(name) {
                        let meta = crate::codegen::types::basic_value_to_metadata_value(
                            &payload,
                            self.context.i64_type(),
                        );
                        let call = self
                            .builder
                            .build_call(func, &[meta], "and_then_call")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("and_then call: {}", e))
                            })?;
                        return Ok(call_try_basic_value(&call).unwrap_or(
                            BasicValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                        ));
                    }
                }
                self.compile_closure_call(fn_val, &[payload], ret_ty)
            }
            BasicValueEnum::StructValue(_) => self.compile_closure_call(fn_val, &[payload], ret_ty),
            _ => Err("and_then: expected a closure or function reference".into()),
        }
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
        let err_bb = self.context.append_basic_block(ctx.function, "map_err_err");
        let merge_bb = self
            .context
            .append_basic_block(ctx.function, "map_err_merge");
        let i1_ty = self.context.bool_type();
        let i64_ty = self.context.i64_type();
        let result_sty = ctx.variant_sty;
        let result_alloca =
            self.build_alloca(BasicTypeEnum::StructType(result_sty), "map_err_result")?;
        self.build_cond_br(ctx.disc, ok_bb, err_bb)?;
        // Ok path: disc=1, payload unchanged, err=0
        self.builder.position_at_end(ok_bb);
        let d_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                0,
                "d_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(d_gep, i1_ty.const_int(1, false))?;
        let p_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                1,
                "p_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(p_gep, ctx.payload)?;
        let e_pad_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                2,
                "e_pad_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(e_pad_gep, i64_ty.const_int(0, false))?;
        self.build_br(merge_bb)?;
        // Err path: disc=0, payload=zero, err=mapped from original
        self.builder.position_at_end(err_bb);
        let d_gep2 = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                0,
                "d_gep2",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(d_gep2, i1_ty.const_int(0, false))?;
        let p_pad_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                1,
                "p_pad_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let zero_payload = Self::zero_value_for_type(result_sty.get_field_types()[1], i64_ty);
        self.build_store(p_pad_gep, zero_payload)?;
        let src_err_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                ctx.pv,
                2,
                "src_err_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let err_val = self.build_load(BasicTypeEnum::IntType(i64_ty), src_err_gep, "err_val")?;
        let mapped = self.compile_closure_call(closure_val, &[err_val], None)?;
        let dst_err_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(result_sty),
                result_alloca,
                2,
                "dst_err_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(dst_err_gep, mapped)?;
        self.build_br(merge_bb)?;
        self.builder.position_at_end(merge_bb);
        self.build_load(
            BasicTypeEnum::StructType(result_sty),
            result_alloca,
            "map_err_val",
        )
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
        let fields = variant_sty.get_field_types();
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
        let zero_payload = Self::zero_value_for_type(fields[1], i64_ty);
        self.build_store(o_gep_e, zero_payload)?;
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

    /// Create a zero/null/undef value appropriate for the given LLVM type.
    fn zero_value_for_type(
        ty: BasicTypeEnum<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match ty {
            BasicTypeEnum::IntType(it) => it.const_zero().into(),
            BasicTypeEnum::PointerType(pt) => pt.const_null().into(),
            BasicTypeEnum::StructType(st) => st.get_undef().into(),
            BasicTypeEnum::ArrayType(at) => at.get_undef().into(),
            _ => i64_ty.const_zero().into(),
        }
    }

    /// Infer the LLVM return type of a function/closure expression used as
    /// the argument to `map` / `and_then` from the Mimi type system.
    /// Returns `None` when the type cannot be determined (fall back to
    /// `ctx.payload_ty`).
    fn infer_fn_return_llvm_type(&self, fn_expr: &Expr) -> Option<BasicTypeEnum<'ctx>> {
        match fn_expr {
            Expr::Ident(name) => {
                if let Some(ty) = self.var_types.get(name) {
                    let ret = match ty {
                        Type::Func(_, ret) | Type::ExternFunc(_, ret) => Some(ret.as_ref()),
                        _ => None,
                    };
                    if let Some(ret) = ret {
                        return self.llvm_type_for(ret);
                    }
                }
            }
            Expr::Lambda {
                ret: Some(ret_ty), ..
            } => return self.llvm_type_for(ret_ty),
            _ => {}
        }
        None
    }

    /// Widen a narrow variant struct (created by Err/None constructors)
    /// to match the declared type's expected struct layout.
    /// For example, `Err(42)` produces `{i1,i64,i64}` but when the declared
    /// type is `Result<string,i64>`, it should be `{i1,{ptr,i64},i64}`.
    pub(in crate::codegen) fn inflate_variant_struct(
        &self,
        val: BasicValueEnum<'ctx>,
        declared_ty: &Type,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let actual_sty = match val {
            BasicValueEnum::StructValue(sv) => sv.get_type(),
            _ => return Ok(val),
        };
        let actual_fields = actual_sty.get_field_types();
        let actual_len = actual_fields.len();
        if actual_len < 2 {
            return Ok(val);
        }
        // Get the expected struct type from the type annotation
        let type_str = self.get_full_type_name(declared_ty).unwrap_or_default();
        let expected = self.compute_variant_struct_type(&type_str);
        let Some(BasicTypeEnum::StructType(expected_sty)) = expected else {
            return Ok(val);
        };
        let expected_fields = expected_sty.get_field_types();
        if expected_fields.len() != actual_len {
            return Ok(val);
        }
        // Check if field 1 (payload/ok-pad) type matches
        if expected_fields[1] == actual_fields[1] {
            return Ok(val); // already the correct layout
        }
        // Inflation needed: rebuild the struct with the correct field 1 type.
        // Use alloca + GEP + store to construct the value.
        // Allocate and zero-initialize the wide struct
        let wide_alloca = self.build_alloca(BasicTypeEnum::StructType(expected_sty), "inflated")?;
        let zero_init = self.const_zero_for_type(expected_sty.into());
        self.build_store(wide_alloca, zero_init)?;
        // Field 0: discriminant — extract from the narrow value
        let agg_val = match val {
            BasicValueEnum::StructValue(sv) => inkwell::values::AggregateValueEnum::StructValue(sv),
            _ => return Ok(val),
        };
        let disc = self.build_extract_value(agg_val, 0, "disc")?;
        let disc_gep = self
            .gep()
            .build_struct_gep(
                BasicTypeEnum::StructType(expected_sty),
                wide_alloca,
                0,
                "disc_gep",
            )
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(disc_gep, disc)?;
        // Field 1 (ok/payload pad) — already zero-initialized from const_zero
        // Field 2 (if Result/3-field): extract error value from narrow value
        if actual_len == 3 {
            let err_val = self.build_extract_value(agg_val, 2, "err_val")?;
            let err_gep = self
                .gep()
                .build_struct_gep(
                    BasicTypeEnum::StructType(expected_sty),
                    wide_alloca,
                    2,
                    "err_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.build_store(err_gep, err_val)?;
        }
        // Load the inflated struct
        self.build_load(
            BasicTypeEnum::StructType(expected_sty),
            wide_alloca,
            "inflated_val",
        )
    }

    /// Return a zero-initialised `BasicValueEnum` for a given `BasicTypeEnum`.
    pub(in crate::codegen) fn const_zero_for_type(
        &self,
        ty: BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match ty {
            BasicTypeEnum::IntType(it) => it.const_zero().into(),
            BasicTypeEnum::FloatType(ft) => ft.const_float(0.0).into(),
            BasicTypeEnum::PointerType(pt) => pt.const_null().into(),
            BasicTypeEnum::StructType(st) => st.const_zero().into(),
            BasicTypeEnum::ArrayType(at) => at.get_undef().into(),
            _ => self.context.i64_type().const_zero().into(),
        }
    }
}
