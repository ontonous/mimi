mod format;
mod helpers;
mod query;
mod transform;

use crate::codegen::CallSiteValueExt;
use crate::codegen::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FloatValue, IntValue, PointerValue};

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_str_char_at(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "str_char_at expects 2 arguments".to_string(),
            ));
        }
        let index = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "str_char_at: second arg must be integer index".to_string(),
                ))
            }
        };
        // Handle both string representations:
        // - PointerValue: char* directly (literal strings)
        // - StructValue: {i8*, i64} (builtin function results)
        // 2026-08-06 (audit 1): layout-checked extraction — a `List` `{i64,ptr}`
        // struct used to reach `into_pointer_value()` and panic the compiler.
        let (data_ptr, byte_len) = self.extract_string_arg_ptr_len(&args[0], "str_char_at")?;
        // CG-H1: Unicode scalar indexing via runtime (matches interpreter).
        let i64_ty = self.context.i64_type();
        let index_i64 = if index.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(index, i64_ty, "idx_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            index
        };
        let char_at_fn = self.get_runtime_fn("mimi_str_char_at_ll")?;
        let raw_result = self
            .build_call(
                char_at_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(data_ptr),
                    BasicMetadataValueEnum::IntValue(byte_len),
                    BasicMetadataValueEnum::IntValue(index_i64),
                ],
                "str_char_at_call",
            )?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_char_at returned void")?
            .into_pointer_value();
        self.register_heap_alloc(raw_result);
        self.wrap_c_string(raw_result)
    }
    pub(super) fn compile_char_code(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "char_code expects 2 arguments".to_string(),
            ));
        }
        let index = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "char_code: second arg must be integer index".to_string(),
                ))
            }
        };
        // Handle both string representations:
        // - PointerValue: char* directly (literal strings)
        // - StructValue: {i8*, i64} (builtin function results)
        let (data_ptr, byte_len_opt) = match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => {
                // Literal string: pv is already a char*
                (*pv, None)
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                // Builtin string struct {i8*, i64}: extract field 0 (data
                // pointer) and field 1 (explicit byte length). Guard against
                // struct layouts whose first field is not a pointer
                // (e.g. List<i32> is {i64, ptr}); the checker accepts these
                // calls today and previously this produced a Rust panic in
                // into_pointer_value().
                let field0 = self
                    .builder
                    .build_extract_value(*sv, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?;
                let field1 = self
                    .builder
                    .build_extract_value(*sv, 1, "str_len")
                    .map_err(|e| CompileError::LlvmError(format!("extract str len: {}", e)))?;
                match field0 {
                    BasicValueEnum::PointerValue(pv) => {
                        let len = match field1 {
                            BasicValueEnum::IntValue(iv) => iv,
                            _ => {
                                return Err(CompileError::TypeMismatch(
                                    "char_code: string struct length field must be i64".to_string(),
                                ))
                            }
                        };
                        (pv, Some(len))
                    }
                    _ => {
                        return Err(CompileError::TypeMismatch(
                            "char_code: first arg must be a string, not a list/struct".to_string(),
                        ))
                    }
                }
            }
            _ => {
                return Err(CompileError::TypeMismatch(
                    "char_code: first arg must be string".to_string(),
                ))
            }
        };
        // Audit fix (full-audit-2026-08-05): the old code indexed RAW BYTES
        // and clamped OOB to code 0 — silent wrongness. The VM reference is
        // char-indexed (`s.chars().nth(i)`) and ERRORS out of bounds
        // (interp/bytecode/builtins/string.rs builtin_char_code). Walk UTF-8
        // leading bytes to the requested scalar index, decode the sequence,
        // and trap on OOB with the VM-style message.
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let i32_ty = self.context.i32_type();
        let zero = i64_ty.const_int(0, false);
        // Normalize the index to i64 (i32 indices from literals/methods).
        let index64 = if index.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(index, i64_ty, "cc_idx_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            index
        };
        // OOB gate: index < 0 || index >= char_count.
        let n_chars = self.count_utf8_chars(data_ptr, byte_len_opt)?;
        let neg = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, index64, zero, "cc_neg")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let over = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGE, index64, n_chars, "cc_over")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let oob = self
            .builder
            .build_or(neg, over, "cc_oob")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for char_code".to_string())?;
        let trap_bb = self.context.append_basic_block(function, "cc_trap");
        let walk_bb = self.context.append_basic_block(function, "cc_walk");
        self.builder
            .build_conditional_branch(oob, trap_bb, walk_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        // Trap block (VM-style message).
        self.builder.position_at_end(trap_bb);
        self.emit_trap_with_int_message("char_code: index %ld out of bounds", index64, "cc")?;

        // Walk block: skip `index64` chars (leading byte + continuations).
        self.builder.position_at_end(walk_bb);
        let i_alloca = self.build_alloca(i64_ty, "cc_i")?;
        let pos_alloca = self.build_alloca(i64_ty, "cc_pos")?;
        self.build_store(i_alloca, zero)?;
        self.build_store(pos_alloca, zero)?;
        let loop_bb = self.context.append_basic_block(function, "cc_loop");
        let step_bb = self.context.append_basic_block(function, "cc_step");
        let decode_bb = self.context.append_basic_block(function, "cc_decode");
        self.build_br(loop_bb)?;
        self.builder.position_at_end(loop_bb);
        let i = self
            .build_load(BasicTypeEnum::IntType(i64_ty), i_alloca, "cc_i_val")?
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i, index64, "cc_cont")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(cont, step_bb, decode_bb)?;
        self.builder.position_at_end(step_bb);
        let pos = self
            .build_load(BasicTypeEnum::IntType(i64_ty), pos_alloca, "cc_pos_val")?
            .into_int_value();
        let b_ptr = self.build_in_bounds_gep(i8_ty, data_ptr, &[pos], "cc_byte_ptr")?;
        let b_raw = self
            .build_load(BasicTypeEnum::IntType(i8_ty), b_ptr, "cc_byte")?
            .into_int_value();
        let b = self
            .builder
            .build_int_z_extend(b_raw, i32_ty, "cc_byte_z")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        // Char width from the leading byte (valid-UTF-8 invariant: a char
        // head is never a continuation byte).
        let w4 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGE,
                b,
                i32_ty.const_int(0xF0, false),
                "cc_w4",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let w3 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGE,
                b,
                i32_ty.const_int(0xE0, false),
                "cc_w3",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let w2 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGE,
                b,
                i32_ty.const_int(0xC0, false),
                "cc_w2",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // Char width from the leading byte: width = w4 ? 4 : (w3 ? 3 : (w2 ? 2 : 1)).
        // (valid-UTF-8 invariant: a char head is never a continuation byte).
        // Central fix 2026-08-05: the previous chain select(w3,3,4)→select(w2,2,·)
        // →select(w4,4,·) yielded 4 for ASCII (all flags false) and 2 for 3-byte
        // heads, corrupting every walk. Build the chain from the 1-byte base up.
        let width_12 = self
            .builder
            .build_select(
                w2,
                i32_ty.const_int(2, false),
                i32_ty.const_int(1, false),
                "cc_w12",
            )
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let width_123 = self
            .builder
            .build_select(w3, i32_ty.const_int(3, false), width_12, "cc_w123")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let width = self
            .builder
            .build_select(w4, i32_ty.const_int(4, false), width_123, "cc_width")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let width64 = self
            .builder
            .build_int_z_extend(width, i64_ty, "cc_width_z")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        let pos_next = self
            .builder
            .build_int_add(pos, width64, "cc_pos_next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(pos_alloca, pos_next)?;
        let i_next = self
            .builder
            .build_int_add(i, i64_ty.const_int(1, false), "cc_i_next")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        self.build_store(i_alloca, i_next)?;
        self.build_br(loop_bb)?;

        // Decode block: branch on the leading byte and load ONLY the
        // continuation bytes the sequence actually has (never read past the
        // string's NUL terminator).
        self.builder.position_at_end(decode_bb);
        let pos_d = self
            .build_load(BasicTypeEnum::IntType(i64_ty), pos_alloca, "cc_pos_dec")?
            .into_int_value();
        let p0 = self.build_in_bounds_gep(i8_ty, data_ptr, &[pos_d], "cc_dec_ptr")?;
        let b0 = self
            .builder
            .build_int_z_extend(
                self.build_load(BasicTypeEnum::IntType(i8_ty), p0, "cc_b0")?
                    .into_int_value(),
                i32_ty,
                "cc_b0_z",
            )
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        let d1_bb = self.context.append_basic_block(function, "cc_d1");
        let chk2_bb = self.context.append_basic_block(function, "cc_chk2");
        let d2_bb = self.context.append_basic_block(function, "cc_d2");
        let chk3_bb = self.context.append_basic_block(function, "cc_chk3");
        let d3_bb = self.context.append_basic_block(function, "cc_d3");
        let d4_bb = self.context.append_basic_block(function, "cc_d4");
        let merge_bb = self.context.append_basic_block(function, "cc_merge");
        let lt_80 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                b0,
                i32_ty.const_int(0x80, false),
                "cc_lt80",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(lt_80, d1_bb, chk2_bb)?;
        self.builder.position_at_end(chk2_bb);
        let lt_e0 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                b0,
                i32_ty.const_int(0xE0, false),
                "cc_ltE0",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(lt_e0, d2_bb, chk3_bb)?;
        self.builder.position_at_end(chk3_bb);
        let lt_f0 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                b0,
                i32_ty.const_int(0xF0, false),
                "cc_ltF0",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        self.build_cond_br(lt_f0, d3_bb, d4_bb)?;
        // 1-byte (ASCII): cp = b0
        self.builder.position_at_end(d1_bb);
        self.build_br(merge_bb)?;
        // Continuation-byte loader: data_ptr[pos_d + off], zext to i32.
        let load_cont = |off: u64, name: &str| -> MimiResult<inkwell::values::IntValue<'ctx>> {
            let off_idx = self.context.i64_type().const_int(off, false);
            let p_off = self
                .builder
                .build_int_add(pos_d, off_idx, &format!("{}_off", name))
                .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
            let p = self.build_in_bounds_gep(i8_ty, data_ptr, &[p_off], name)?;
            let raw = self
                .build_load(BasicTypeEnum::IntType(i8_ty), p, &format!("{}_val", name))?
                .into_int_value();
            self.builder
                .build_int_z_extend(raw, i32_ty, &format!("{}_z", name))
                .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))
        };
        // 2-byte: cp = ((b0 & 0x1F) << 6) | (b1 & 0x3F)
        self.builder.position_at_end(d2_bb);
        let b1 = load_cont(1, "cc_b1")?;
        let cp2_hi = self
            .builder
            .build_left_shift(
                self.builder
                    .build_and(b0, i32_ty.const_int(0x1F, false), "cc_b0_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                i32_ty.const_int(6, false),
                "cc_cp2_hi",
            )
            .map_err(|e| CompileError::LlvmError(format!("shl error: {}", e)))?;
        let cp2 = self
            .builder
            .build_or(
                cp2_hi,
                self.builder
                    .build_and(b1, i32_ty.const_int(0x3F, false), "cc_b1_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                "cc_cp2",
            )
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        self.build_br(merge_bb)?;
        // 3-byte: cp = ((b0 & 0x0F) << 12) | ((b1 & 0x3F) << 6) | (b2 & 0x3F)
        self.builder.position_at_end(d3_bb);
        let b1 = load_cont(1, "cc_b1")?;
        let b2 = load_cont(2, "cc_b2")?;
        let cp3_hi = self
            .builder
            .build_left_shift(
                self.builder
                    .build_and(b0, i32_ty.const_int(0x0F, false), "cc_b0_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                i32_ty.const_int(12, false),
                "cc_cp3_hi",
            )
            .map_err(|e| CompileError::LlvmError(format!("shl error: {}", e)))?;
        let cp3_mid = self
            .builder
            .build_left_shift(
                self.builder
                    .build_and(b1, i32_ty.const_int(0x3F, false), "cc_b1_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                i32_ty.const_int(6, false),
                "cc_cp3_mid",
            )
            .map_err(|e| CompileError::LlvmError(format!("shl error: {}", e)))?;
        let cp3 = self
            .builder
            .build_or(
                self.builder
                    .build_or(cp3_hi, cp3_mid, "cc_cp3_hm")
                    .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?,
                self.builder
                    .build_and(b2, i32_ty.const_int(0x3F, false), "cc_b2_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                "cc_cp3",
            )
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        self.build_br(merge_bb)?;
        // 4-byte: cp = ((b0 & 0x07) << 18) | ((b1 & 0x3F) << 12)
        //              | ((b2 & 0x3F) << 6) | (b3 & 0x3F)
        self.builder.position_at_end(d4_bb);
        let b1 = load_cont(1, "cc_b1")?;
        let b2 = load_cont(2, "cc_b2")?;
        let b3 = load_cont(3, "cc_b3")?;
        let cp4_hi = self
            .builder
            .build_left_shift(
                self.builder
                    .build_and(b0, i32_ty.const_int(0x07, false), "cc_b0_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                i32_ty.const_int(18, false),
                "cc_cp4_hi",
            )
            .map_err(|e| CompileError::LlvmError(format!("shl error: {}", e)))?;
        let cp4_m1 = self
            .builder
            .build_left_shift(
                self.builder
                    .build_and(b1, i32_ty.const_int(0x3F, false), "cc_b1_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                i32_ty.const_int(12, false),
                "cc_cp4_m1",
            )
            .map_err(|e| CompileError::LlvmError(format!("shl error: {}", e)))?;
        let cp4_m2 = self
            .builder
            .build_left_shift(
                self.builder
                    .build_and(b2, i32_ty.const_int(0x3F, false), "cc_b2_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                i32_ty.const_int(6, false),
                "cc_cp4_m2",
            )
            .map_err(|e| CompileError::LlvmError(format!("shl error: {}", e)))?;
        let cp4 = self
            .builder
            .build_or(
                self.builder
                    .build_or(
                        self.builder
                            .build_or(cp4_hi, cp4_m1, "cc_cp4_h1")
                            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?,
                        cp4_m2,
                        "cc_cp4_h2",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?,
                self.builder
                    .build_and(b3, i32_ty.const_int(0x3F, false), "cc_b3_m")
                    .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?,
                "cc_cp4",
            )
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        self.build_br(merge_bb)?;
        // Merge: phi over the four decode paths, then zext to i64.
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(i32_ty, "cc_cp")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        phi.add_incoming(&[
            (&BasicValueEnum::IntValue(b0), d1_bb),
            (&BasicValueEnum::IntValue(cp2), d2_bb),
            (&BasicValueEnum::IntValue(cp3), d3_bb),
            (&BasicValueEnum::IntValue(cp4), d4_bb),
        ]);
        let cp = phi.as_basic_value().into_int_value();
        let result = self
            .builder
            .build_int_z_extend(cp, i64_ty, "char_code_ext")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    pub(super) fn compile_chr(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "chr expects 1 argument".to_string(),
            ));
        }
        let code = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "chr: first arg must be integer code point".to_string(),
                ))
            }
        };
        // Audit fix (full-audit-2026-08-05): the old code truncated the code
        // point to i8 (single byte) — silently corrupting every code point
        // above 255. Full UTF-8 encoding now mirrors the VM reference
        // (interp/bytecode/builtins/string.rs builtin_chr):
        //   - validate 0..=0x10FFFF            → "chr: code point out of range: {}"
        //   - reject surrogates (char::from_u32) → "chr: invalid code point {}"
        //   - encode 1–4 bytes into a malloc'd string.
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let i8_ty = self.context.i8_type();
        let zero = i64_ty.const_int(0, false);
        // Normalize to i64 (callers may pass i32 literals).
        let code64 = if code.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(code, i64_ty, "chr_code_sext")
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            code
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for chr".to_string())?;

        // 1) Range validation: 0..=0x10FFFF.
        let is_neg = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, code64, zero, "chr_neg")
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let too_big = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                code64,
                i64_ty.const_int(0x10FFFF, false),
                "chr_big",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let bad_range = self
            .builder
            .build_or(is_neg, too_big, "chr_bad_range")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let range_trap_bb = self.context.append_basic_block(function, "chr_range_trap");
        let surr_chk_bb = self.context.append_basic_block(function, "chr_surr_chk");
        self.builder
            .build_conditional_branch(bad_range, range_trap_bb, surr_chk_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(range_trap_bb);
        self.emit_trap_with_int_message("chr: code point out of range: %ld", code64, "chr_rng")?;

        // 2) Surrogate rejection (char::from_u32 fails on U+D800..=U+DFFF).
        self.builder.position_at_end(surr_chk_bb);
        let ge_d800 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGE,
                code64,
                i64_ty.const_int(0xD800, false),
                "chr_ge_d800",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let le_dfff = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLE,
                code64,
                i64_ty.const_int(0xDFFF, false),
                "chr_le_dfff",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_surr = self
            .builder
            .build_and(ge_d800, le_dfff, "chr_is_surr")
            .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;
        let surr_trap_bb = self.context.append_basic_block(function, "chr_surr_trap");
        let encode_bb = self.context.append_basic_block(function, "chr_encode");
        self.builder
            .build_conditional_branch(is_surr, surr_trap_bb, encode_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(surr_trap_bb);
        self.emit_trap_with_int_message("chr: invalid code point %ld", code64, "chr_sur")?;

        // 3) UTF-8 encode (mirrors Rust's char encoding, branched on ranges).
        self.builder.position_at_end(encode_bb);
        let cp = self
            .builder
            .build_int_truncate(code64, i32_ty, "chr_cp")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
        // Encoded length: cp < 0x80 → 1; < 0x800 → 2; < 0x10000 → 3; else 4.
        let lt_80 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                cp,
                i32_ty.const_int(0x80, false),
                "chr_lt80",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let lt_800 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                cp,
                i32_ty.const_int(0x800, false),
                "chr_lt800",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let lt_10000 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::ULT,
                cp,
                i32_ty.const_int(0x10000, false),
                "chr_lt10000",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let len_34 = self
            .builder
            .build_select(
                lt_10000,
                i32_ty.const_int(3, false),
                i32_ty.const_int(4, false),
                "chr_l34",
            )
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let len_234 = self
            .builder
            .build_select(lt_800, i32_ty.const_int(2, false), len_34, "chr_l234")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let len32 = self
            .builder
            .build_select(lt_80, i32_ty.const_int(1, false), len_234, "chr_len")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let len64 = self
            .builder
            .build_int_z_extend(len32, i64_ty, "chr_len_z")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        let is_len2 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                len32,
                i32_ty.const_int(2, false),
                "chr_islen2",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_len3 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                len32,
                i32_ty.const_int(3, false),
                "chr_islen3",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let is_len4 = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                len32,
                i32_ty.const_int(4, false),
                "chr_islen4",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        // Continuation-byte right shifts: len2 → 0, len3 → 6, len4 → 12.
        let sh1_23 = self
            .builder
            .build_select(
                is_len3,
                i32_ty.const_int(6, false),
                i32_ty.const_int(12, false),
                "chr_sh1_23",
            )
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let sh1 = self
            .builder
            .build_select(is_len2, i32_ty.const_int(0, false), sh1_23, "chr_sh1")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let sh2 = self
            .builder
            .build_select(
                is_len3,
                i32_ty.const_int(0, false),
                i32_ty.const_int(6, false),
                "chr_sh2",
            )
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // Leading byte per length: 1 → cp; 2 → 0xC0|(cp>>6);
        // 3 → 0xE0|(cp>>12); 4 → 0xF0|(cp>>18).
        let b0_2 = self
            .builder
            .build_or(
                i32_ty.const_int(0xC0, false),
                self.builder
                    .build_right_shift(cp, i32_ty.const_int(6, false), false, "chr_cp_s6")
                    .map_err(|e| CompileError::LlvmError(format!("shr error: {}", e)))?,
                "chr_b0_2",
            )
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let b0_3 = self
            .builder
            .build_or(
                i32_ty.const_int(0xE0, false),
                self.builder
                    .build_right_shift(cp, i32_ty.const_int(12, false), false, "chr_cp_s12")
                    .map_err(|e| CompileError::LlvmError(format!("shr error: {}", e)))?,
                "chr_b0_3",
            )
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let b0_4 = self
            .builder
            .build_or(
                i32_ty.const_int(0xF0, false),
                self.builder
                    .build_right_shift(cp, i32_ty.const_int(18, false), false, "chr_cp_s18")
                    .map_err(|e| CompileError::LlvmError(format!("shr error: {}", e)))?,
                "chr_b0_4",
            )
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        // len==1 stores the code point unchanged (ASCII); longer sequences
        // use their tagged leading byte.
        let b0_12 = self
            .builder
            .build_select(is_len2, b0_2, cp, "chr_b0_12")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let b0_123 = self
            .builder
            .build_select(is_len3, b0_3, b0_12, "chr_b0_123")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        let b0 = self
            .builder
            .build_select(is_len4, b0_4, b0_123, "chr_b0")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // Continuation bytes: 0x80 | ((cp >> shift) & 0x3F).
        let cont = |shift: inkwell::values::IntValue<'ctx>,
                    name: &str|
         -> MimiResult<inkwell::values::IntValue<'ctx>> {
            let shifted = self
                .builder
                .build_right_shift(cp, shift, false, &format!("{}_sh", name))
                .map_err(|e| CompileError::LlvmError(format!("shr error: {}", e)))?;
            let masked = self
                .builder
                .build_and(
                    shifted,
                    i32_ty.const_int(0x3F, false),
                    &format!("{}_m", name),
                )
                .map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?;
            self.builder
                .build_or(i32_ty.const_int(0x80, false), masked, name)
                .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))
        };
        let b1 = cont(sh1, "chr_b1")?;
        let b2 = cont(sh2, "chr_b2")?;
        let b3 = cont(i32_ty.const_int(0, false), "chr_b3")?;
        // Allocate 5 bytes: max 4 encoded + NUL (B4: NULL-checked malloc).
        let buf = self.malloc_or_abort(i64_ty.const_int(5, false), "chr_malloc")?;
        // Store all four slots (unused slots hold inert values — the NUL
        // below terminates the string at exactly `len` bytes).
        let bytes = [b0, b1, b2, b3];
        for (k, bv) in bytes.iter().enumerate() {
            let slot = self
                .gep()
                .build_in_bounds_gep(
                    BasicTypeEnum::IntType(i8_ty),
                    buf,
                    &[i64_ty.const_int(k as u64, false)],
                    &format!("chr_slot{}", k),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
            let byte = self
                .builder
                .build_int_truncate(*bv, i8_ty, &format!("chr_byte{}", k))
                .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;
            self.builder
                .build_store(slot, byte)
                .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        }
        // NUL terminator at buf[len] (len ∈ 1..=4, buf is 5 bytes).
        let null_gep = self
            .gep()
            .build_in_bounds_gep(BasicTypeEnum::IntType(i8_ty), buf, &[len64], "chr_nul")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(null_gep, i8_ty.const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        // Build string struct { i8*, i64 } with the ENCODED byte length.
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let str_alloca = self.build_entry_alloca(string_ty, "chr_str")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(ptr_gep, buf)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        self.register_heap_slot(str_alloca, string_ty, 0);
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(len_gep, len64)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let result = self
            .builder
            .build_load(
                BasicTypeEnum::StructType(string_ty),
                str_alloca,
                "chr_result",
            )
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
        Ok(result)
    }

    /// Trap with a STATIC message via `mimi_runtime_abort` (noreturn), then
    /// terminate the current block with `unreachable`. Callers must position
    /// the builder at a block that exists solely for this trap. Used by the
    /// fail-loud parse family (audit-wave2 §5.7): `to_int`/`to_float` on
    /// unparsable input mirror the VM's `Err("to_int parse error: …")`.
    fn emit_parse_trap(&self, message: &str, label: &str) -> MimiResult<()> {
        let abort_fn = self.get_or_declare_abort_fn();
        let msg = self
            .builder
            .build_global_string_ptr(message, &format!("{}_msg", label))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        self.builder
            .build_call(
                abort_fn,
                &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
                &format!("{}_abort", label),
            )
            .map_err(|e| CompileError::LlvmError(format!("abort error: {}", e)))?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        Ok(())
    }

    /// Trap with a printf-formatted message carrying one `%s` string value
    /// (same layout contract as `emit_trap_with_int_message`). Used for
    /// `to_float parse error: non-finite value '<input>'` where the message
    /// embeds the offending input string.
    fn emit_trap_with_str_message(
        &self,
        fmt: &str,
        s_ptr: PointerValue<'ctx>,
        name: &str,
    ) -> MimiResult<()> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i32_ty = self.context.i32_type();
        // 256-byte scratch message buffer (cold trap path); snprintf caps the
        // write, so over-long inputs truncate inside the message only.
        let buf = self.build_alloca(i64_ty.array_type(32), &format!("{}_msg", name))?;
        let fmt_global = self
            .builder
            .build_global_string_ptr(fmt, &format!("{}_fmt", name))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let snprintf_ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                snprintf_ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        self.builder
            .build_call(
                snprintf_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(256, false)),
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                ],
                &format!("{}_snprintf", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("snprintf error: {}", e)))?;
        let abort_fn = self.get_or_declare_abort_fn();
        self.builder
            .build_call(
                abort_fn,
                &[BasicMetadataValueEnum::PointerValue(buf)],
                &format!("{}_abort", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("abort error: {}", e)))?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        Ok(())
    }

    /// Trap with a printf-formatted message carrying one integer value.
    ///
    /// Emits `snprintf(buf, 128, fmt, value)` into a stack scratch buffer and
    /// calls `mimi_runtime_abort` (noreturn), then terminates the current
    /// block with `unreachable`. Callers must position the builder at a
    /// block that exists solely for this trap. VM-style messages (e.g.
    /// "chr: code point out of range: %ld") keep codegen diagnostics aligned
    /// with the Bytecode VM's error text.
    fn emit_trap_with_int_message(
        &self,
        fmt: &str,
        value: inkwell::values::IntValue<'ctx>,
        name: &str,
    ) -> MimiResult<()> {
        let i64_ty = self.context.i64_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i32_ty = self.context.i32_type();
        // 128-byte scratch message buffer (cold trap path — stack is fine).
        let buf = self.build_alloca(i64_ty.array_type(16), &format!("{}_msg", name))?;
        let fmt_global = self
            .builder
            .build_global_string_ptr(fmt, &format!("{}_fmt", name))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        // B3/CG-C3: snprintf returns i32 (not i8*); declare with the correct
        // variadic signature if the module lacks it.
        let snprintf_fn = self.module.get_function("snprintf").unwrap_or_else(|| {
            let snprintf_ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                true,
            );
            self.module.add_function(
                "snprintf",
                snprintf_ty,
                Some(inkwell::module::Linkage::External),
            )
        });
        let value64 = if value.get_type().get_bit_width() < 64 {
            self.builder
                .build_int_s_extend(value, i64_ty, &format!("{}_val_sext", name))
                .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
        } else {
            value
        };
        self.builder
            .build_call(
                snprintf_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(128, false)),
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                    BasicMetadataValueEnum::IntValue(value64),
                ],
                &format!("{}_snprintf", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("snprintf error: {}", e)))?;
        let abort_fn = self.get_or_declare_abort_fn();
        self.builder
            .build_call(
                abort_fn,
                &[BasicMetadataValueEnum::PointerValue(buf)],
                &format!("{}_abort", name),
            )
            .map_err(|e| CompileError::LlvmError(format!("abort error: {}", e)))?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;
        Ok(())
    }

    /// Parse a C string with strtol and return (ok, value).
    /// ok is true when at least one digit was consumed and the rest of the
    /// string is the null terminator (whole-string parse).
    fn emit_strtol(
        &self,
        s_ptr: PointerValue<'ctx>,
    ) -> MimiResult<(
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    )> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let strtol_fn = self
            .module
            .get_function("strtol")
            .or_else(|| {
                let ty = i64_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::IntType(self.context.i32_type()),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strtol",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strtol function".to_string())?;
        let endptr_alloca = self.build_alloca(i8_ptr, "strtol_endptr")?;
        self.build_store(endptr_alloca, i8_ptr.const_null())?;
        let call = self
            .builder
            .build_call(
                strtol_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(endptr_alloca),
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(10, false)),
                ],
                "strtol_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strtol error: {}", e)))?;
        let value = self.expect_basic_value(&call, "strtol")?.into_int_value();
        let endptr = self
            .builder
            .build_load(i8_ptr, endptr_alloca, "strtol_endptr_load")
            .map_err(|e| CompileError::LlvmError(format!("load endptr: {}", e)))?
            .into_pointer_value();
        let end_i = self
            .builder
            .build_ptr_to_int(endptr, i64_ty, "end_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint endptr: {}", e)))?;
        let s_i = self
            .builder
            .build_ptr_to_int(s_ptr, i64_ty, "s_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint s: {}", e)))?;
        let consumed = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, end_i, s_i, "strtol_consumed")
            .map_err(|e| CompileError::LlvmError(format!("icmp consumed: {}", e)))?;
        let end_byte = self
            .builder
            .build_load(BasicTypeEnum::IntType(i8_ty), endptr, "strtol_end_byte")
            .map_err(|e| CompileError::LlvmError(format!("load end byte: {}", e)))?
            .into_int_value();
        let end_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                end_byte,
                i8_ty.const_int(0, false),
                "strtol_end_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("icmp end null: {}", e)))?;
        let ok = self
            .builder
            .build_and(consumed, end_null, "strtol_ok")
            .map_err(|e| CompileError::LlvmError(format!("and ok: {}", e)))?;
        // 0.39.136 (L1): on failure the value field must be 0, matching the VM
        // (`s.parse::<i64>()` → (false, 0)). Without the select, the raw
        // strtol partial result leaked through: str_parse_int("7x").1 was 7
        // natively vs 0 in the VM.
        let value = self
            .builder
            .build_select(
                ok,
                value,
                i64_ty.const_int(0, false),
                "strtol_value_or_zero",
            )
            .map_err(|e| CompileError::LlvmError(format!("select value: {e}")))?
            .into_int_value();
        Ok((ok, value))
    }

    /// Build a (bool, i64) tuple value from a success flag and an i64.
    fn build_parse_int_tuple(
        &self,
        ok: inkwell::values::IntValue<'ctx>,
        value: inkwell::values::IntValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let tuple_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let alloca = self.build_alloca(tuple_ty, "parse_int_tuple")?;
        let ok_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 0, "parse_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(ok_gep, ok)?;
        let val_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 1, "parse_val")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(val_gep, value)?;
        self.build_load(tuple_ty, alloca, "parse_int_tuple_val")
    }

    /// Extract a C string pointer from a string argument (raw pointer or {ptr,len} struct).
    /// 2026-08-06 (audit 1): a non-string struct (e.g. `List` with `{i64, ptr}`
    /// layout) used to reach `into_pointer_value()` and PANIC the compiler —
    /// match the field types and fail loud instead (VM parity: E0800 runtime).
    fn extract_string_arg_ptr(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        caller: &str,
    ) -> MimiResult<PointerValue<'ctx>> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => Ok(*pv),
            BasicMetadataValueEnum::StructValue(sv) => {
                let ftys = sv.get_type().get_field_types();
                let is_str_layout = ftys.len() == 2
                    && matches!(ftys[0], BasicTypeEnum::PointerType(_))
                    && matches!(ftys[1], BasicTypeEnum::IntType(it) if it.get_bit_width() == 64);
                if !is_str_layout {
                    return Err(CompileError::TypeMismatch(format!(
                        "{}: first arg must be string, int, or float (found a non-string struct)",
                        caller
                    )));
                }
                let ptr = self
                    .builder
                    .build_extract_value(*sv, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?
                    .into_pointer_value();
                Ok(ptr)
            }
            _ => Err(CompileError::TypeMismatch(format!(
                "{}: first arg must be string, int, or float",
                caller
            ))),
        }
    }

    /// Saturating float->i64 conversion mirroring Rust's `f64 as i64`
    /// semantics, used for float operands of `to_int` / `str_parse_int`.
    ///
    /// LLVM `fptosi` is UNDEFINED BEHAVIOR for values outside the i64 range
    /// (and for NaN/Inf); at `-O2` the poison result miscompiles into a crash
    /// (AUD-2). The *safe* fix is to **never** call `fptosi` on an
    /// out-of-range / non-finite value: branch to a constant result for those
    /// cases and only emit `fptosi` on the provably in-range arm:
    ///   NaN            -> 0
    ///   +Inf / >= 2^63 -> i64::MAX
    ///   -Inf / <= -2^63 -> i64::MIN
    ///   finite in range -> `fptosi` (unchanged behavior for normal values)
    ///
    /// `i64::MAX as f64` rounds up to exactly 2^63.0, and there is no f64
    /// between i64::MAX (9.22e18) and 2^63, so the `>= 2^63` threshold
    /// captures every out-of-range-high float.
    fn build_saturating_float_to_signed_int(
        &mut self,
        fv: FloatValue<'ctx>,
    ) -> MimiResult<IntValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();
        let imax = i64_ty.const_int(i64::MAX as u64, false);
        let imin = i64_ty.const_int(i64::MIN as u64, false);
        let zero = i64_ty.const_int(0, false);
        // `i64::MAX as f64` == 2^63.0 (rounds up); `i64::MIN as f64` == -2^63.0.
        let thresh_hi = f64_ty.const_float(i64::MAX as f64);
        let thresh_lo = f64_ty.const_float(i64::MIN as f64);

        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("ftoi: no enclosing function".into()))?;

        // NaN: a value is UNOrdered with itself.
        let is_nan = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::UNE, fv, fv, "ftoi_nan")
            .map_err(|e| CompileError::LlvmError(format!("ftoi nan cmp: {}", e)))?;
        // Out-of-range high: >= 2^63 (covers +Inf).
        let ge_hi = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OGE, fv, thresh_hi, "ftoi_ge_hi")
            .map_err(|e| CompileError::LlvmError(format!("ftoi ge hi: {}", e)))?;
        // Out-of-range low: <= -2^63 (covers -Inf).
        let le_lo = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OLE, fv, thresh_lo, "ftoi_le_lo")
            .map_err(|e| CompileError::LlvmError(format!("ftoi le lo: {}", e)))?;

        // Cascade of basic blocks; `fptosi` is only ever reached on the
        // provably-in-range arm, so it can never produce UB/poison.
        let nan_bb = self.context.append_basic_block(function, "ftoi_nan");
        let range_bb = self.context.append_basic_block(function, "ftoi_range");
        let hi_bb = self.context.append_basic_block(function, "ftoi_hi");
        let lo_or_in_bb = self.context.append_basic_block(function, "ftoi_lo_or_in");
        let lo_bb = self.context.append_basic_block(function, "ftoi_lo");
        let in_bb = self.context.append_basic_block(function, "ftoi_in");
        let merge_bb = self.context.append_basic_block(function, "ftoi_merge");

        // is_nan ? nan : range
        self.builder
            .build_conditional_branch(is_nan, nan_bb, range_bb)
            .map_err(|e| CompileError::LlvmError(format!("ftoi br nan: {}", e)))?;
        // range: >= 2^63 ? hi : lo_or_in
        self.builder.position_at_end(range_bb);
        self.builder
            .build_conditional_branch(ge_hi, hi_bb, lo_or_in_bb)
            .map_err(|e| CompileError::LlvmError(format!("ftoi br hi: {}", e)))?;
        // lo_or_in: <= -2^63 ? lo : in (in range)
        self.builder.position_at_end(lo_or_in_bb);
        self.builder
            .build_conditional_branch(le_lo, lo_bb, in_bb)
            .map_err(|e| CompileError::LlvmError(format!("ftoi br lo: {}", e)))?;

        // nan_bb -> 0
        self.builder.position_at_end(nan_bb);
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("ftoi nan br: {}", e)))?;
        // hi_bb -> i64::MAX
        self.builder.position_at_end(hi_bb);
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("ftoi hi br: {}", e)))?;
        // lo_bb -> i64::MIN
        self.builder.position_at_end(lo_bb);
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("ftoi lo br: {}", e)))?;
        // in_bb -> fptosi(fv); fv is provably in range here, so no UB.
        self.builder.position_at_end(in_bb);
        let in_val = self
            .builder
            .build_float_to_signed_int(fv, i64_ty, "ftoi_in_val")
            .map_err(|e| CompileError::LlvmError(format!("ftoi conv: {}", e)))?;
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("ftoi in br: {}", e)))?;

        // merge: phi over the four results.
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(i64_ty, "ftoi_phi")
            .map_err(|e| CompileError::LlvmError(format!("ftoi phi: {}", e)))?;
        phi.add_incoming(&[
            (&zero as &dyn inkwell::values::BasicValue, nan_bb),
            (&imax as &dyn inkwell::values::BasicValue, hi_bb),
            (&imin as &dyn inkwell::values::BasicValue, lo_bb),
            (&in_val as &dyn inkwell::values::BasicValue, in_bb),
        ]);
        Ok(phi.as_basic_value().into_int_value())
    }

    pub(super) fn compile_to_int(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "to_int expects 1 argument".to_string(),
            ));
        }
        self.pending_to_number_is_any = false;
        match &args[0] {
            BasicMetadataValueEnum::IntValue(iv) => {
                // `Any` (map_get value) is an untyped i64 handle at LLVM level:
                // it may be a heap pointer to a C string, and we cannot
                // distinguish it from a plain integer statically (no local var
                // type directory in CheckedProgram). Route ALL i64 arguments
                // through the runtime heuristic: small integers (<1MB) pass
                // through without syscalls; string handles are parsed; large
                // even integers fall through mincore-miss to their own value.
                // Same design as `to_string`'s mimi_any_to_string path.
                return self.emit_any_to_int(*iv);
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let iv = self.build_saturating_float_to_signed_int(*fv)?;
                return Ok(iv.into());
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "to_int")?;
        // Audit wave2 §5.7 (red-line tier — ACTIVE L1 divergence): the old
        // code IGNORED the strtol ok flag and returned the sentinel (0 for
        // "abc"); the VM (interp/bytecode/builtins/convert.rs builtin_to_int)
        // fails loud with `to_int parse error: <Rust ParseIntError text>`.
        // Trap loud on any whole-string parse failure, classifying the
        // message Rust-style (empty input vs invalid digit).
        let (ok, value) = self.emit_strtol(s_ptr)?;
        self.emit_parse_fail_guard(s_ptr, ok, true, "to_int")?;
        Ok(value.into())
    }

    /// Fail-loud gate for the parse family (audit wave2 §5.7). Branches on
    /// the strtol/strtod whole-string `ok` flag: success continues in a
    /// fresh block; failure aborts with a Rust-style message. Rust rejects
    /// leading whitespace that strtol/strtod silently skip, so a leading
    /// blank byte is routed to the same trap (`is_int` selects the message
    /// family). `mimi_runtime_abort` is noreturn; on the trap path the
    /// builder ends after `unreachable`.
    fn emit_parse_fail_guard(
        &self,
        s_ptr: PointerValue<'ctx>,
        ok: inkwell::values::IntValue<'ctx>,
        is_int: bool,
        builtin: &str,
    ) -> MimiResult<()> {
        let i8_ty = self.context.i8_type();
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError(format!("{}: no enclosing function", builtin))
        })?;
        let ok_bb = self
            .context
            .append_basic_block(function, &format!("{}_parse_ok_bb", builtin));
        let trap_bb = self
            .context
            .append_basic_block(function, &format!("{}_parse_trap_bb", builtin));
        self.builder
            .build_conditional_branch(ok, ok_bb, trap_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(trap_bb);
        // Rust message classification: empty input → "cannot parse … from
        // empty string"; any other failure (including lead-whitespace
        // inputs strtol would skip) → "invalid digit found in string".
        let first_byte = self
            .build_load(BasicTypeEnum::IntType(i8_ty), s_ptr, "parse_first_byte")?
            .into_int_value();
        let is_empty = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                first_byte,
                i8_ty.const_int(0, false),
                "parse_is_empty",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let empty_msg = if is_int {
            "to_int parse error: cannot parse integer from empty string"
        } else {
            "to_float parse error: cannot parse float from empty string"
        };
        let invalid_msg = if is_int {
            "to_int parse error: invalid digit found in string"
        } else {
            "to_float parse error: invalid digit found in string"
        };
        let empty_g = self
            .builder
            .build_global_string_ptr(empty_msg, &format!("{}_parse_empty_msg", builtin))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let invalid_g = self
            .builder
            .build_global_string_ptr(invalid_msg, &format!("{}_parse_invalid_msg", builtin))
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let msg = self
            .builder
            .build_select(
                is_empty,
                empty_g.as_pointer_value(),
                invalid_g.as_pointer_value(),
                "parse_trap_msg",
            )
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?;
        let abort_fn = self.get_or_declare_abort_fn();
        self.builder
            .build_call(
                abort_fn,
                &[BasicMetadataValueEnum::PointerValue(
                    msg.into_pointer_value(),
                )],
                &format!("{}_parse_abort", builtin),
            )
            .map_err(|e| CompileError::LlvmError(format!("abort error: {}", e)))?;
        // SAFETY: mimi_runtime_abort is noreturn; this block is unreachable.
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("unreachable error: {}", e)))?;

        self.builder.position_at_end(ok_bb);
        // Rust's int/float parsers reject leading whitespace that strtol /
        // strtod silently skip ("  12"). Detect it and trap with the same
        // invalid-digit message so `to_int("  12")` matches the VM's Err.
        let lead_sp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                first_byte,
                i8_ty.const_int(0x20, false),
                "parse_lead_sp",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let lead_tab = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                first_byte,
                i8_ty.const_int(0x09, false),
                "parse_lead_tab",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let lead_nl = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                first_byte,
                i8_ty.const_int(0x0A, false),
                "parse_lead_nl",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let lead_cr = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                first_byte,
                i8_ty.const_int(0x0D, false),
                "parse_lead_cr",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let ws1 = self
            .builder
            .build_or(lead_sp, lead_tab, "parse_ws1")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let ws2 = self
            .builder
            .build_or(lead_nl, lead_cr, "parse_ws2")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let lead_ws = self
            .builder
            .build_or(ws1, ws2, "parse_lead_ws")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let cont_bb = self
            .context
            .append_basic_block(function, &format!("{}_parse_cont_bb", builtin));
        let ws_trap_bb = self
            .context
            .append_basic_block(function, &format!("{}_parse_ws_trap_bb", builtin));
        self.builder
            .build_conditional_branch(lead_ws, ws_trap_bb, cont_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(ws_trap_bb);
        self.emit_parse_trap(invalid_msg, &format!("{}_ws", builtin))?;
        self.builder.position_at_end(cont_bb);
        Ok(())
    }

    /// Promote a value used as an Any handle to the runtime's i64 handle.
    /// The runtime `mimi_any_to_int`/`mimi_any_to_float` signatures take i64;
    /// passing a narrower integer directly creates invalid LLVM IR.
    fn promote_any_handle_int(
        &self,
        iv: inkwell::values::IntValue<'ctx>,
        name: &str,
    ) -> MimiResult<inkwell::values::IntValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        if iv.get_type().get_bit_width() == i64_ty.get_bit_width() {
            return Ok(iv);
        }
        let promoted = if iv.get_type().get_bit_width() == 1 {
            self.builder
                .build_int_z_extend(iv, i64_ty, name)
                .map_err(|e| CompileError::LlvmError(format!("{name}: {e}")))?
        } else {
            self.builder
                .build_int_s_extend(iv, i64_ty, name)
                .map_err(|e| CompileError::LlvmError(format!("{name}: {e}")))?
        };
        Ok(promoted)
    }

    /// Emit a call to `mimi_any_to_int(value: i64) -> i64` (runtime heuristic:
    /// string handles are parsed via strtol, integers pass through).
    fn emit_any_to_int(
        &mut self,
        iv: inkwell::values::IntValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let i64_ty = self.context.i64_type();
        let promoted = self.promote_any_handle_int(iv, "any_to_int_promote")?;
        let any_fn_ty = i64_ty.fn_type(&[BasicMetadataTypeEnum::IntType(i64_ty)], false);
        let fn_any = self
            .module
            .get_function("mimi_any_to_int")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_any_to_int",
                    any_fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let result = self
            .builder
            .build_call(
                fn_any,
                &[BasicMetadataValueEnum::IntValue(promoted)],
                "any_to_int",
            )
            .map_err(|e| CompileError::LlvmError(format!("any_to_int: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_any_to_int returned void")?
            .into_int_value();
        Ok(result.into())
    }

    /// Emit a call to `mimi_any_to_float(value: i64) -> f64`.
    fn emit_any_to_float(
        &mut self,
        iv: inkwell::values::IntValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();
        let promoted = self.promote_any_handle_int(iv, "any_to_float_promote")?;
        let any_fn_ty = f64_ty.fn_type(&[BasicMetadataTypeEnum::IntType(i64_ty)], false);
        let fn_any = self
            .module
            .get_function("mimi_any_to_float")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "mimi_any_to_float",
                    any_fn_ty,
                    Some(inkwell::module::Linkage::External),
                )
            });
        let result = self
            .builder
            .build_call(
                fn_any,
                &[BasicMetadataValueEnum::IntValue(promoted)],
                "any_to_float",
            )
            .map_err(|e| CompileError::LlvmError(format!("any_to_float: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_any_to_float returned void")?
            .into_float_value();
        Ok(result.into())
    }

    pub(super) fn compile_str_parse_int(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_parse_int expects 1 argument".to_string(),
            ));
        }
        self.pending_to_number_is_any = false;
        let true_val = self.context.bool_type().const_int(1, false);
        match &args[0] {
            BasicMetadataValueEnum::IntValue(iv) => {
                // See compile_to_int: i64 arguments may be Any handles (map
                // values), so route through the runtime heuristic.
                let v = self.emit_any_to_int(*iv)?.into_int_value();
                return self.build_parse_int_tuple(true_val, v);
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let iv = self.build_saturating_float_to_signed_int(*fv)?;
                return self.build_parse_int_tuple(true_val, iv);
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "str_parse_int")?;
        let (ok, value) = self.emit_strtol(s_ptr)?;
        self.build_parse_int_tuple(ok, value)
    }

    /// Parse a C string with strtod and return (ok, value).
    fn emit_strtod(
        &self,
        s_ptr: PointerValue<'ctx>,
    ) -> MimiResult<(
        inkwell::values::IntValue<'ctx>,
        inkwell::values::FloatValue<'ctx>,
    )> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let f64_ty = self.context.f64_type();
        let strtod_fn = self
            .module
            .get_function("strtod")
            .or_else(|| {
                let ty = f64_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                        BasicMetadataTypeEnum::PointerType(i8_ptr),
                    ],
                    false,
                );
                Some(self.module.add_function(
                    "strtod",
                    ty,
                    Some(inkwell::module::Linkage::External),
                ))
            })
            .ok_or_else(|| "failed to get or create strtod function".to_string())?;
        let endptr_alloca = self.build_alloca(i8_ptr, "strtod_endptr")?;
        self.build_store(endptr_alloca, i8_ptr.const_null())?;
        let call = self
            .builder
            .build_call(
                strtod_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(s_ptr),
                    BasicMetadataValueEnum::PointerValue(endptr_alloca),
                ],
                "strtod_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strtod error: {}", e)))?;
        let value = self.expect_basic_value(&call, "strtod")?.into_float_value();
        let endptr = self
            .builder
            .build_load(i8_ptr, endptr_alloca, "strtod_endptr_load")
            .map_err(|e| CompileError::LlvmError(format!("load endptr: {}", e)))?
            .into_pointer_value();
        let end_i = self
            .builder
            .build_ptr_to_int(endptr, i64_ty, "end_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint endptr: {}", e)))?;
        let s_i = self
            .builder
            .build_ptr_to_int(s_ptr, i64_ty, "s_i")
            .map_err(|e| CompileError::LlvmError(format!("ptrtoint s: {}", e)))?;
        let consumed = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, end_i, s_i, "strtod_consumed")
            .map_err(|e| CompileError::LlvmError(format!("icmp consumed: {}", e)))?;
        let end_byte = self
            .builder
            .build_load(BasicTypeEnum::IntType(i8_ty), endptr, "strtod_end_byte")
            .map_err(|e| CompileError::LlvmError(format!("load end byte: {}", e)))?
            .into_int_value();
        let end_null = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                end_byte,
                i8_ty.const_int(0, false),
                "strtod_end_null",
            )
            .map_err(|e| CompileError::LlvmError(format!("icmp end null: {}", e)))?;
        let ok = self
            .builder
            .build_and(consumed, end_null, "strtod_ok")
            .map_err(|e| CompileError::LlvmError(format!("and ok: {}", e)))?;
        Ok((ok, value))
    }

    fn build_parse_float_tuple(
        &self,
        ok: inkwell::values::IntValue<'ctx>,
        value: inkwell::values::FloatValue<'ctx>,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        let tuple_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.bool_type()),
                BasicTypeEnum::FloatType(self.context.f64_type()),
            ],
            false,
        );
        let alloca = self.build_alloca(tuple_ty, "parse_float_tuple")?;
        let ok_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 0, "parse_ok")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let val_gep = self
            .gep()
            .build_struct_gep(tuple_ty, alloca, 1, "parse_val")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        // B-1 (SD-9, 2026-08-06): finiteness gate for str_parse_float. The VM
        // requires the parsed value finite — NaN/±Inf → (false, 0.0), matching
        // its `Ok(n) if n.is_finite()` / `Ok(_) => (false, 0.0)` arms
        // (interp/bytecode/builtins/string.rs:531-532). codegen's strtod
        // ACCEPTS "NaN"/"inf" (ok=true) -> non-finite value entered the system
        // and the native output diverged from the VM (b1.mimi: nan:ok vs
        // nan:bad). Gate the emitted value here: finite → keep, else
        // (false, 0.0).
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("no current function for parse_float".into()))?;
        let f64_ty = self.context.f64_type();
        // not NaN: `v == v` is false for NaN under ORD.
        let not_nan = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::ORD, value, value, "pf_not_nan")
            .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?;
        // not ±Inf: |v| <= f64::MAX.
        let fabs_name = format!("llvm.fabs.f{}", f64_ty.get_bit_width());
        let fabs_fn = self.module.get_function(&fabs_name).unwrap_or_else(|| {
            self.module.add_function(
                &fabs_name,
                f64_ty.fn_type(&[BasicMetadataTypeEnum::FloatType(f64_ty)], false),
                Some(inkwell::module::Linkage::External),
            )
        });
        let abs_val = self
            .builder
            .build_call(
                fabs_fn,
                &[BasicMetadataValueEnum::FloatValue(value)],
                "pf_abs",
            )
            .map_err(|e| CompileError::LlvmError(format!("fabs: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("pf_abs void".into()))?
            .into_float_value();
        let max_val = f64_ty.const_float(f64::MAX);
        let le_max = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OLE, abs_val, max_val, "pf_le_max")
            .map_err(|e| CompileError::LlvmError(format!("fcmp: {}", e)))?;
        let finite = self
            .builder
            .build_and(not_nan, le_max, "pf_finite")
            .map_err(|e| CompileError::LlvmError(format!("and: {}", e)))?;
        let finite_bb = self.context.append_basic_block(function, "pf_finite");
        let bad_bb = self.context.append_basic_block(function, "pf_nonfinite");
        let merge_bb = self.context.append_basic_block(function, "pf_merge");
        self.builder
            .build_conditional_branch(finite, finite_bb, bad_bb)
            .map_err(|e| CompileError::LlvmError(format!("br: {}", e)))?;
        self.builder.position_at_end(finite_bb);
        self.build_store(ok_gep, ok)?;
        self.build_store(val_gep, value)?;
        self.build_br(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("br: {}", e)))?;
        self.builder.position_at_end(bad_bb);
        let false_bool = self.context.bool_type().const_int(0, false);
        let zero = f64_ty.const_float(0.0);
        self.build_store(ok_gep, false_bool)?;
        self.build_store(val_gep, zero)?;
        self.build_br(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("br: {}", e)))?;
        self.builder.position_at_end(merge_bb);
        self.build_load(tuple_ty, alloca, "parse_float_tuple_val")
    }

    pub(super) fn compile_to_float(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "to_float expects 1 argument".to_string(),
            ));
        }
        match &args[0] {
            BasicMetadataValueEnum::FloatValue(fv) => return Ok((*fv).into()),
            BasicMetadataValueEnum::IntValue(iv) => {
                // Typed i32 (and narrower) integers are definitely numeric,
                // not Any handles, so convert directly like the VM does.
                // Only i64 can also be an Any handle at the LLVM level and
                // therefore keeps the runtime heuristic.
                let bit_width = iv.get_type().get_bit_width();
                if bit_width == 64 {
                    return self.emit_any_to_float(*iv);
                }
                let f64_ty = self.context.f64_type();
                let fv = self
                    .builder
                    .build_signed_int_to_float(*iv, f64_ty, "to_float_i32")
                    .map_err(|e| CompileError::LlvmError(format!("to_float int->float: {}", e)))?;
                return Ok(fv.into());
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "to_float")?;
        // Audit wave2 §5.7 (red-line tier): the old code ignored the strtod
        // ok flag (sentinel 0.0 for "abc"); strtod also happily parses
        // "inf"/"nan"/"1e999" (→ ±Inf/NaN) which the VM rejects. Mirror the
        // VM (convert.rs builtin_to_float): whole-string parse failure →
        // "to_float parse error: …"; non-finite RESULT → "to_float parse
        // error: non-finite value '<input>'" — the finiteness rejection is
        // UNCONDITIONAL (the VM does not gate it on ieee_float{}).
        let (ok, value) = self.emit_strtod(s_ptr)?;
        self.emit_parse_fail_guard(s_ptr, ok, false, "to_float")?;
        // Non-finite gate (unconditional — VM parity, not SD-9 ieee-gated).
        let f64_ty = self.context.f64_type();
        let function = self.current_function().ok_or_else(|| {
            CompileError::LlvmError("to_float: no enclosing function".to_string())
        })?;
        let is_nan = self
            .builder
            .build_float_compare(
                inkwell::FloatPredicate::UNO,
                value,
                value,
                "to_float_is_nan",
            )
            .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?;
        let fabs_fn = self
            .module
            .get_function("llvm.fabs.f64")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "llvm.fabs.f64",
                    f64_ty.fn_type(&[BasicMetadataTypeEnum::FloatType(f64_ty)], false),
                    Some(inkwell::module::Linkage::External),
                )
            });
        let abs_val = self
            .builder
            .build_call(
                fabs_fn,
                &[BasicMetadataValueEnum::FloatValue(value)],
                "to_float_abs",
            )
            .map_err(|e| CompileError::LlvmError(format!("fabs error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("llvm.fabs.f64 returned void")?
            .into_float_value();
        let is_inf = self
            .builder
            .build_float_compare(
                inkwell::FloatPredicate::OEQ,
                abs_val,
                f64_ty.const_float(f64::INFINITY),
                "to_float_is_inf",
            )
            .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?;
        let not_finite = self
            .builder
            .build_or(is_nan, is_inf, "to_float_not_finite")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let finite_bb = self
            .context
            .append_basic_block(function, "to_float_finite_bb");
        let nf_trap_bb = self
            .context
            .append_basic_block(function, "to_float_nonfinite_trap_bb");
        self.builder
            .build_conditional_branch(not_finite, nf_trap_bb, finite_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(nf_trap_bb);
        self.emit_trap_with_str_message(
            "to_float parse error: non-finite value '%s'",
            s_ptr,
            "to_float_nf",
        )?;
        self.builder.position_at_end(finite_bb);
        Ok(value.into())
    }

    pub(super) fn compile_str_parse_float(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "str_parse_float expects 1 argument".to_string(),
            ));
        }
        let true_val = self.context.bool_type().const_int(1, false);
        match &args[0] {
            BasicMetadataValueEnum::FloatValue(fv) => {
                return self.build_parse_float_tuple(true_val, *fv);
            }
            BasicMetadataValueEnum::IntValue(iv) => {
                let fv = self.emit_any_to_float(*iv)?.into_float_value();
                return self.build_parse_float_tuple(true_val, fv);
            }
            _ => {}
        }
        let s_ptr = self.extract_string_arg_ptr(&args[0], "str_parse_float")?;
        let (ok, value) = self.emit_strtod(s_ptr)?;
        self.build_parse_float_tuple(ok, value)
    }
}
