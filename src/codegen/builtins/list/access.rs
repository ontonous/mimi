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
                    // String: use strlen
                    let strlen_fn = self
                        .module
                        .get_function("strlen")
                        .ok_or_else(|| "strlen not declared".to_string())?;
                    let len = self
                        .builder
                        .build_call(
                            strlen_fn,
                            &[BasicMetadataValueEnum::PointerValue(pv)],
                            "strlen",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strlen returned void")?;
                    Ok(len)
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
                    // String struct {i8*, i64} — field 1 is the length directly
                    self.builder
                        .build_extract_value(sv, 1, "str_len")
                        .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))
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
            // String list: elements are `i8*` C-string pointers.
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
            let elem_str_ptr = self
                .builder
                .build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    elem_ptr_ptr,
                    "elem_str",
                )
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
                (BasicValueEnum::IntValue(a), BasicMetadataValueEnum::IntValue(b)) => self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, a, b, "eq")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
                _ => {
                    return Err(CompileError::TypeMismatch(
                        "contains: element comparison only supports i64 for now".to_string(),
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
        // Done: phi(true, false)
        self.builder.position_at_end(done_bb);
        let phi = self
            .builder
            .build_phi(i64_ty, "result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        phi.add_incoming(&[
            (&i64_ty.const_int(1, false), found_bb),
            (&i64_ty.const_int(0, false), loop_bb),
        ]);
        Ok(phi.as_basic_value())
    }
}
