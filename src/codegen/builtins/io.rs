use super::CodeGenerator;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use super::super::CallSiteValueExt;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::IntPredicate;
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_println(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.is_empty() {
                    return Err(CompileError::WrongArgCount("println expects at least 1 argument".to_string()));
                }
                let i64_ty = self.context.i64_type();
                // Single string pointer: use puts (which appends newline automatically)
                if args.len() == 1 {
                    if let BasicMetadataValueEnum::PointerValue(_) = args[0] {
                        let puts = self.module.get_function("puts")
                            .ok_or_else(|| "puts not declared".to_string())?;
                        self.builder.build_call(puts, args, "puts_call")
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
                let fmt_global = self.builder.build_global_string_ptr(&fmt_str, "println_fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend(print_args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
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
                    let ptr = self.builder.build_extract_value(*sv, 0, "str_ptr")
                        .map_err(|e| CompileError::LlvmError(format!("extract str ptr: {}", e)))?;
                    match ptr {
                        BasicValueEnum::PointerValue(pv) =>
                            Ok((BasicMetadataValueEnum::PointerValue(pv), "%s".to_string())),
                        _ => Ok((BasicMetadataValueEnum::StructValue(*sv), "%p".to_string())),
                    }
                } else if num_fields >= 2 {
                    let payload = self.builder.build_extract_value(*sv, 1, "payload")
                        .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;
                    match payload {
                        BasicValueEnum::IntValue(iv) => {
                            let ext = if iv.get_type().get_bit_width() < 64 {
                                self.builder.build_int_z_extend(iv, i64_ty, "payload_zext")
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
            BasicMetadataValueEnum::PointerValue(pv) =>
                Ok((BasicMetadataValueEnum::PointerValue(*pv), "%s".to_string())),
            BasicMetadataValueEnum::IntValue(iv) =>
                Ok((BasicMetadataValueEnum::IntValue(*iv), "%ld".to_string())),
            BasicMetadataValueEnum::FloatValue(fv) =>
                Ok((BasicMetadataValueEnum::FloatValue(*fv), "%f".to_string())),
            _ => Ok((*arg, "%p".to_string())),
        }
    }

    pub(super) fn compile_print(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.is_empty() {
                    return Err(CompileError::WrongArgCount("print expects at least 1 argument".to_string()));
                }
                let i64_ty = self.context.i64_type();
                let (print_arg, fmt_spec) = self.extract_print_arg(&args[0], i64_ty)?;
                let fmt_global = self.builder.build_global_string_ptr(&fmt_spec, "fmt")
                    .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.push(print_arg);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
                    .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_eprintln(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.is_empty() {
                    return Err(CompileError::WrongArgCount("eprintln expects at least 1 argument".to_string()));
                }
                let i64_ty = self.context.i64_type();
                let (print_arg, mut fmt_spec) = self.extract_print_arg(&args[0], i64_ty)?;
                fmt_spec.push('\n');
                let fmt_global = self.builder.build_global_string_ptr(&fmt_spec, "efmt")
                    .map_err(|e| CompileError::LlvmError(format!("efmt error: {}", e)))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.push(print_arg);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "eprintf_call")
                    .map_err(|e| CompileError::LlvmError(format!("eprintf error: {}", e)))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_assert(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 {
                    return Err(CompileError::WrongArgCount("assert expects 1 argument".to_string()));
                }
                let cond = match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("assert requires boolean/i64 argument".to_string())),
                };
                let function = self.current_function().ok_or_else(|| "codegen: no current function for assert".to_string())?;
                let ok_bb = self.context.append_basic_block(function, "assert_ok");
                let fail_bb = self.context.append_basic_block(function, "assert_fail");
                self.builder.build_conditional_branch(cond, ok_bb, fail_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed\n", "assert_msg")
                    .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "assert_printf")
                    .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "assert_exit")
                    .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_assert_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 {
                    return Err(CompileError::WrongArgCount("assert_eq expects 2 arguments".to_string()));
                }
                let a = args[0];
                let b = args[1];
                let eq = match (a, b) {
                    (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "cmp")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                        let strcmp_fn = self.module.get_function("strcmp")
                            .ok_or_else(|| "strcmp not declared".to_string())?;
                        let cmp_result = self.builder.build_call(strcmp_fn, &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ], "strcmp_call")
                            .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or("strcmp returned void")?;
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "streq")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    _ => {
                        let l_ptr = self.extract_raw_str_ptr(&a).ok();
                        let r_ptr = self.extract_raw_str_ptr(&b).ok();
                        if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                            let strcmp_fn = self.module.get_function("strcmp")
                                .ok_or_else(|| "strcmp not declared".to_string())?;
                            let cmp_result = self.builder.build_call(strcmp_fn, &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ], "strcmp_call")
                                .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                                .try_as_basic_value_opt()
                                .ok_or("strcmp returned void")?;
                            let zero = self.context.i32_type().const_int(0, false);
                            self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "streq")
                                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        } else {
                            return Err(CompileError::TypeMismatch("assert_eq requires same types".to_string()));
                        }
                    },
                };
                let function = self.current_function().ok_or_else(|| "codegen: no current function for assert_eq".to_string())?;
                let ok_bb = self.context.append_basic_block(function, "aeq_ok");
                let fail_bb = self.context.append_basic_block(function, "aeq_fail");
                self.builder.build_conditional_branch(eq, ok_bb, fail_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values not equal\n", "aeq_msg")
                    .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "aeq_printf")
                    .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "aeq_exit")
                    .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_assert_ne(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 {
                    return Err(CompileError::WrongArgCount("assert_ne expects 2 arguments".to_string()));
                }
                let a = args[0];
                let b = args[1];
                let ne = match (a, b) {
                    (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::NE, l, r, "cmp")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "cmp")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                        let strcmp_fn = self.module.get_function("strcmp")
                            .ok_or_else(|| "strcmp not declared".to_string())?;
                        let cmp_result = self.builder.build_call(strcmp_fn, &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ], "strcmp_call")
                            .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or("strcmp returned void")?;
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_int_compare(inkwell::IntPredicate::NE, cmp_result.into_int_value(), zero, "strne")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    _ => {
                        let l_ptr = self.extract_raw_str_ptr(&a).ok();
                        let r_ptr = self.extract_raw_str_ptr(&b).ok();
                        if let (Some(l), Some(r)) = (l_ptr, r_ptr) {
                            let strcmp_fn = self.module.get_function("strcmp")
                                .ok_or_else(|| "strcmp not declared".to_string())?;
                            let cmp_result = self.builder.build_call(strcmp_fn, &[
                                BasicMetadataValueEnum::PointerValue(l),
                                BasicMetadataValueEnum::PointerValue(r),
                            ], "strcmp_call")
                                .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                                .try_as_basic_value_opt()
                                .ok_or("strcmp returned void")?;
                            let zero = self.context.i32_type().const_int(0, false);
                            self.builder.build_int_compare(inkwell::IntPredicate::NE, cmp_result.into_int_value(), zero, "strne")
                                .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        } else {
                            return Err(CompileError::TypeMismatch("assert_ne requires same types".to_string()));
                        }
                    },
                };
                let function = self.current_function().ok_or_else(|| "codegen: no current function for assert_ne".to_string())?;
                let ok_bb = self.context.append_basic_block(function, "ane_ok");
                let fail_bb = self.context.append_basic_block(function, "ane_fail");
                self.builder.build_conditional_branch(ne, ok_bb, fail_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values are equal\n", "ane_msg")
                    .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "ane_printf")
                    .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "ane_exit")
                    .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_assert_approx_eq(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 {
                    return Err(CompileError::WrongArgCount("assert_approx_eq expects 2 arguments".to_string()));
                }
                let a = args[0];
                let b = args[1];
                let eq = match (a, b) {
                    (BasicMetadataValueEnum::IntValue(l), BasicMetadataValueEnum::IntValue(r)) => {
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "cmp")
                            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        let diff = self.builder.build_float_sub(l, r, "diff")
                            .map_err(|e| CompileError::LlvmError(format!("fsub error: {}", e)))?;
                let fabs_fn = self.module.get_function("fabs")
                    .unwrap_or_else(|| {
                        let f64 = self.context.f64_type();
                        let ty = f64.fn_type(
                            &[inkwell::types::BasicMetadataTypeEnum::FloatType(f64)], false);
                        self.module.add_function("fabs", ty, Some(inkwell::module::Linkage::External))
                    });
                        let abs_diff = self.builder.build_call(fabs_fn, &[
                            BasicMetadataValueEnum::FloatValue(diff),
                        ], "fabs_call")
                            .map_err(|e| CompileError::LlvmError(format!("fabs error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or("fabs returned void")?
                            .into_float_value();
                        let eps = self.context.f64_type().const_float(1e-6);
                        self.builder.build_float_compare(inkwell::FloatPredicate::OLT, abs_diff, eps, "approx")
                            .map_err(|e| CompileError::LlvmError(format!("fcmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::TypeMismatch("assert_approx_eq requires same numeric types".to_string())),
                };
                let function = self.current_function().ok_or_else(|| "codegen: no current function for assert_approx_eq".to_string())?;
                let ok_bb = self.context.append_basic_block(function, "aaeq_ok");
                let fail_bb = self.context.append_basic_block(function, "aaeq_fail");
                self.builder.build_conditional_branch(eq, ok_bb, fail_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values not approximately equal\n", "aaeq_msg")
                    .map_err(|e| CompileError::LlvmError(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "aaeq_printf")
                    .map_err(|e| CompileError::LlvmError(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "aaeq_exit")
                    .map_err(|e| CompileError::LlvmError(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                self.builder.position_at_end(ok_bb);
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_input(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() > 1 { return Err(CompileError::WrongArgCount("input expects 0 or 1 argument".to_string())); }
                // Allocate buffer (4096 bytes)
                let buf_size = self.context.i64_type().const_int(4096, false);
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(buf_size),
                ], "input_malloc")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // NOTE: not registered — returned value owns the allocation
                // fgets(buf, 4096, stdin)
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                let stdin_global = self.module.add_global(
                    i8_ptr_ty, None, "stdin"
                );
                stdin_global.set_linkage(inkwell::module::Linkage::External);
                let stdin_val = self.builder.build_load(
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    stdin_global.as_pointer_value(),
                    "stdin"
                ).map_err(|e| CompileError::LlvmError(format!("load stdin error: {}", e)))?.into_pointer_value();
                let fgets_fn = self.module.get_function("fgets")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("fgets", ty, Some(inkwell::module::Linkage::External))
                    });
                self.builder.build_call(fgets_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(buf_size),
                    BasicMetadataValueEnum::PointerValue(stdin_val),
                ], "fgets_call")
                    .map_err(|e| CompileError::LlvmError(format!("fgets error: {}", e)))?;
                // strlen(buf) for string struct length
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                ], "strlen_call")
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "input_str")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let ptr_gep = self.gep().build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let len_gep = self.gep().build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_file_exists(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("file_exists expects 1 argument".to_string())); }
                let path_ptr = self.extract_raw_str_ptr(&args[0])?;
                // access(path, F_OK) where F_OK = 0
                let i32_ty = self.context.i32_type();
                let access_fn = self.module.get_function("access")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(i32_ty),
                        ], false);
                        self.module.add_function("access", ty, Some(inkwell::module::Linkage::External))
                    });
                let ret = self.builder.build_call(access_fn, &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(0, false)),
                ], "access_call")
                    .map_err(|e| CompileError::LlvmError(format!("access error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("access returned void")?;
                let zero = i32_ty.const_int(0, false);
                let cmp = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    ret.into_int_value(),
                    zero,
                    "exists"
                ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "result")
                    .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?.into();
                Ok(ext)

    }

    pub(super) fn compile_read_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("read_file expects 1 argument".to_string())); }
                let path_ptr = self.extract_raw_str_ptr(&args[0])?;
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // fopen(path, "r")
                let mode_str = self.builder.build_global_string_ptr("r", "read_mode")
                    .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
                let fopen_fn = self.module.get_function("fopen")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("fopen", ty, Some(inkwell::module::Linkage::External))
                    });
                let file = self.builder.build_call(fopen_fn, &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ], "fopen_call")
                    .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("fopen returned void")?
                    .into_pointer_value();
                // fseek(file, 0, SEEK_END)
                let i32_ty = self.context.i32_type();
                let fseek_fn = self.module.get_function("fseek")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::IntType(i32_ty),
                        ], false);
                        self.module.add_function("fseek", ty, Some(inkwell::module::Linkage::External))
                    });
                let fseek_result = self.builder.build_call(fseek_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(2, false)), // SEEK_END
                ], "fseek_call")
                    .map_err(|e| CompileError::LlvmError(format!("fseek error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("fseek returned void")?
                    .into_int_value();
                // fseek returns 0 on success, non-zero on failure; guard against negative ftell result
                let fseek_ok = self.builder.build_int_compare(
                    IntPredicate::EQ,
                    fseek_result,
                    i32_ty.const_int(0, false),
                    "fseek_ok",
                ).map_err(|e| CompileError::LlvmError(format!("fseek compare error: {}", e)))?;
                // ftell(file) -> file size (may be -1 if fseek failed)
                let ftell_fn = self.module.get_function("ftell")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("ftell", ty, Some(inkwell::module::Linkage::External))
                    });
                let file_size = self.builder.build_call(ftell_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "ftell_call")
                    .map_err(|e| CompileError::LlvmError(format!("ftell error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("ftell returned void")?
                    .into_int_value();
                // If fseek failed, clamp file_size to 0 to avoid negative malloc size
                let zero = self.context.i64_type().const_int(0, false);
                let neg_one = self.context.i64_type().const_int(
                    u64::MAX, false); // -1 in two's complement
                let is_neg_one = self.builder.build_int_compare(
                    IntPredicate::EQ,
                    file_size,
                    neg_one,
                    "is_neg_one",
                ).map_err(|e| CompileError::LlvmError(format!("neg_one compare error: {}", e)))?;
                let fseek_failed = self.builder.build_xor(fseek_ok,
                    self.context.bool_type().const_int(1, false), "fseek_failed")
                    .map_err(|e| CompileError::LlvmError(format!("xor error: {}", e)))?;
                let clamp_cond = self.builder.build_or(fseek_failed, is_neg_one, "clamp_cond")
                    .map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?;
                let file_size = self.builder.build_select(clamp_cond, zero, file_size, "file_size")
                    .map_err(|e| CompileError::LlvmError(format!("select error: {}", e)))?
                    .into_int_value();
                // rewind(file)
                let rewind_fn = self.module.get_function("rewind")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.void_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("rewind", ty, Some(inkwell::module::Linkage::External))
                    });
                self.builder.build_call(rewind_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "rewind_call")
                    .map_err(|e| CompileError::LlvmError(format!("rewind error: {}", e)))?;
                // malloc(file_size + 1)
                let one = self.context.i64_type().const_int(1, false);
                let alloc_size = self.builder.build_int_add(file_size, one, "alloc_size")
                    .map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "read_malloc")
                    .map_err(|e| CompileError::LlvmError(format!("malloc error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
                // NOTE: not registered — returned value owns the allocation
                // fread(buf, 1, file_size, file)
                let fread_fn = self.module.get_function("fread")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("fread", ty, Some(inkwell::module::Linkage::External))
                    });
                self.builder.build_call(fread_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                    BasicMetadataValueEnum::IntValue(file_size),
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fread_call")
                    .map_err(|e| CompileError::LlvmError(format!("fread error: {}", e)))?;
                // Null-terminate
                                let null_gep = unsafe {
                    self.gep().build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        buf,
                        &[file_size],
                        "null_byte"
                    )
                }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(null_gep, self.context.i8_type().const_int(0, false))
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                // fclose(file)
                let fclose_fn = self.module.get_function("fclose")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("fclose", ty, Some(inkwell::module::Linkage::External))
                    });
                self.builder.build_call(fclose_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fclose_call")
                    .map_err(|e| CompileError::LlvmError(format!("fclose error: {}", e)))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "read_str")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let ptr_gep = self.gep().build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let len_gep = self.gep().build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, file_size)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_write_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("write_file expects 2 arguments".to_string())); }
                let path_ptr = self.extract_raw_str_ptr(&args[0])?;
                let content_ptr = self.extract_raw_str_ptr(&args[1])?;
                // fopen(path, "w")
                let mode_str = self.builder.build_global_string_ptr("w", "write_mode")
                    .map_err(|e| CompileError::LlvmError(format!("global string error: {}", e)))?;
                let fopen_fn = self.module.get_function("fopen")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i8_ptr.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("fopen", ty, Some(inkwell::module::Linkage::External))
                    });
                let file = self.builder.build_call(fopen_fn, &[
                    BasicMetadataValueEnum::PointerValue(path_ptr),
                    BasicMetadataValueEnum::PointerValue(mode_str.as_pointer_value()),
                ], "fopen_call")
                    .map_err(|e| CompileError::LlvmError(format!("fopen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("fopen returned void")?
                    .into_pointer_value();
                // strlen(content) for length
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let content_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                ], "strlen_call")
                    .map_err(|e| CompileError::LlvmError(format!("strlen error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("strlen returned void")?;
                // fwrite(content, 1, len, file)
                let fwrite_fn = self.module.get_function("fwrite")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = self.context.i64_type().fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("fwrite", ty, Some(inkwell::module::Linkage::External))
                    });
                self.builder.build_call(fwrite_fn, &[
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(1, false)),
                    BasicMetadataValueEnum::IntValue(content_len.into_int_value()),
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fwrite_call")
                    .map_err(|e| CompileError::LlvmError(format!("fwrite error: {}", e)))?;
                // fclose(file)
                let i32_ty = self.context.i32_type();
                let fclose_fn = self.module.get_function("fclose")
                    .unwrap_or_else(|| {
                        let i8_ptr = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                        let ty = i32_ty.fn_type(&[
                            BasicMetadataTypeEnum::PointerType(i8_ptr),
                        ], false);
                        self.module.add_function("fclose", ty, Some(inkwell::module::Linkage::External))
                    });
                self.builder.build_call(fclose_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                ], "fclose_call")
                    .map_err(|e| CompileError::LlvmError(format!("fclose error: {}", e)))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

}
