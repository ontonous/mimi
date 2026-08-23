use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn compile_len(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "len expects 1 argument".to_string(),
            ));
        }
        match args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => {
                if self.pending_len_is_string {
                    // String: char count, not byte count — VM reference is
                    // `s.chars().count()` (interp/bytecode/builtins/list.rs
                    // builtin_len). strlen counts BYTES and diverges on any
                    // multi-byte UTF-8 (len("你好") == 2, not 6). Count UTF-8
                    // leading bytes inline (valid-UTF-8 string invariant).
                    let len = self.count_utf8_chars(pv, None)?;
                    Ok(len.into())
                } else {
                    // List struct { i64 len, i8* data }: read first field
                    let list_ty = self.list_struct_type();
                    let len_gep = self
                        .gep()
                        .build_struct_gep(list_ty, pv, 0, "list.len")
                        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                    let len = self
                        .builder
                        .build_load(self.context.i64_type(), len_gep, "len")
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                    Ok(len)
                }
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let fields = sv.get_type().get_field_types();
                // Distinguish string {i8*, i64} from list {i64, i8*} by field layout.
                let is_string_struct = matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(t)]
                        if t.get_bit_width() == 64
                );
                if is_string_struct {
                    // String struct {i8*, i64}: field 1 is the authoritative
                    // BYTE length. len() semantics = Unicode scalar values
                    // (VM: `s.chars().count()`), so count UTF-8 leading bytes
                    // BOUNDED by field 1 — never a NUL-terminated walk, which
                    // would truncate strings carrying embedded NUL bytes
                    // (e.g. f"a{chr(0)}b" must count 3). The bounded scan also
                    // stays correct on multi-byte UTF-8.
                    let data_ptr = self
                        .builder
                        .build_extract_value(sv, 0, "str_data_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?
                        .into_pointer_value();
                    let byte_len = self
                        .builder
                        .build_extract_value(sv, 1, "str_byte_len")
                        .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?
                        .into_int_value();
                    let len = self.count_utf8_chars(data_ptr, Some(byte_len))?;
                    Ok(len.into())
                } else {
                    // List struct {i64, i8*} passed as StructValue (e.g. from nested indexing).
                    // Extract field 0 (len) directly.
                    self.builder
                        .build_extract_value(sv, 0, "list_len")
                        .map_err(|e| CompileError::LlvmError(format!("extract list len: {}", e)))
                }
            }
            _ => Err(CompileError::TypeMismatch(
                "len expects a list or string pointer".to_string(),
            )),
        }
    }

    /// 0.1.9 Phase B (0.39.31): free builtin `is_empty(collection)`.
    /// 与 VM `builtin_is_empty`（List/String/Map/Set）对齐，但 codegen 仅实现
    /// List + String（LEN-READ-001 开放的核心场景）；Map/Set 是裸 i64 handle，
    /// 值层无法区分 → 保持 Unsupported（与现状一致，无回归）。
    pub(in crate::codegen) fn compile_is_empty(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "is_empty expects 1 argument".to_string(),
            ));
        }
        let zero_i64 = self.context.i64_type().const_zero();
        let cmp = |builder: &inkwell::builder::Builder<'ctx>,
                   len: inkwell::values::IntValue<'ctx>|
         -> MimiResult<inkwell::values::IntValue<'ctx>> {
            builder
                .build_int_compare(inkwell::IntPredicate::EQ, len, zero_i64, "is_empty")
                .map_err(|e| CompileError::LlvmError(format!("is_empty cmp: {e}")))
        };
        match args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => {
                if self.pending_len_is_string {
                    // C-string: empty iff first byte is NUL.
                    let byte = self
                        .builder
                        .build_load(self.context.i8_type(), pv, "str_first_byte")
                        .map_err(|e| CompileError::LlvmError(format!("str load: {e}")))?
                        .into_int_value();
                    let empty = cmp(&self.builder, byte)?;
                    Ok(empty.into())
                } else {
                    // List struct {i64 len, i8* data}: empty iff field 0 == 0.
                    let list_ty = self.list_struct_type();
                    let len_gep = self
                        .gep()
                        .build_struct_gep(list_ty, pv, 0, "list.len")
                        .map_err(|e| CompileError::LlvmError(format!("gep: {e}")))?;
                    let len = self
                        .builder
                        .build_load(self.context.i64_type(), len_gep, "len")
                        .map_err(|e| CompileError::LlvmError(format!("load: {e}")))?
                        .into_int_value();
                    let empty = cmp(&self.builder, len)?;
                    Ok(empty.into())
                }
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let fields = sv.get_type().get_field_types();
                // String {i8*, i64} vs List {i64, i8*} discriminated by layout.
                let is_string_struct = matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(t)]
                        if t.get_bit_width() == 64
                );
                if is_string_struct {
                    let byte_len = self
                        .builder
                        .build_extract_value(sv, 1, "str_byte_len")
                        .map_err(|e| CompileError::LlvmError(format!("extract: {e}")))?
                        .into_int_value();
                    let empty = cmp(&self.builder, byte_len)?;
                    Ok(empty.into())
                } else {
                    let len = self
                        .builder
                        .build_extract_value(sv, 0, "list_len")
                        .map_err(|e| CompileError::LlvmError(format!("extract list len: {e}")))?
                        .into_int_value();
                    let empty = cmp(&self.builder, len)?;
                    Ok(empty.into())
                }
            }
            BasicMetadataValueEnum::IntValue(handle) => {
                // Map and set values both lower to bare i64 handles. The call
                // site set pending_is_empty_kind from the inferred type:
                //   "map" -> mimi_map_size (map_new() -> Record handle)
                //   "set" -> mimi_set_size ({...} set literal handle)
                // Anything else is genuinely type-ambiguous → fail closed.
                let runtime_name = match self.pending_is_empty_kind {
                    Some("map") => "mimi_map_size",
                    Some("set") => "mimi_set_size",
                    _ => {
                        return Err(CompileError::Unsupported(
                            "is_empty: bare i64 handle with no Map/Set type hint                              (call-site type classification failed)"
                                .into(),
                        ))
                    }
                };
                let func = self
                    .module
                    .get_function(runtime_name)
                    .ok_or_else(|| format!("{runtime_name} not declared"))?;
                let result = self
                    .builder
                    .build_call(
                        func,
                        &[BasicMetadataValueEnum::IntValue(handle)],
                        "handle_size_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("handle_size call: {e}")))?;
                let size = result
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("size helper returned void".into()))?
                    .into_int_value();
                let empty = cmp(&self.builder, size)?;
                Ok(empty.into())
            }
            _ => Err(CompileError::TypeMismatch(
                "is_empty expects a list or string".to_string(),
            )),
        }
    }

    pub(in crate::codegen) fn compile_contains(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "contains expects 2 arguments".to_string(),
            ));
        }
        let list_ptr = self.require_list_pointer(args[0], "contains")?;
        let elem_val = args[1];
        let i64_ty = self.context.i64_type();
        // Get list length and data
        let list_len = self.load_list_len(list_ptr)?;
        // Determine whether we are comparing strings by looking at the element value.
        let elem_basic = match elem_val {
            BasicMetadataValueEnum::PointerValue(pv) => pv.into(),
            BasicMetadataValueEnum::StructValue(sv) => sv.into(),
            BasicMetadataValueEnum::IntValue(iv) => iv.into(),
            BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
            _ => {
                return Err(CompileError::TypeMismatch(
                    "contains: unsupported element type".to_string(),
                ))
            }
        };
        let target_str_ptr = self.extract_string_ptr(&elem_basic);
        let is_string = target_str_ptr.is_some();
        // Loop through list elements
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for contains loop".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "contains_loop");
        let body_bb = self.context.append_basic_block(function, "contains_body");
        let found_bb = self.context.append_basic_block(function, "contains_found");
        let done_bb = self.context.append_basic_block(function, "contains_done");
        let idx_alloca = self
            .builder
            .build_alloca(i64_ty, "ci")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, i64_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(loop_bb);
        let idx = self
            .builder
            .build_load(BasicTypeEnum::IntType(i64_ty), idx_alloca, "idx")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let cmp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, idx, list_len, "cmp")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.builder
            .build_conditional_branch(cmp, body_bb, done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(body_bb);
        let eq = if is_string {
            // String list: elements are `MimiStr` fat boxes (`i8*` to a
            // {magic, ptr, len} allocation) since 0.38.26. Unbox before
            // comparing with the target C string pointer.
            let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let data_raw = self.load_list_data_raw(list_ptr)?;
            let elem_ptr_ptr = self
                .gep()
                .build_in_bounds_gep(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    data_raw,
                    &[idx],
                    "elem_ptr_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let elem_box_ptr = self
                .builder
                .build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    elem_ptr_ptr,
                    "elem_box",
                )
                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                .into_pointer_value();
            let elem_box_i64 = self
                .builder
                .build_ptr_to_int(elem_box_ptr, i64_ty, "elem_box_int")
                .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?;
            let out_ptr_alloca = self
                .builder
                .build_alloca(i8_ptr_ty, "contains_str_out_ptr")
                .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
            let out_len_alloca = self
                .builder
                .build_alloca(i64_ty, "contains_str_out_len")
                .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
            self.build_call(
                self.get_runtime_fn("mimi_str_unbox")?,
                &[
                    BasicMetadataValueEnum::IntValue(elem_box_i64),
                    BasicMetadataValueEnum::PointerValue(out_ptr_alloca),
                    BasicMetadataValueEnum::PointerValue(out_len_alloca),
                ],
                "contains_str_unbox",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_unbox returned void")?;
            let elem_str_ptr = self
                .builder
                .build_load(i8_ptr_ty, out_ptr_alloca, "elem_str")
                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
                .into_pointer_value();
            let strcmp_fn = self.get_runtime_fn("strcmp")?;
            let cmp_result = self
                .build_call(
                    strcmp_fn,
                    &[
                        BasicMetadataValueEnum::PointerValue(target_str_ptr.ok_or_else(|| {
                            CompileError::TypeMismatch(
                                "contains: missing string target pointer".to_string(),
                            )
                        })?),
                        BasicMetadataValueEnum::PointerValue(elem_str_ptr),
                    ],
                    "strcmp_contains",
                )?
                .try_as_basic_value_opt()
                .ok_or("strcmp returned void")?
                .into_int_value();
            let zero = self.context.i32_type().const_int(0, false);
            self.builder
                .build_int_compare(inkwell::IntPredicate::EQ, cmp_result, zero, "streq")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
        } else {
            let data_ptr = self.load_list_data_i64(list_ptr)?;
            let elem_ptr = self
                .gep()
                .build_in_bounds_gep(i64_ty, data_ptr, &[idx], "elem")
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let elem = self
                .builder
                .build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, "elem_val")
                .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
            match (elem, elem_val) {
                (BasicValueEnum::IntValue(a), BasicMetadataValueEnum::IntValue(b)) => {
                    // List elements are stored as i64; extend search value to i64 if narrower.
                    let b_i64 = if b.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_s_extend(b, i64_ty, "search_sext")
                            .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
                    } else {
                        b
                    };
                    self.builder
                        .build_int_compare(inkwell::IntPredicate::EQ, a, b_i64, "eq")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                }
                (BasicValueEnum::IntValue(a), BasicMetadataValueEnum::FloatValue(b)) => {
                    // List<f64> slots are stored as i64 bit patterns; decode
                    // the loaded slot to f64 and compare numerically (VM uses
                    // `==` semantics: -0.0 == 0.0, NaN != anything).
                    let a_f64 = self
                        .build_bit_cast(
                            BasicValueEnum::IntValue(a),
                            BasicTypeEnum::FloatType(self.context.f64_type()),
                            "contains_elem_f64",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
                        .into_float_value();
                    self.builder
                        .build_float_compare(inkwell::FloatPredicate::OEQ, a_f64, b, "f64_eq")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                }
                _ => {
                    return Err(CompileError::TypeMismatch(
                        "contains: element comparison only supports i64/f64 strings for now"
                            .to_string(),
                    ))
                }
            }
        };
        let inc_bb = self.context.append_basic_block(function, "contains_inc");
        self.builder
            .build_conditional_branch(eq, found_bb, inc_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Next iteration
        self.builder.position_at_end(inc_bb);
        let next = self
            .builder
            .build_int_add(idx, i64_ty.const_int(1, false), "next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.builder
            .build_store(idx_alloca, next)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.builder
            .build_unconditional_branch(loop_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Found
        self.builder.position_at_end(found_bb);
        self.builder
            .build_unconditional_branch(done_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Done: phi(true, false) — i1 (bool): the checker infers `bool` for
        // contains; i64 made native print "1" vs VM "true" (L1 divergence).
        self.builder.position_at_end(done_bb);
        let phi = self
            .builder
            .build_phi(self.context.bool_type(), "result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        phi.add_incoming(&[
            (&self.context.bool_type().const_int(1, false), found_bb),
            (&self.context.bool_type().const_int(0, false), loop_bb),
        ]);
        Ok(phi.as_basic_value())
    }
}
