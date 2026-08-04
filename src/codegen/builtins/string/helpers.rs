use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};

impl<'ctx> CodeGenerator<'ctx> {
    /// Count Unicode scalar values (chars) in a UTF-8 byte buffer —
    /// codegen counterpart of the Bytecode VM's `s.chars().count()`.
    ///
    /// Emits an inline loop counting UTF-8 *leading bytes*, i.e. bytes `b`
    /// with `(b & 0xC0) != 0x80` (continuation bytes are skipped). Two exit
    /// conditions are supported:
    /// - `bound == None`: scan until the NUL terminator (raw C strings).
    /// - `bound == Some(n)`: scan exactly `n` bytes starting at `data_ptr`
    ///   (prefix counting, e.g. byte-offset → char-index conversion for
    ///   `str_index_of`).
    ///
    /// Valid-UTF-8 assumption (language string invariant, debug_assert
    /// style): every Mimi string is produced by the lexer or runtime as
    /// valid UTF-8. Under that invariant this equals `chars().count()`;
    /// invalid UTF-8 would yield a miscount, never UB.
    pub(in crate::codegen) fn count_utf8_chars(
        &self,
        data_ptr: PointerValue<'ctx>,
        bound: Option<IntValue<'ctx>>,
    ) -> MimiResult<IntValue<'ctx>> {
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let zero = i64_ty.const_int(0, false);
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for utf8 char count".to_string())?;
        let loop_bb = self.context.append_basic_block(function, "u8c_loop");
        let body_bb = self.context.append_basic_block(function, "u8c_body");
        let done_bb = self.context.append_basic_block(function, "u8c_done");

        let i_alloca = self.build_alloca(i64_ty, "u8c_i")?;
        let count_alloca = self.build_alloca(i64_ty, "u8c_count")?;
        self.build_store(i_alloca, zero)?;
        self.build_store(count_alloca, zero)?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(loop_bb);
        let i = self
            .build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "u8c_i_val")?
            .into_int_value();
        match bound {
            Some(n) => {
                // Bounded scan: exit when i >= bound.
                let cont = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, i, n, "u8c_cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.build_cond_br(cont, body_bb, done_bb)?;
            }
            None => {
                // NUL-terminated scan: exit when the current byte is NUL.
                let p = self.build_in_bounds_gep(i8_ty, data_ptr, &[i], "u8c_ptr")?;
                let b = self
                    .build_load(BasicTypeEnum::IntType(i8_ty), p, "u8c_byte")?
                    .into_int_value();
                let is_nul = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        b,
                        i8_ty.const_int(0, false),
                        "u8c_nul",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                self.build_cond_br(is_nul, done_bb, body_bb)?;
            }
        }

        self.builder.position_at_end(body_bb);
        let i_b = self
            .build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "u8c_i_val2")?
            .into_int_value();
        let p_b = self.build_in_bounds_gep(i8_ty, data_ptr, &[i_b], "u8c_ptr2")?;
        let b = self
            .build_load(BasicTypeEnum::IntType(i8_ty), p_b, "u8c_byte2")?
            .into_int_value();
        // Leading byte test: (b & 0xC0) != 0x80 (bitwise — i8 signedness irrelevant).
        let masked = self
            .builder
            .build_and(b, i8_ty.const_int(0xC0, false), "u8c_mask")
            .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;
        let is_cont = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                masked,
                i8_ty.const_int(0x80, false),
                "u8c_cont",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_lead = self
            .builder
            .build_not(is_cont, "u8c_lead")
            .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?;
        let inc = self
            .builder
            .build_int_z_extend(is_lead, i64_ty, "u8c_inc")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        let count = self
            .build_load(
                BasicTypeEnum::IntType(i64_ty),
                count_alloca,
                "u8c_count_val",
            )?
            .into_int_value();
        let new_count = self
            .builder
            .build_int_add(count, inc, "u8c_new_count")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(count_alloca, new_count)?;
        let next = self
            .builder
            .build_int_add(i_b, i64_ty.const_int(1, false), "u8c_next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(i_alloca, next)?;
        self.build_br(loop_bb)?;

        self.builder.position_at_end(done_bb);
        let result = self.build_load(BasicTypeEnum::IntType(i64_ty), count_alloca, "u8c_result")?;
        Ok(result.into_int_value())
    }

    pub(in crate::codegen) fn compile_str_to_c_str(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Extract the raw C string pointer from a Mimi string
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_to_c_str expects 1 argument".to_string(),
            ));
        }
        let c_ptr = self.extract_raw_str_ptr(&args[0])?;
        Ok(c_ptr.into())
    }

    pub(in crate::codegen) fn compile_c_str_to_string(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Wrap a raw C string pointer into a Mimi string struct {i8*, i64}
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "c_str_to_string expects 1 argument".to_string(),
            ));
        }
        let raw_ptr = match args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => pv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "c_str_to_string: argument must be a raw C string pointer".to_string(),
                ))
            }
        };
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self
            .builder
            .build_alloca(string_ty, "cstr_str")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(ptr_gep, raw_ptr)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                "strlen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?;
        self.builder
            .build_store(len_gep, str_len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(str_alloca.into())
    }
}
