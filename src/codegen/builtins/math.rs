use super::CodeGenerator;
use crate::codegen::CallSiteValueExt;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FloatValue, IntValue};

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_abs(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("abs expects 1 argument".into());
        }
        match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => {
                // abs(x) = x < 0 ? -x : x
                // Audit 2026-08-05 §8 [CRITICAL] FIX-5: the old code SATURATED
                // iN::MIN to iN::MAX — a silent SD-7 violation. abs(iN::MIN)
                // overflows (two's-complement asymmetry); the bytecode VM's
                // checked_abs errors on it (interp/bytecode/builtins/math.rs:234).
                // Trap with the E0802 integer-overflow machinery instead, same
                // pattern as compile_int_binop (expr/operator.rs), including
                // Fault absorption in fallible multi-target transitions.
                let ty = iv.get_type();
                let bw = ty.get_bit_width();
                if bw >= 8 {
                    // iN::MIN at the operand's OWN width is 0x8000...0, i.e.
                    // 1 << (N-1). (FIX-5: the old code built i32/i64 constants
                    // and truncated them into narrower types — i8/i16 got wrong
                    // MIN values.) bw < 8 (i1) has no meaningful MIN overflow.
                    let min_val = ty.const_int(1u64 << (bw - 1), false);
                    let is_min = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::EQ, iv, min_val, "is_min")
                        .map_err(|e| format!("cmp error: {}", e))?;
                    let function = self.current_function().ok_or_else(|| {
                        CompileError::LlvmError("abs: no enclosing function".into())
                    })?;
                    let trap_bb = self.context.append_basic_block(function, "trap_abs_min");
                    let ok_bb = self.context.append_basic_block(function, "abs_ok");
                    self.builder
                        .build_conditional_branch(is_min, trap_bb, ok_bb)
                        .map_err(|e| format!("br error: {}", e))?;
                    self.builder.position_at_end(trap_bb);
                    if self.in_fallible_multi_target() {
                        self.emit_panic_fault_return("E0802")?;
                    } else {
                        let trap_fn = self.get_runtime_fn("mimi_trap_overflow")?;
                        let op_cstr = self
                            .builder
                            .build_global_string_ptr("abs", "abs_op_name")
                            .map_err(|e| format!("global string error: {}", e))?;
                        self.builder
                            .build_call(
                                trap_fn,
                                &[BasicMetadataValueEnum::PointerValue(
                                    op_cstr.as_pointer_value(),
                                )],
                                "",
                            )
                            .map_err(|e| format!("call error: {}", e))?;
                        self.builder
                            .build_unreachable()
                            .map_err(|e| format!("unreachable error: {}", e))?;
                    }
                    self.builder.position_at_end(ok_bb);
                }
                // The only overflow case (x == iN::MIN) trapped above, so the
                // negation here is exact. No saturation anywhere.
                let zero = ty.const_zero();
                let neg = self
                    .builder
                    .build_int_sub(zero, iv, "neg")
                    .map_err(|e| format!("neg error: {}", e))?;
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, iv, zero, "is_neg")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let result = self
                    .builder
                    .build_select(cmp, neg, iv, "abs_val")
                    .map_err(|e| format!("select error: {}", e))?
                    .into_int_value();
                Ok(result.into())
            }
            BasicMetadataValueEnum::FloatValue(_fv) => {
                // Use fabs
                let fabs_fn = self.module.get_function("fabs").unwrap_or_else(|| {
                    let fabs_ty = self.context.f64_type().fn_type(
                        &[inkwell::types::BasicMetadataTypeEnum::FloatType(
                            self.context.f64_type(),
                        )],
                        false,
                    );
                    self.module.add_function(
                        "fabs",
                        fabs_ty,
                        Some(inkwell::module::Linkage::External),
                    )
                });
                let call = self
                    .builder
                    .build_call(fabs_fn, args, "fabs_call")
                    .map_err(|e| format!("fabs error: {}", e))?;
                Ok(self.expect_basic_value(&call, "fabs")?)
            }
            _ => Err("abs requires numeric type".into()),
        }
    }

    pub(super) fn compile_sqrt(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("sqrt expects 1 argument".into());
        }
        // Audit 2026-08-05 §8 [HIGH] FIX-6: sqrt is subject to the SD-9
        // finiteness invariant — the bytecode VM's builtin_sqrt runs
        // check_float (unary_float! macro), so sqrt(-1.0) = NaN traps with
        // E0813 outside ieee_float{}. Also coerce the operand to f64 so
        // integer arguments produce a well-typed call (the old code passed
        // raw args at whatever LLVM type they had into the f64 libc
        // signature).
        let arg = self.coerce_to_f64(args[0], "sqrt")?;
        let sqrt_fn = self.module.get_function("sqrt").unwrap_or_else(|| {
            let sqrt_ty = self.context.f64_type().fn_type(
                &[inkwell::types::BasicMetadataTypeEnum::FloatType(
                    self.context.f64_type(),
                )],
                false,
            );
            self.module
                .add_function("sqrt", sqrt_ty, Some(inkwell::module::Linkage::External))
        });
        let call = self
            .builder
            .build_call(
                sqrt_fn,
                &[BasicMetadataValueEnum::FloatValue(arg)],
                "sqrt_call",
            )
            .map_err(|e| format!("sqrt error: {}", e))?;
        let result = self.expect_basic_value(&call, "sqrt")?.into_float_value();
        let result = self.enforce_float_finite(result, "sqrt")?;
        Ok(result.into())
    }

    pub(super) fn compile_min_max(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("min/max expects 2 arguments".into());
        }
        let a = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "min/max requires integer types".into(),
                ))
            }
        };
        let b = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "min/max requires integer types".into(),
                ))
            }
        };
        let pred = if name == "min" {
            inkwell::IntPredicate::SLT
        } else {
            inkwell::IntPredicate::SGT
        };
        let cmp = self
            .builder
            .build_int_compare(pred, a, b, "cmp")
            .map_err(|e| format!("cmp error: {}", e))?;
        let result = self
            .builder
            .build_select(cmp, a, b, "minmax")
            .map_err(|e| format!("select error: {}", e))?;
        Ok(result)
    }

    pub(super) fn compile_floor_ceil_round(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err("floor/ceil/round expects 1 argument".into());
        }
        let fn_name = match name {
            "floor" => "floor",
            "ceil" => "ceil",
            _ => "round",
        };
        let c_fn = self.module.get_function(fn_name).unwrap_or_else(|| {
            let ty = self.context.f64_type().fn_type(
                &[inkwell::types::BasicMetadataTypeEnum::FloatType(
                    self.context.f64_type(),
                )],
                false,
            );
            self.module
                .add_function(fn_name, ty, Some(inkwell::module::Linkage::External))
        });
        let call = self
            .builder
            .build_call(c_fn, args, &format!("{}_call", fn_name))
            .map_err(|e| format!("{} error: {}", fn_name, e))?;
        self.expect_basic_value(&call, fn_name)
    }

    pub(super) fn compile_pow(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("pow expects 2 arguments".into());
        }
        let f64_ty = self.context.f64_type();
        // Audit 2026-08-05 §8 [CRITICAL] FIX-4: int×int must NOT detour
        // through f64 + libc pow — that silently loses precision above 2^53
        // and, far worse, turns integer overflow (pow(10, 19) > i64::MAX)
        // into a silent Inf-ish float. Bytecode VM semantics are the
        // reference (interp/bytecode/builtins/math.rs:348-369): integer pow
        // is checked_pow — overflow and negative exponents are errors.
        // Route to the runtime's __mimi_pow_i64 (runtime/mod.rs:1101), which
        // implements exactly those traps (CG-H3).
        if let (BasicMetadataValueEnum::IntValue(a), BasicMetadataValueEnum::IntValue(b)) =
            (args[0], args[1])
        {
            let a64 = self.widen_pow_arg_to_i64(a, "pow_base")?;
            let b64 = self.widen_pow_arg_to_i64(b, "pow_exp")?;
            let pow_fn = self.get_or_declare_pow_i64();
            let call = self
                .builder
                .build_call(
                    pow_fn,
                    &[
                        BasicMetadataValueEnum::IntValue(a64),
                        BasicMetadataValueEnum::IntValue(b64),
                    ],
                    "pow_i64_call",
                )
                .map_err(|e| format!("pow error: {}", e))?;
            let r64 = self.expect_basic_value(&call, "pow")?.into_int_value();
            // The checker types pow(_, _) -> f64 (core/infer/call/simple.rs:838),
            // so keep the value-type contract: convert the exact i64 result to
            // f64. The trap semantics (overflow / negative exponent) are fully
            // decided by the runtime call above; results with |v| <= 2^53
            // round-trip exactly through f64.
            let rf = self
                .builder
                .build_signed_int_to_float(r64, f64_ty, "pow_i64_to_f64")
                .map_err(|e| format!("pow int_to_float error: {}", e))?;
            return Ok(rf.into());
        }
        // Float path (float×float and int×float mixes): coerce to f64, libc
        // pow, then the SD-9 finiteness invariant — the bytecode VM runs
        // check_float on this path too (builtin_pow float arm).
        let a = self.coerce_to_f64(args[0], "pow")?;
        let b = self.coerce_to_f64(args[1], "pow")?;
        let pow_fn = self.module.get_function("pow").unwrap_or_else(|| {
            let ty = f64_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::FloatType(f64_ty),
                    BasicMetadataTypeEnum::FloatType(f64_ty),
                ],
                false,
            );
            self.module
                .add_function("pow", ty, Some(inkwell::module::Linkage::External))
        });
        let call = self
            .builder
            .build_call(
                pow_fn,
                &[
                    BasicMetadataValueEnum::FloatValue(a),
                    BasicMetadataValueEnum::FloatValue(b),
                ],
                "pow_call",
            )
            .map_err(|e| format!("pow error: {}", e))?;
        let result = self.expect_basic_value(&call, "pow")?.into_float_value();
        let result = self.enforce_float_finite(result, "pow")?;
        Ok(result.into())
    }

    pub(super) fn compile_random(
        &self,
        _args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Call libc random() and normalize to f64 in [0, 1)
        let f64_ty = self.context.f64_type();
        let i64_ty = self.context.i64_type();
        let random_fn = self.module.get_function("random").unwrap_or_else(|| {
            let ty = i64_ty.fn_type(&[], false);
            self.module
                .add_function("random", ty, Some(inkwell::module::Linkage::External))
        });
        let call = self
            .builder
            .build_call(random_fn, &[], "random_call")
            .map_err(|e| format!("random error: {}", e))?;
        let raw = self.expect_basic_value(&call, "random")?.into_int_value();
        let raw_f = self
            .builder
            .build_signed_int_to_float(raw, f64_ty, "rand_f")
            .map_err(|e| format!("random int_to_float error: {}", e))?;
        // glibc random() returns values through 2^31-1 inclusive. Divide by
        // 2^31, not RAND_MAX, to preserve the documented half-open range.
        let random_range = f64_ty.const_float(2147483648.0);
        let result = self
            .builder
            .build_float_div(raw_f, random_range, "rand_norm")
            .map_err(|e| format!("random div error: {}", e))?;
        Ok(result.into())
    }

    pub(super) fn compile_pi(
        &self,
        _args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        // Return constant pi as f64
        Ok(self
            .context
            .f64_type()
            .const_float(std::f64::consts::PI)
            .into())
    }

    // === v0.28.13 trigonometric and exponential builtins ===
    //
    // Most are thin wrappers around libc libm functions. The runtime is
    // linked via cc, so the symbol is available at link time.

    /// Helper: ensure a value is f64, converting i64 if needed.
    fn coerce_to_f64(
        &self,
        v: BasicMetadataValueEnum<'ctx>,
        name: &str,
    ) -> MimiResult<inkwell::values::FloatValue<'ctx>> {
        let f64_ty = self.context.f64_type();
        match v {
            BasicMetadataValueEnum::FloatValue(fv) => Ok(fv),
            BasicMetadataValueEnum::IntValue(iv) => self
                .builder
                .build_signed_int_to_float(iv, f64_ty, &format!("{}_f64", name))
                .map_err(|e| CompileError::LlvmError(format!("int_to_float error: {}", e))),
            _ => Err(CompileError::TypeMismatch(format!(
                "{} requires a numeric argument",
                name
            ))),
        }
    }

    /// Helper: get-or-declare a unary f64 -> f64 libc function.
    fn get_or_declare_unary_f64(&self, fn_name: &str) -> inkwell::values::FunctionValue<'ctx> {
        self.module.get_function(fn_name).unwrap_or_else(|| {
            let f64_ty = self.context.f64_type();
            let ty = f64_ty.fn_type(
                &[inkwell::types::BasicMetadataTypeEnum::FloatType(f64_ty)],
                false,
            );
            self.module
                .add_function(fn_name, ty, Some(inkwell::module::Linkage::External))
        })
    }

    /// Helper: get-or-declare a binary f64,f64 -> f64 libc function.
    fn get_or_declare_binary_f64(&self, fn_name: &str) -> inkwell::values::FunctionValue<'ctx> {
        self.module.get_function(fn_name).unwrap_or_else(|| {
            let f64_ty = self.context.f64_type();
            let ty = f64_ty.fn_type(
                &[
                    inkwell::types::BasicMetadataTypeEnum::FloatType(f64_ty),
                    inkwell::types::BasicMetadataTypeEnum::FloatType(f64_ty),
                ],
                false,
            );
            self.module
                .add_function(fn_name, ty, Some(inkwell::module::Linkage::External))
        })
    }

    pub(super) fn compile_math_unary(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
        fn_name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(format!("{} expects 1 argument", fn_name).into());
        }
        let arg = self.coerce_to_f64(args[0], fn_name)?;
        let f = self.get_or_declare_unary_f64(fn_name);
        let call = self
            .builder
            .build_call(f, &[BasicMetadataValueEnum::FloatValue(arg)], "math_call")
            .map_err(|e| format!("{} error: {}", fn_name, e))?;
        // Audit 2026-08-05 §8 [HIGH] FIX-6: SD-9 parity with the bytecode
        // VM's unary_float! macro — every unary math builtin runs check_float
        // on its result (E0813 outside ieee_float{}).
        let result = self.expect_basic_value(&call, fn_name)?.into_float_value();
        let result = self.enforce_float_finite(result, fn_name)?;
        Ok(result.into())
    }

    pub(super) fn compile_math_binary(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
        fn_name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(format!("{} expects 2 arguments", fn_name).into());
        }
        let a = self.coerce_to_f64(args[0], fn_name)?;
        let b = self.coerce_to_f64(args[1], fn_name)?;
        let f = self.get_or_declare_binary_f64(fn_name);
        let call = self
            .builder
            .build_call(
                f,
                &[
                    BasicMetadataValueEnum::FloatValue(a),
                    BasicMetadataValueEnum::FloatValue(b),
                ],
                "math_call",
            )
            .map_err(|e| format!("{} error: {}", fn_name, e))?;
        // Audit 2026-08-05 §8 [HIGH] FIX-6: SD-9 (VM builtin_atan2 runs
        // check_float on the result).
        let result = self.expect_basic_value(&call, fn_name)?.into_float_value();
        let result = self.enforce_float_finite(result, fn_name)?;
        Ok(result.into())
    }

    /// log(x) = natural log; log(x, base) = base-N logarithm (log(x)/log(base)).
    pub(super) fn compile_math_log(
        &mut self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() || args.len() > 2 {
            return Err("log expects 1 or 2 arguments".into());
        }
        let x = self.coerce_to_f64(args[0], "log")?;
        let ln_fn = self.get_or_declare_unary_f64("log");
        // Audit 2026-08-05 §8 [HIGH] FIX-6: base-domain check with the VM's
        // semantics (interp/bytecode/builtins/math.rs:288-307): log(x, base)
        // with base <= 0 or base == 1 errors UNCONDITIONALLY — the VM checks
        // it before check_float, i.e. even inside ieee_float{}. log(x, 1.0)
        // used to produce Inf silently here (audit: "log(x,1)→Inf 静默").
        let base = if args.len() == 2 {
            let base = self.coerce_to_f64(args[1], "log")?;
            let zero = base.get_type().const_float(0.0);
            let one = base.get_type().const_float(1.0);
            let le_zero = self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OLE, base, zero, "log_base_le0")
                .map_err(|e| format!("log base cmp error: {}", e))?;
            let eq_one = self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OEQ, base, one, "log_base_eq1")
                .map_err(|e| format!("log base cmp error: {}", e))?;
            let bad_base = self
                .builder
                .build_or(le_zero, eq_one, "log_base_bad")
                .map_err(|e| format!("log base or error: {}", e))?;
            let function = self
                .current_function()
                .ok_or_else(|| CompileError::LlvmError("log: no enclosing function".into()))?;
            let trap_bb = self.context.append_basic_block(function, "trap_log_base");
            let ok_bb = self.context.append_basic_block(function, "log_base_ok");
            self.builder
                .build_conditional_branch(bad_base, trap_bb, ok_bb)
                .map_err(|e| format!("log base br error: {}", e))?;
            self.builder.position_at_end(trap_bb);
            // VM message parity (no E08xx code exists for math domain
            // errors — use the generic loud abort, same as list OOB).
            let abort_fn = self.get_or_declare_abort_fn();
            let msg = self
                .builder
                .build_global_string_ptr("log: base must be positive and not 1", "log_base_msg")
                .map_err(|e| format!("global string error: {}", e))?;
            self.build_call(
                abort_fn,
                &[BasicMetadataValueEnum::PointerValue(msg.as_pointer_value())],
                "log_base_abort",
            )?;
            self.builder
                .build_unreachable()
                .map_err(|e| format!("unreachable error: {}", e))?;
            self.builder.position_at_end(ok_bb);
            Some(base)
        } else {
            None
        };
        let ln_call = self
            .builder
            .build_call(ln_fn, &[BasicMetadataValueEnum::FloatValue(x)], "log_x")
            .map_err(|e| format!("log error: {}", e))?;
        let ln_x = self.expect_basic_value(&ln_call, "log")?.into_float_value();
        let result = match base {
            None => ln_x,
            Some(base) => {
                let ln_base_call = self
                    .builder
                    .build_call(
                        ln_fn,
                        &[BasicMetadataValueEnum::FloatValue(base)],
                        "log_base",
                    )
                    .map_err(|e| format!("log error: {}", e))?;
                let ln_base = self
                    .expect_basic_value(&ln_base_call, "log")?
                    .into_float_value();
                self.builder
                    .build_float_div(ln_x, ln_base, "log_result")
                    .map_err(|e| format!("log div error: {}", e))?
            }
        };
        // Audit 2026-08-05 §8 [HIGH] FIX-6: SD-9 on the FINAL result only
        // (the VM's builtin_log runs check_float once on r). ln(-1) = NaN or
        // ln(0) = -Inf therefore trap here with E0813 outside ieee_float{}.
        let result = self.enforce_float_finite(result, "log")?;
        Ok(BasicValueEnum::FloatValue(result))
    }

    // === SD-7 escape hatches: wrapping arithmetic ===

    pub(super) fn compile_wrapping_arith(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(format!("{} expects 2 arguments", name).into());
        }
        let a = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(format!(
                    "{} requires integers",
                    name
                )))
            }
        };
        let b = match args[1] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(format!(
                    "{} requires integers",
                    name
                )))
            }
        };
        // Wrapping arithmetic: plain LLVM add/sub/mul wraps by default.
        let result = match name {
            "wrapping_add" => self.builder.build_int_add(a, b, "wadd"),
            "wrapping_sub" => self.builder.build_int_sub(a, b, "wsub"),
            "wrapping_mul" => self.builder.build_int_mul(a, b, "wmul"),
            _ => return Err(format!("unknown wrapping op: {}", name).into()),
        };
        Ok(result.map_err(|e| format!("{} error: {}", name, e))?.into())
    }

    // === SD-9 support: float classification ===

    pub(super) fn compile_float_classify(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(format!("{} expects 1 argument", name).into());
        }
        let v = match args[0] {
            BasicMetadataValueEnum::FloatValue(fv) => fv,
            BasicMetadataValueEnum::IntValue(iv) => {
                // Widen integer to float for classification.
                self.builder
                    .build_signed_int_to_float(iv, self.context.f64_type(), "int_to_f64")
                    .map_err(|e| format!("{} widen error: {}", name, e))?
            }
            _ => {
                return Err(CompileError::TypeMismatch(format!(
                    "{} requires a number",
                    name
                )))
            }
        };
        let bool_ty = self.context.bool_type();
        let result = match name {
            "is_nan" => {
                // NaN: fcmp uno x, x (unordered with self).
                self.builder
                    .build_float_compare(inkwell::FloatPredicate::UNO, v, v, "is_nan")
                    .map_err(|e| format!("is_nan error: {}", e))?
            }
            "is_infinite" => {
                // Inf: |x| == Inf.
                let fabs_fn = self.get_or_declare_fabs()?;
                let abs_val = self
                    .builder
                    .build_call(fabs_fn, &[BasicMetadataValueEnum::FloatValue(v)], "fabs")
                    .map_err(|e| format!("fabs error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("fabs returned void".into()))?
                    .into_float_value();
                let inf = self.context.f64_type().const_float(f64::INFINITY);
                self.builder
                    .build_float_compare(inkwell::FloatPredicate::OEQ, abs_val, inf, "is_inf")
                    .map_err(|e| format!("is_infinite error: {}", e))?
            }
            "is_finite" => {
                // Finite: NOT (NaN OR Inf).
                let is_nan = self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::UNO, v, v, "nan_check")
                    .map_err(|e| format!("is_finite nan error: {}", e))?;
                let fabs_fn = self.get_or_declare_fabs()?;
                let abs_val = self
                    .builder
                    .build_call(fabs_fn, &[BasicMetadataValueEnum::FloatValue(v)], "fabs2")
                    .map_err(|e| format!("fabs2 error: {}", e))?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("fabs2 returned void".into()))?
                    .into_float_value();
                let inf = self.context.f64_type().const_float(f64::INFINITY);
                let is_inf = self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::OEQ, abs_val, inf, "inf_check")
                    .map_err(|e| format!("is_finite inf error: {}", e))?;
                let not_finite = self
                    .builder
                    .build_or(is_nan, is_inf, "not_finite")
                    .map_err(|e| format!("is_finite or error: {}", e))?;
                self.builder
                    .build_not(not_finite, "is_finite")
                    .map_err(|e| format!("is_finite not error: {}", e))?
            }
            _ => return Err(format!("unknown float classify op: {}", name).into()),
        };
        // Ensure result is i1 (bool).
        let _ = bool_ty;
        Ok(BasicValueEnum::IntValue(result))
    }

    // === SD-10 escape hatches: explicit float comparison ===

    pub(super) fn compile_is_close(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 3 {
            return Err("is_close expects 3 arguments (a, b, epsilon)".into());
        }
        let a = self.expect_float_arg(args, 0, "is_close")?;
        let b = self.expect_float_arg(args, 1, "is_close")?;
        let eps = self.expect_float_arg(args, 2, "is_close")?;
        // |a - b| <= epsilon
        let diff = self
            .builder
            .build_float_sub(a, b, "close_diff")
            .map_err(|e| format!("is_close sub error: {}", e))?;
        let fabs_fn = self.get_or_declare_fabs()?;
        let abs_diff = self
            .builder
            .build_call(
                fabs_fn,
                &[BasicMetadataValueEnum::FloatValue(diff)],
                "close_abs",
            )
            .map_err(|e| format!("is_close fabs error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("is_close fabs returned void".into()))?
            .into_float_value();
        let result = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OLE, abs_diff, eps, "is_close")
            .map_err(|e| format!("is_close cmp error: {}", e))?;
        Ok(BasicValueEnum::IntValue(result))
    }

    pub(super) fn compile_f64_eq_exact(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err("f64_eq_exact expects 2 arguments".into());
        }
        let a = self.expect_float_arg(args, 0, "f64_eq_exact")?;
        let b = self.expect_float_arg(args, 1, "f64_eq_exact")?;
        let result = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OEQ, a, b, "f64_eq")
            .map_err(|e| format!("f64_eq_exact error: {}", e))?;
        Ok(BasicValueEnum::IntValue(result))
    }

    // === Helpers ===

    /// Audit 2026-08-05 §8 FIX-4/FIX-6 — SD-9 finiteness invariant for math
    /// builtins. Mirrors `compile_float_binop` (expr/operator.rs:745-851,
    /// read-only reference): suspended inside `ieee_float { }`
    /// (`ieee_depth > 0`); otherwise a NaN/Inf result traps with E0813 —
    /// absorbed into a `Fault` in a fallible multi-target transition, or
    /// aborting via `mimi_trap_float_not_finite` everywhere else.
    fn enforce_float_finite(
        &mut self,
        result: FloatValue<'ctx>,
        op_name: &str,
    ) -> MimiResult<FloatValue<'ctx>> {
        // v0.34.10a (SD-9): inside `ieee_float { }` the finiteness invariant
        // is suspended — IEEE 754 NaN/Inf are legitimate there.
        if self.ieee_depth > 0 {
            return Ok(result);
        }
        let f_ty = result.get_type();
        let function = self
            .current_function()
            .ok_or_else(|| CompileError::LlvmError("math builtin: no enclosing function".into()))?;

        // NaN: fcmp uno x, x → true only for NaN.
        let is_nan = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::UNO, result, result, "is_nan")
            .map_err(|e| format!("fcmp error: {}", e))?;
        // Inf: |x| == Inf via llvm.fabs (width-generic, as in operator.rs).
        let fabs_name = format!("llvm.fabs.f{}", f_ty.get_bit_width());
        let fabs_fn = self.module.get_function(&fabs_name).unwrap_or_else(|| {
            self.module.add_function(
                &fabs_name,
                f_ty.fn_type(&[BasicMetadataTypeEnum::FloatType(f_ty)], false),
                Some(inkwell::module::Linkage::External),
            )
        });
        let abs_val = self
            .builder
            .build_call(
                fabs_fn,
                &[BasicMetadataValueEnum::FloatValue(result)],
                "fabs",
            )
            .map_err(|e| format!("fabs call error: {}", e))?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("fabs returned void".into()))?
            .into_float_value();
        let inf_const = f_ty.const_float(f64::INFINITY);
        let is_inf = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::OEQ, abs_val, inf_const, "is_inf")
            .map_err(|e| format!("fcmp error: {}", e))?;
        let not_finite = self
            .builder
            .build_or(is_nan, is_inf, "not_finite")
            .map_err(|e| format!("or error: {}", e))?;

        // Branch: trap or continue.
        let trap_bb = self
            .context
            .append_basic_block(function, "trap_float_builtin");
        let ok_bb = self
            .context
            .append_basic_block(function, "float_builtin_ok");
        self.builder
            .build_conditional_branch(not_finite, trap_bb, ok_bb)
            .map_err(|e| format!("br error: {}", e))?;

        // Trap block — or absorb into Fault in a fallible transition.
        self.builder.position_at_end(trap_bb);
        if self.in_fallible_multi_target() {
            self.emit_panic_fault_return("E0813")?;
        } else {
            let trap_fn = self.get_runtime_fn("mimi_trap_float_not_finite")?;
            let op_cstr = self
                .builder
                .build_global_string_ptr(op_name, "math_op_name")
                .map_err(|e| format!("global string error: {}", e))?;
            self.builder
                .build_call(
                    trap_fn,
                    &[BasicMetadataValueEnum::PointerValue(
                        op_cstr.as_pointer_value(),
                    )],
                    "",
                )
                .map_err(|e| format!("call error: {}", e))?;
            self.builder
                .build_unreachable()
                .map_err(|e| format!("unreachable error: {}", e))?;
        }

        // OK block.
        self.builder.position_at_end(ok_bb);
        Ok(result)
    }

    /// Get-or-declare the runtime's checked integer power (runtime/mod.rs:
    /// `__mimi_pow_i64`, FIX-4). Declared lazily, same pattern as operator.rs.
    fn get_or_declare_pow_i64(&self) -> inkwell::values::FunctionValue<'ctx> {
        self.module
            .get_function("__mimi_pow_i64")
            .unwrap_or_else(|| {
                let i64_ty = self.context.i64_type();
                let ty = i64_ty.fn_type(
                    &[
                        BasicMetadataTypeEnum::IntType(i64_ty),
                        BasicMetadataTypeEnum::IntType(i64_ty),
                    ],
                    false,
                );
                self.module.add_function(
                    "__mimi_pow_i64",
                    ty,
                    Some(inkwell::module::Linkage::External),
                )
            })
    }

    /// Sign-extend a narrower integer to i64 (the runtime's integer
    /// arithmetic width — the bytecode VM also computes in i64).
    fn widen_pow_arg_to_i64(&self, iv: IntValue<'ctx>, name: &str) -> MimiResult<IntValue<'ctx>> {
        if iv.get_type().get_bit_width() == 64 {
            return Ok(iv);
        }
        self.builder
            .build_int_s_extend(iv, self.context.i64_type(), name)
            .map_err(|e| format!("{} widen error: {}", name, e).into())
    }

    fn get_or_declare_fabs(&self) -> MimiResult<inkwell::values::FunctionValue<'ctx>> {
        let f64_ty = self.context.f64_type();
        Ok(self
            .module
            .get_function("llvm.fabs.f64")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "llvm.fabs.f64",
                    f64_ty.fn_type(
                        &[inkwell::types::BasicMetadataTypeEnum::FloatType(f64_ty)],
                        false,
                    ),
                    Some(inkwell::module::Linkage::External),
                )
            }))
    }

    fn expect_float_arg(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
        idx: usize,
        name: &str,
    ) -> MimiResult<FloatValue<'ctx>> {
        match args[idx] {
            BasicMetadataValueEnum::FloatValue(fv) => Ok(fv),
            BasicMetadataValueEnum::IntValue(iv) => {
                // Widen integer to float.
                Ok(self
                    .builder
                    .build_signed_int_to_float(iv, self.context.f64_type(), "arg_to_f64")
                    .map_err(|e| format!("{} arg widen error: {}", name, e))?)
            }
            _ => Err(CompileError::TypeMismatch(format!(
                "{} argument {} must be numeric",
                name,
                idx + 1
            ))),
        }
    }
}
