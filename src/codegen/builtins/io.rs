use super::super::CallSiteValueExt;
use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::IntPredicate;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_println(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "println expects at least 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        // Single string pointer: use puts (which appends newline automatically)
        if args.len() == 1 {
            if let BasicMetadataValueEnum::PointerValue(_) = args[0] {
                let puts = self
                    .module
                    .get_function("puts")
                    .ok_or_else(|| "puts not declared".to_string())?;
                self.builder
                    .build_call(puts, args, "puts_call")
                    .map_err(|e| CompileError::LlvmError(format!("puts error: {}", e)))?;
                return Ok(i64_ty.const_int(0, false).into());
            }
        }
        // Build format and arg list, handling struct/enum values by extracting the payload
        let mut print_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        let mut fmt_str = String::new();
        for arg in args {
            let (print_arg, spec) = self.extract_print_arg(arg, i64_ty)?;
            print_args.push(print_arg);
            fmt_str.push_str(&spec);
        }
        fmt_str.push('\n');
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_str, "println_fmt")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.extend(print_args);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        self.builder
            .build_call(printf, &printf_args, "printf_call")
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        Ok(i64_ty.const_int(0, false).into())
    }

    fn extract_print_arg(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
    ) -> MimiResult<(BasicMetadataValueEnum<'ctx>, String)> {
        match arg {
            BasicMetadataValueEnum::StructValue(sv) => {
                let fields = sv.get_type().get_field_types();
                let num_fields = fields.len();
                // Detect Mimi string struct: {i8*, i64}
                if num_fields == 2 && matches!(fields[0], BasicTypeEnum::PointerType(_)) {
                    let ptr = self
                        .builder
                        .build_extract_value(*sv, 0, "str_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?;
                    match ptr {
                        BasicValueEnum::PointerValue(pv) => {
                            Ok((BasicMetadataValueEnum::PointerValue(pv), "%s".to_string()))
                        }
                        _ => Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string())),
                    }
                } else if num_fields >= 2 {
                    let payload = self
                        .builder
                        .build_extract_value(*sv, 1, "payload")
                        .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;
                    match payload {
                        BasicValueEnum::IntValue(iv) => {
                            let ext = if iv.get_type().get_bit_width() < 64 {
                                self.builder
                                    .build_int_z_extend(iv, i64_ty, "payload_zext")
                                    .map_err(|e| CompileError::LlvmError(e.to_string()))?
                            } else {
                                iv
                            };
                            Ok((BasicMetadataValueEnum::IntValue(ext), "%ld".to_string()))
                        }
                        _ => Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string())),
                    }
                } else {
                    Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string()))
                }
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                Ok((BasicMetadataValueEnum::PointerValue(*pv), "%s".to_string()))
            }
            BasicMetadataValueEnum::IntValue(iv) => {
                Ok((BasicMetadataValueEnum::IntValue(*iv), "%ld".to_string()))
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                Ok((BasicMetadataValueEnum::FloatValue(*fv), "%f".to_string()))
            }
            _ => Ok((*arg, "%p".to_string())),
        }
    }

    pub(super) fn compile_print(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "print expects at least 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        let (print_arg, fmt_spec) = self.extract_print_arg(&args[0], i64_ty)?;
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_spec, "fmt")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.push(print_arg);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        self.builder
            .build_call(printf, &printf_args, "printf_call")
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_eprintln(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "eprintln expects at least 1 argument".to_string(),
            ));
        }
        let i64_ty = self.context.i64_type();
        let (print_arg, mut fmt_spec) = self.extract_print_arg(&args[0], i64_ty)?;
        fmt_spec.push('\n');
        let fmt_global = self
            .builder
            .build_global_string_ptr(&fmt_spec, "efmt")
            .map_err(|e| CompileError::LlvmError(format!("efmt error: {}", e)))?;
        let mut printf_args = vec![BasicMetadataValueEnum::PointerValue(
            fmt_global.as_pointer_value(),
        )];
        printf_args.push(print_arg);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        self.builder
            .build_call(printf, &printf_args, "eprintf_call")
            .map_err(|e| CompileError::LlvmError(format!("eprintf error: {}", e)))?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() || args.len() > 2 {
            return Err(CompileError::WrongArgCount(
                "assert expects 1 or 2 arguments (condition, optional message)".to_string(),
            ));
        }
        let cond = match args[0] {
            BasicMetadataValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(
                    "assert requires boolean/i64 argument".to_string(),
                ))
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "assert_ok");
        let fail_bb = self.context.append_basic_block(function, "assert_fail");
        self.builder
            .build_conditional_branch(cond, ok_bb, fail_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        if args.len() == 2 {
            // Use custom message
            let msg_ptr = match args[1] {
                BasicMetadataValueEnum::PointerValue(pv) => pv,
                _ => {
                    return Err(CompileError::TypeMismatch(
                        "assert message argument must be a string pointer".to_string(),
                    ))
                }
            };
            self.builder
                .build_call(
                    printf,
                    &[BasicMetadataValueEnum::PointerValue(msg_ptr)],
                    "assert_printf",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        } else {
            let fmt_global = self
                .builder
                .build_global_string_ptr("assertion failed\n", "assert_msg")
                .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
            self.builder
                .build_call(
                    printf,
                    &[BasicMetadataValueEnum::PointerValue(
                        fmt_global.as_pointer_value(),
                    )],
                    "assert_printf",
                )
                .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        }
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.builder
            .build_call(
                exit_fn,
                &[BasicMetadataValueEnum::IntValue(
                    self.context.i32_type().const_int(1, false),
                )],
                "assert_exit",
            )
            .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
        self.builder
            .build_unconditional_branch(ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_eq expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let eq = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => self
                .builder
                .build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                let strcmp_fn = self
                    .module
                    .get_function("strcmp")
                    .ok_or_else(|| "strcmp not declared".to_string())?;
                let cmp_result = self
                    .builder
                    .build_call(
                        strcmp_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ],
                        "strcmp_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strcmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                self.builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        cmp_result.into_int_value(),
                        zero,
                        "streq",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            }
            _ => {
                let l_ptr = self.extract_raw_str_ptr(&a).ok();
                let r_ptr = self.extract_raw_str_ptr(&b).ok();
                if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                    let strcmp_fn = self
                        .module
                        .get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let cmp_result = self
                        .builder
                        .build_call(
                            strcmp_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ],
                            "strcmp_call",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strcmp returned void")?;
                    let zero = self.context.i32_type().const_int(0, false);
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            cmp_result.into_int_value(),
                            zero,
                            "streq",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                } else {
                    return Err(CompileError::TypeMismatch(
                        "assert_eq requires same types".to_string(),
                    ));
                }
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_eq".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "aeq_ok");
        let fail_bb = self.context.append_basic_block(function, "aeq_fail");
        self.builder
            .build_conditional_branch(eq, ok_bb, fail_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;

        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "aeq_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(
                    prefix.as_pointer_value(),
                )],
                "aeq_prefix_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;

        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " != "
        let sep = self
            .builder
            .build_global_string_ptr(" != ", "aeq_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
                "aeq_sep_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "aeq_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
                "aeq_nl_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;

        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.builder
            .build_call(
                exit_fn,
                &[BasicMetadataValueEnum::IntValue(
                    self.context.i32_type().const_int(1, false),
                )],
                "aeq_exit",
            )
            .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
        self.builder
            .build_unconditional_branch(ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_ne(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_ne expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let ne = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::NE, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => self
                .builder
                .build_float_compare(inkwell::FloatPredicate::ONE, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                let strcmp_fn = self
                    .module
                    .get_function("strcmp")
                    .ok_or_else(|| "strcmp not declared".to_string())?;
                let cmp_result = self
                    .builder
                    .build_call(
                        strcmp_fn,
                        &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ],
                        "strcmp_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strcmp returned void")?;
                let zero = self.context.i32_type().const_int(0, false);
                self.builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        cmp_result.into_int_value(),
                        zero,
                        "strne",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            }
            _ => {
                let l_ptr = self.extract_raw_str_ptr(&a).ok();
                let r_ptr = self.extract_raw_str_ptr(&b).ok();
                if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                    let strcmp_fn = self
                        .module
                        .get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let cmp_result = self
                        .builder
                        .build_call(
                            strcmp_fn,
                            &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ],
                            "strcmp_call",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("strcmp returned void")?;
                    let zero = self.context.i32_type().const_int(0, false);
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            cmp_result.into_int_value(),
                            zero,
                            "strne",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                } else {
                    return Err(CompileError::TypeMismatch(
                        "assert_ne requires same types".to_string(),
                    ));
                }
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_ne".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "ane_ok");
        let fail_bb = self.context.append_basic_block(function, "ane_fail");
        self.builder
            .build_conditional_branch(ne, ok_bb, fail_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "ane_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(
                    prefix.as_pointer_value(),
                )],
                "ane_prefix_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " == "
        let sep = self
            .builder
            .build_global_string_ptr(" == ", "ane_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
                "ane_sep_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "ane_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
                "ane_nl_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.builder
            .build_call(
                exit_fn,
                &[BasicMetadataValueEnum::IntValue(
                    self.context.i32_type().const_int(1, false),
                )],
                "ane_exit",
            )
            .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
        self.builder
            .build_unconditional_branch(ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_assert_approx_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "assert_approx_eq expects 2 arguments".to_string(),
            ));
        }
        let a = args[0];
        let b = args[1];
        let eq = match (a, b) {
            (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?,
            (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                let diff = self
                    .builder
                    .build_float_sub(l, r, "diff")
                    .map_err(|e| CompileError::LlvmError(format!("fsub error: {}", e)))?;
                let fabs_fn = self.module.get_function("fabs").unwrap_or_else(|| {
                    let f64 = self.context.f64_type();
                    let ty = f64.fn_type(
                        &[inkwell::types::BasicMetadataTypeEnum::FloatType(f64)],
                        false,
                    );
                    self.module
                        .add_function("fabs", ty, Some(inkwell::module::Linkage::External))
                });
                let abs_diff = self
                    .builder
                    .build_call(
                        fabs_fn,
                        &[BasicMetadataValueEnum::FloatValue(diff)],
                        "fabs_call",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("fabs error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("fabs returned void")?
                    .into_float_value();
                let eps = self.context.f64_type().const_float(1e-6);
                self.builder
                    .build_float_compare(inkwell::FloatPredicate::OLT, abs_diff, eps, "approx")
                    .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?
            }
            _ => {
                return Err(CompileError::TypeMismatch(
                    "assert_approx_eq requires same numeric types".to_string(),
                ))
            }
        };
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for assert_approx_eq".to_string())?;
        let ok_bb = self.context.append_basic_block(function, "aaeq_ok");
        let fail_bb = self.context.append_basic_block(function, "aaeq_fail");
        self.builder
            .build_conditional_branch(eq, ok_bb, fail_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(fail_bb);
        let printf = self
            .module
            .get_function("printf")
            .ok_or_else(|| "printf not declared".to_string())?;
        // Print "assertion failed: "
        let prefix = self
            .builder
            .build_global_string_ptr("assertion failed: ", "aaeq_prefix")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(
                    prefix.as_pointer_value(),
                )],
                "aaeq_prefix_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        // Print left value
        self.build_print_value(printf, &a)?;
        // Print " !≈ "
        let sep = self
            .builder
            .build_global_string_ptr(" !≈ ", "aaeq_sep")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(sep.as_pointer_value())],
                "aaeq_sep_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        // Print right value
        self.build_print_value(printf, &b)?;
        // Print newline
        let nl = self
            .builder
            .build_global_string_ptr("\n", "aaeq_nl")
            .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
        self.builder
            .build_call(
                printf,
                &[BasicMetadataValueEnum::PointerValue(nl.as_pointer_value())],
                "aaeq_nl_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
        let exit_fn = self
            .module
            .get_function("exit")
            .ok_or_else(|| "exit not declared".to_string())?;
        self.builder
            .build_call(
                exit_fn,
                &[BasicMetadataValueEnum::IntValue(
                    self.context.i32_type().const_int(1, false),
                )],
                "aaeq_exit",
            )
            .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
        self.builder
            .build_unconditional_branch(ok_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(ok_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_input(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() > 1 {
            return Err(CompileError::WrongArgCount(
                "input expects 0 or 1 argument".to_string(),
            ));
        }
        // Allocate buffer (4096 bytes)
        let buf_size = self.context.i64_type().const_int(4096, false);
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let buf = self
            .builder
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(buf_size)],
                "input_malloc",
            )
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        // NOTE: not registered — returned value owns the allocation
        // fgets(buf, 4096, stdin)
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let stdin_global = self.module.add_global(i8_ptr_ty, None, "stdin");
        stdin_global.set_linkage(inkwell::module::Linkage::External);
        let stdin_val = self
            .builder
            .build_load(
                BasicTypeEnum::PointerType(i8_ptr_ty),
                stdin_global.as_pointer_value(),
                "stdin",
            )
            .map_err(|e| CompileError::LlvmError(format!("load stdin error: {}", e)))?
            .into_pointer_value();
        let fgets_fn = self.module.get_function("fgets").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fgets", ty, Some(inkwell::module::Linkage::External))
        });
        self.builder
            .build_call(
                fgets_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(buf_size),
                    BasicMetadataValueEnum::PointerValue(stdin_val),
                ],
                "fgets_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fgets error: {}", e)))?;
        // strlen(buf) for string struct length
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let str_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(buf)],
                "strlen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?;
        // Build string struct { i8*, i64 }
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self
            .builder
            .build_alloca(string_ty, "input_str")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(ptr_gep, buf)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(len_gep, str_len)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(str_alloca.into())
    }

    pub(super) fn compile_file_exists(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "file_exists expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        // access(path, F_OK) where F_OK = 0
        let i32_ty = self.context.i32_type();
        let access_fn = self.module.get_function("access").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(i32_ty),
                ],
                false,
            );
            self.module
                .add_function("access", ty, Some(inkwell::module::Linkage::External))
        });
        let ret = self
            .builder
            .build_call(
                access_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(0, false)),
                ],
                "access_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("access error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("access returned void")?;
        let zero = i32_ty.const_int(0, false);
        let cmp = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                ret.into_int_value(),
                zero,
                "exists",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let ext: BasicValueEnum = self
            .builder
            .build_int_z_extend(cmp, self.context.i64_type(), "result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
            .into();
        Ok(ext)
    }

    pub(super) fn compile_read_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(
                "read_file expects 1 argument".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        // fopen(path, "r")
        let mode_str = self
            .builder
            .build_global_string_ptr("r", "read_mode")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let fopen_fn = self.module.get_function("fopen").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fopen", ty, Some(inkwell::module::Linkage::External))
        });
        let file = self
            .builder
            .build_call(
                fopen_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ],
                "fopen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fopen returned void")?
            .into_pointer_value();
        // fseek(file, 0, SEEK_END)
        let i32_ty = self.context.i32_type();
        let fseek_fn = self.module.get_function("fseek").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::IntType(i32_ty),
                ],
                false,
            );
            self.module
                .add_function("fseek", ty, Some(inkwell::module::Linkage::External))
        });
        let fseek_result = self
            .builder
            .build_call(
                fseek_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(file),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(2, false)), // SEEK_END
                ],
                "fseek_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fseek error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fseek returned void")?
            .into_int_value();
        // fseek returns 0 on success, non-zero on failure; guard against negative ftell result
        let fseek_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                fseek_result,
                i32_ty.const_int(0, false),
                "fseek_ok",
            )
            .map_err(|e| CompileError::LlvmError(format!("fseek compare error: {}", e)))?;
        // ftell(file) -> file size (may be -1 if fseek failed)
        let ftell_fn = self.module.get_function("ftell").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = self
                .context
                .i64_type()
                .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
            self.module
                .add_function("ftell", ty, Some(inkwell::module::Linkage::External))
        });
        let file_size = self
            .builder
            .build_call(
                ftell_fn,
                &[BasicMetadataValueEnum::PointerValue(file)],
                "ftell_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("ftell error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("ftell returned void")?
            .into_int_value();
        // If fseek failed, clamp file_size to 0 to avoid negative malloc size
        let zero = self.context.i64_type().const_int(0, false);
        let neg_one = self.context.i64_type().const_int(u64::MAX, false); // -1 in two's complement
        let is_neg_one = self
            .builder
            .build_int_compare(IntPredicate::EQ, file_size, neg_one, "is_neg_one")
            .map_err(|e| CompileError::LlvmError(format!("neg_one compare error: {}", e)))?;
        let fseek_failed = self
            .builder
            .build_xor(
                fseek_ok,
                self.context.bool_type().const_int(1, false),
                "fseek_failed",
            )
            .map_err(|e| CompileError::LlvmError(format!("xor error: {}", e)))?;
        let clamp_cond = self
            .builder
            .build_or(fseek_failed, is_neg_one, "clamp_cond")
            .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
        let file_size = self
            .builder
            .build_select(clamp_cond, zero, file_size, "file_size")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // rewind(file)
        let rewind_fn = self.module.get_function("rewind").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = self
                .context
                .void_type()
                .fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
            self.module
                .add_function("rewind", ty, Some(inkwell::module::Linkage::External))
        });
        self.builder
            .build_call(
                rewind_fn,
                &[BasicMetadataValueEnum::PointerValue(file)],
                "rewind_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("rewind error: {}", e)))?;
        // malloc(file_size + 1)
        let one = self.context.i64_type().const_int(1, false);
        let alloc_size = self
            .builder
            .build_int_add(file_size, one, "alloc_size")
            .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
        let malloc_fn = self
            .module
            .get_function("malloc")
            .ok_or_else(|| "malloc not declared".to_string())?;
        let buf = self
            .builder
            .build_call(
                malloc_fn,
                &[BasicMetadataValueEnum::IntValue(alloc_size)],
                "read_malloc",
            )
            .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("malloc returned void")?
            .into_pointer_value();
        // NOTE: not registered — returned value owns the allocation
        // fread(buf, 1, file_size, file)
        let fread_fn = self.module.get_function("fread").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = self.context.i64_type().fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fread", ty, Some(inkwell::module::Linkage::External))
        });
        self.builder
            .build_call(
                fread_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                    BasicMetadataValueEnum::IntValue(file_size),
                    BasicMetadataValueEnum::PointerValue(file),
                ],
                "fread_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fread error: {}", e)))?;
        // Null-terminate
        let null_gep = {
            self.gep().build_in_bounds_gep(
                BasicTypeEnum::IntType(self.context.i8_type()),
                buf,
                &[file_size],
                "null_byte",
            )
        }
        .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(null_gep, self.context.i8_type().const_int(0, false))
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        // fclose(file)
        let fclose_fn = self.module.get_function("fclose").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
            self.module
                .add_function("fclose", ty, Some(inkwell::module::Linkage::External))
        });
        self.builder
            .build_call(
                fclose_fn,
                &[BasicMetadataValueEnum::PointerValue(file)],
                "fclose_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fclose error: {}", e)))?;
        // Build string struct { i8*, i64 }
        let string_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let str_alloca = self
            .builder
            .build_alloca(string_ty, "read_str")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(ptr_gep, buf)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let len_gep = self
            .gep()
            .build_struct_gep(string_ty, str_alloca, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder
            .build_store(len_gep, file_size)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        Ok(str_alloca.into())
    }

    pub(super) fn compile_write_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount(
                "write_file expects 2 arguments".to_string(),
            ));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let content_ptr = self.extract_raw_str_ptr(&args[1])?;
        // fopen(path, "w")
        let mode_str = self
            .builder
            .build_global_string_ptr("w", "write_mode")
            .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
        let fopen_fn = self.module.get_function("fopen").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i8_ptr.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fopen", ty, Some(inkwell::module::Linkage::External))
        });
        let file = self
            .builder
            .build_call(
                fopen_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ],
                "fopen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("fopen returned void")?
            .into_pointer_value();
        // strlen(content) for length
        let strlen_fn = self
            .module
            .get_function("strlen")
            .ok_or_else(|| "strlen not declared".to_string())?;
        let content_len = self
            .builder
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(content_ptr)],
                "strlen_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("strlen returned void")?;
        // fwrite(content, 1, len, file)
        let fwrite_fn = self.module.get_function("fwrite").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = self.context.i64_type().fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::PointerType(i8_ptr),
                ],
                false,
            );
            self.module
                .add_function("fwrite", ty, Some(inkwell::module::Linkage::External))
        });
        self.builder
            .build_call(
                fwrite_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                    BasicMetadataValueEnum::IntValue(content_len.into_int_value()),
                    BasicMetadataValueEnum::PointerValue(file),
                ],
                "fwrite_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fwrite error: {}", e)))?;
        // fclose(file)
        let i32_ty = self.context.i32_type();
        let fclose_fn = self.module.get_function("fclose").unwrap_or_else(|| {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            let ty = i32_ty.fn_type(&[BasicMetadataTypeEnum::PointerType(i8_ptr)], false);
            self.module
                .add_function("fclose", ty, Some(inkwell::module::Linkage::External))
        });
        self.builder
            .build_call(
                fclose_fn,
                &[BasicMetadataValueEnum::PointerValue(file)],
                "fclose_call",
            )
            .map_err(|e| CompileError::LlvmError(format!("fclose error: {}", e)))?;
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Print a single value to stdout for assert_eq diagnostics.
    fn build_print_value(
        &self,
        printf: FunctionValue<'ctx>,
        val: &BasicMetadataValueEnum<'ctx>,
    ) -> Result<(), CompileError> {
        match val {
            BasicMetadataValueEnum::IntValue(iv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%lld", "int_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.builder
                    .build_call(
                        printf,
                        &[
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::IntValue(*iv),
                        ],
                        "print_int",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::FloatValue(fv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%f", "float_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.builder
                    .build_call(
                        printf,
                        &[
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::FloatValue(*fv),
                        ],
                        "print_float",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::PointerValue(pv) => {
                let fmt = self
                    .builder
                    .build_global_string_ptr("%s", "str_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                self.builder
                    .build_call(
                        printf,
                        &[
                            BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                            BasicMetadataValueEnum::PointerValue(*pv),
                        ],
                        "print_str",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                if let Ok(BasicValueEnum::PointerValue(pv)) =
                    self.builder.build_extract_value(*sv, 0, "str_field")
                {
                    let fmt = self
                        .builder
                        .build_global_string_ptr("%s", "struct_str_fmt")
                        .map_err(|e| CompileError::LlvmError(format!("fmt: {}", e)))?;
                    self.builder
                        .build_call(
                            printf,
                            &[
                                BasicMetadataValueEnum::PointerValue(fmt.as_pointer_value()),
                                BasicMetadataValueEnum::PointerValue(pv),
                            ],
                            "print_struct_str",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("printf: {}", e)))?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // === Directory & path operations (codegen) ===

    fn call_runtime_str_to_bool(
        &self,
        runtime_fn_name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(format!("{} expects 1 argument", runtime_fn_name)));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self.module.get_function(runtime_fn_name)
            .ok_or_else(|| CompileError::LlvmError(format!("{} not declared", runtime_fn_name)))?;
        let result = self.builder.build_call(
            fn_val,
            &[BasicMetadataValueEnum::PointerValue(path_ptr)],
            &format!("{}_call", runtime_fn_name),
        ).map_err(|e| CompileError::LlvmError(format!("{}: {}", runtime_fn_name, e)))?;
        let ret = result.try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", runtime_fn_name)))?;
        Ok(ret)
    }

    fn call_runtime_str_to_str(
        &self,
        runtime_fn_name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount(format!("{} expects 1 argument", runtime_fn_name)));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self.module.get_function(runtime_fn_name)
            .ok_or_else(|| CompileError::LlvmError(format!("{} not declared", runtime_fn_name)))?;
        let result = self.builder.build_call(
            fn_val,
            &[BasicMetadataValueEnum::PointerValue(path_ptr)],
            &format!("{}_call", runtime_fn_name),
        ).map_err(|e| CompileError::LlvmError(format!("{}: {}", runtime_fn_name, e)))?;
        let raw_ptr = result.try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError(format!("{} returned void", runtime_fn_name)))?;
        // Wrap raw C string into Mimi string struct {ptr, len}
        self.wrap_c_string(raw_ptr.into_pointer_value())
    }

    pub(super) fn compile_listdir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount("listdir expects 1 argument".to_string()));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self.module.get_function("mimi_listdir")
            .ok_or("mimi_listdir not declared")?;
        let result = self.builder.build_call(
            fn_val,
            &[BasicMetadataValueEnum::PointerValue(path_ptr)],
            "listdir_call",
        ).map_err(|e| CompileError::LlvmError(format!("listdir: {}", e)))?;
        let list_ptr = result.try_as_basic_value_opt()
            .ok_or("listdir returned void")?;
        // Return as opaque pointer (MimiList*)
        Ok(list_ptr)
    }

    pub(super) fn compile_is_dir(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_is_dir", args)
    }

    pub(super) fn compile_is_file(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_is_file", args)
    }

    pub(super) fn compile_path_join(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount("path_join expects 2 arguments".to_string()));
        }
        let a_ptr = self.extract_raw_str_ptr(&args[0])?;
        let b_ptr = self.extract_raw_str_ptr(&args[1])?;
        let fn_val = self.module.get_function("mimi_path_join")
            .ok_or("mimi_path_join not declared")?;
        let result = self.builder.build_call(
            fn_val,
            &[
                BasicMetadataValueEnum::PointerValue(a_ptr),
                BasicMetadataValueEnum::PointerValue(b_ptr),
            ],
            "path_join_call",
        ).map_err(|e| CompileError::LlvmError(format!("path_join: {}", e)))?;
        let raw_ptr = result.try_as_basic_value_opt()
            .ok_or("path_join returned void")?;
        self.wrap_c_string(raw_ptr.into_pointer_value())
    }

    pub(super) fn compile_path_ext(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_ext", args)
    }

    pub(super) fn compile_path_basename(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_basename", args)
    }

    pub(super) fn compile_path_dirname(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_path_dirname", args)
    }

    pub(super) fn compile_walk_dir(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount("walk_dir expects 1 argument".to_string()));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let fn_val = self.module.get_function("mimi_walk_dir")
            .ok_or("mimi_walk_dir not declared")?;
        let result = self.builder.build_call(
            fn_val,
            &[BasicMetadataValueEnum::PointerValue(path_ptr)],
            "walk_dir_call",
        ).map_err(|e| CompileError::LlvmError(format!("walk_dir: {}", e)))?;
        let list_ptr = result.try_as_basic_value_opt()
            .ok_or("walk_dir returned void")?;
        Ok(list_ptr)
    }

    pub(super) fn compile_mkdir_p(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_mkdir_p", args)
    }

    pub(super) fn compile_remove_file(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_bool("mimi_remove_file", args)
    }

    // === Process & advanced file operations (codegen) ===

    pub(super) fn compile_exec(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount("exec expects 1 argument".to_string()));
        }
        let cmd_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());

        // Call mimi_exec(cmd) -> MimiExecResult*
        let exec_fn = self.module.get_function("mimi_exec")
            .ok_or_else(|| "mimi_exec not declared".to_string())?;
        let res_ptr = self.builder.build_call(
            exec_fn,
            &[BasicMetadataValueEnum::PointerValue(cmd_ptr)],
            "exec_call",
        ).map_err(|e| CompileError::LlvmError(format!("exec error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_exec returned void")?
            .into_pointer_value();

        // MimiExecResult layout: { i64 exit_code, i8* stdout, i8* stderr }
        let res_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
            ],
            false,
        );

        // Extract exit_code
        let exit_gep = self.gep().build_struct_gep(res_ty, res_ptr, 0, "exit_code_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let exit_code_raw = self.builder.build_load(self.context.i64_type(), exit_gep, "exit_code_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        // Truncate to i32 for ExecResult.exit_code field
        let exit_code = self.builder.build_int_truncate(exit_code_raw, self.context.i32_type(), "exit_code_i32")
            .map_err(|e| CompileError::LlvmError(format!("trunc error: {}", e)))?;

        // Extract stdout
        let stdout_gep = self.gep().build_struct_gep(res_ty, res_ptr, 1, "stdout_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stdout_raw = self.builder.build_load(i8_ptr, stdout_gep, "stdout_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stdout_str = self.wrap_c_string(stdout_raw)?;

        // Extract stderr
        let stderr_gep = self.gep().build_struct_gep(res_ty, res_ptr, 2, "stderr_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let stderr_raw = self.builder.build_load(i8_ptr, stderr_gep, "stderr_raw")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_pointer_value();
        let stderr_str = self.wrap_c_string(stderr_raw)?;

        // Free the runtime struct (not the strings — they're owned by ExecResult)
        let free_struct_fn = self.module.get_function("mimi_exec_free_struct")
            .ok_or_else(|| "mimi_exec_free_struct not declared".to_string())?;
        self.builder.build_call(
            free_struct_fn,
            &[BasicMetadataValueEnum::PointerValue(res_ptr)],
            "exec_free_struct",
        ).map_err(|e| CompileError::LlvmError(format!("exec_free_struct error: {}", e)))?;

        // Build ExecResult LLVM struct { i32, {i8*,i64}, {i8*,i64} }
        let string_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::PointerType(i8_ptr),
                inkwell::types::BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );
        let exec_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(self.context.i32_type()),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
                inkwell::types::BasicTypeEnum::StructType(string_ty),
            ],
            false,
        );
        let alloca = self.builder.build_alloca(exec_result_ty, "exec_result")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;

        // Store exit_code
        let f0 = self.gep().build_struct_gep(exec_result_ty, alloca, 0, "f0")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(f0, exit_code)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Store stdout string
        let f1 = self.gep().build_struct_gep(exec_result_ty, alloca, 1, "f1")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(f1, stdout_str)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Store stderr string
        let f2 = self.gep().build_struct_gep(exec_result_ty, alloca, 2, "f2")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(f2, stderr_str)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        Ok(alloca.into())
    }

    pub(super) fn compile_file_stat(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 1 {
            return Err(CompileError::WrongArgCount("file_stat expects 1 argument".to_string()));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();

        // Allocate err_out pointer
        let err_alloca = self.builder.build_alloca(i8_ptr, "err_out")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
        self.builder.build_store(err_alloca, i8_ptr.const_null())
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Call mimi_file_stat(path, &err_out)
        let stat_fn = self.module.get_function("mimi_file_stat")
            .ok_or_else(|| "mimi_file_stat not declared".to_string())?;
        let stat_ptr = self.builder.build_call(
            stat_fn,
            &[
                BasicMetadataValueEnum::PointerValue(path_ptr),
                BasicMetadataValueEnum::PointerValue(err_alloca),
            ],
            "stat_call",
        ).map_err(|e| CompileError::LlvmError(format!("file_stat error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_file_stat returned void")?
            .into_pointer_value();

        // MimiStatResult layout: { i64 size, i64 modified, i64 is_file, i64 is_dir }
        let mimi_stat_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        // Check if stat_ptr is null (error case)
        let is_null = self.builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            stat_ptr,
            i8_ptr.const_null(),
            "stat_null",
        ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;

        // Build StatResult LLVM struct { i64, i64, i1, i1 }
        let bool_ty = self.context.bool_type();
        let stat_result_ty = self.context.struct_type(
            &[
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(i64_ty),
                inkwell::types::BasicTypeEnum::IntType(bool_ty),
                inkwell::types::BasicTypeEnum::IntType(bool_ty),
            ],
            false,
        );
        let alloca = self.builder.build_alloca(stat_result_ty, "stat_result")
            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;

        // Extract fields from MimiStatResult (or use defaults if null)
        let zero_i64 = i64_ty.const_int(0, false);
        let neg_one_i64 = i64_ty.const_int((-1i64) as u64, false);
        let false_val = bool_ty.const_int(0, false);

        // size
        let size_gep = self.gep().build_struct_gep(mimi_stat_ty, stat_ptr, 0, "size_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let size_loaded = self.builder.build_load(i64_ty, size_gep, "size_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let size_val = self.builder.build_select(is_null, neg_one_i64, size_loaded, "size_sel")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // modified
        let mod_gep = self.gep().build_struct_gep(mimi_stat_ty, stat_ptr, 1, "mod_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let mod_loaded = self.builder.build_load(i64_ty, mod_gep, "mod_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let mod_val = self.builder.build_select(is_null, zero_i64, mod_loaded, "mod_sel")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // is_file
        let isf_gep = self.gep().build_struct_gep(mimi_stat_ty, stat_ptr, 2, "isf_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let isf_raw = self.builder.build_load(i64_ty, isf_gep, "isf_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let isf_bool = self.builder.build_int_compare(
            inkwell::IntPredicate::NE, isf_raw, zero_i64, "isf_cmp",
        ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let isf_val = self.builder.build_select(is_null, false_val, isf_bool, "isf_sel")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();
        // is_dir
        let isd_gep = self.gep().build_struct_gep(mimi_stat_ty, stat_ptr, 3, "isd_ptr")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let isd_raw = self.builder.build_load(i64_ty, isd_gep, "isd_loaded")
            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?
            .into_int_value();
        let isd_bool = self.builder.build_int_compare(
            inkwell::IntPredicate::NE, isd_raw, zero_i64, "isd_cmp",
        ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let isd_val = self.builder.build_select(is_null, false_val, isd_bool, "isd_sel")
            .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
            .into_int_value();

        // Store into StatResult struct
        let s0 = self.gep().build_struct_gep(stat_result_ty, alloca, 0, "s0")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(s0, size_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let s1 = self.gep().build_struct_gep(stat_result_ty, alloca, 1, "s1")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(s1, mod_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let s2 = self.gep().build_struct_gep(stat_result_ty, alloca, 2, "s2")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(s2, isf_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
        let s3 = self.gep().build_struct_gep(stat_result_ty, alloca, 3, "s3")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.builder.build_store(s3, isd_val)
            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;

        // Free the stat result (uses Rust allocator via Box::from_raw)
        let free_fn = self.module.get_function("mimi_file_stat_free")
            .ok_or_else(|| "mimi_file_stat_free not declared".to_string())?;
        self.builder.build_call(
            free_fn,
            &[BasicMetadataValueEnum::PointerValue(stat_ptr)],
            "stat_free",
        ).map_err(|e| CompileError::LlvmError(format!("free error: {}", e)))?;

        Ok(alloca.into())
    }

    pub(super) fn compile_append_file(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount("append_file expects 2 arguments".to_string()));
        }
        let path_ptr = self.extract_raw_str_ptr(&args[0])?;
        let content_ptr = self.extract_raw_str_ptr(&args[1])?;

        let append_fn = self.module.get_function("mimi_append_file")
            .ok_or_else(|| "mimi_append_file not declared".to_string())?;
        let ret = self.builder.build_call(
            append_fn,
            &[
                BasicMetadataValueEnum::PointerValue(path_ptr),
                BasicMetadataValueEnum::PointerValue(content_ptr),
            ],
            "append_call",
        ).map_err(|e| CompileError::LlvmError(format!("append_file error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_append_file returned void")?
            .into_int_value();

        // Convert i64 to bool (i64): ret != 0
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self.builder.build_int_compare(
            inkwell::IntPredicate::NE, ret, zero, "append_ok",
        ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let result = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "append_result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    pub(super) fn compile_set_env(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.len() != 2 {
            return Err(CompileError::WrongArgCount("set_env expects 2 arguments".to_string()));
        }
        let key_ptr = self.extract_raw_str_ptr(&args[0])?;
        let val_ptr = self.extract_raw_str_ptr(&args[1])?;

        let set_fn = self.module.get_function("mimi_set_env")
            .ok_or_else(|| "mimi_set_env not declared".to_string())?;
        let ret = self.builder.build_call(
            set_fn,
            &[
                BasicMetadataValueEnum::PointerValue(key_ptr),
                BasicMetadataValueEnum::PointerValue(val_ptr),
            ],
            "set_env_call",
        ).map_err(|e| CompileError::LlvmError(format!("set_env error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_set_env returned void")?
            .into_int_value();

        // Convert i64 to bool (i64): ret != 0
        let zero = self.context.i64_type().const_int(0, false);
        let cmp = self.builder.build_int_compare(
            inkwell::IntPredicate::NE, ret, zero, "set_env_ok",
        ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
        let result = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "set_env_result")
            .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?;
        Ok(result.into())
    }

    // === Crypto operations (codegen) ===

    pub(super) fn compile_sha256(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_sha256", args)
    }

    pub(super) fn compile_base64_encode(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_base64_encode", args)
    }

    pub(super) fn compile_base64_decode(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        self.call_runtime_str_to_str("mimi_base64_decode", args)
    }

    pub(super) fn compile_format(&self, args: &[BasicMetadataValueEnum<'ctx>]) -> MimiResult<BasicValueEnum<'ctx>> {
        if args.is_empty() {
            return Err(CompileError::WrongArgCount(
                "format expects at least 1 argument (template string)".to_string(),
            ));
        }
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // Convert all arguments to string pointers
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        // First arg: number of format arguments
        call_args.push(BasicMetadataValueEnum::IntValue(
            i64_ty.const_int((args.len() - 1) as u64, false),
        ));
        // Second arg: template string
        match &args[0] {
            BasicMetadataValueEnum::PointerValue(pv) => {
                call_args.push(BasicMetadataValueEnum::PointerValue(*pv));
            }
            _ => return Err(CompileError::TypeMismatch(
                "format: first arg must be a string template".to_string(),
            )),
        }
        // Remaining args: convert to string pointers (up to 8)
        for i in 1..args.len().min(9) {
            match &args[i] {
                BasicMetadataValueEnum::PointerValue(pv) => {
                    call_args.push(BasicMetadataValueEnum::PointerValue(*pv));
                }
                BasicMetadataValueEnum::IntValue(iv) => {
                    let to_i64_fn = self.module.get_function("mimi_to_string_i64")
                        .ok_or_else(|| "mimi_to_string_i64 not declared".to_string())?;
                    let str_result = self.builder.build_call(
                        to_i64_fn,
                        &[BasicMetadataValueEnum::IntValue(*iv)],
                        "to_str_i64",
                    ).map_err(|e| CompileError::LlvmError(format!("to_string error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_to_string_i64 returned void")?
                        .into_pointer_value();
                    call_args.push(BasicMetadataValueEnum::PointerValue(str_result));
                }
                BasicMetadataValueEnum::FloatValue(fv) => {
                    let to_f64_fn = self.module.get_function("mimi_to_string_f64")
                        .ok_or_else(|| "mimi_to_string_f64 not declared".to_string())?;
                    let str_result = self.builder.build_call(
                        to_f64_fn,
                        &[BasicMetadataValueEnum::FloatValue(*fv)],
                        "to_str_f64",
                    ).map_err(|e| CompileError::LlvmError(format!("to_string error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("mimi_to_string_f64 returned void")?
                        .into_pointer_value();
                    call_args.push(BasicMetadataValueEnum::PointerValue(str_result));
                }
                _ => {
                    call_args.push(BasicMetadataValueEnum::PointerValue(i8_ptr.const_null()));
                }
            }
        }
        // Pad with null pointers if less than 8 args
        while call_args.len() < 10 {
            call_args.push(BasicMetadataValueEnum::PointerValue(
                i8_ptr.const_null(),
            ));
        }
        let format_fn = self.module.get_function("mimi_str_format")
            .ok_or_else(|| "mimi_str_format not declared".to_string())?;
        let result = self.builder.build_call(
            format_fn,
            &call_args,
            "format_call",
        ).map_err(|e| CompileError::LlvmError(format!("format error: {}", e)))?
            .try_as_basic_value_opt()
            .ok_or("mimi_str_format returned void")?;
        Ok(result)
    }
}
