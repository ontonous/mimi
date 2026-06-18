use super::CodeGenerator;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use crate::error::{CompileError, MimiResult};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_println(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.is_empty() {
                    return Err(CompileError::WrongArgCount("println expects at least 1 argument".to_string()));
                }
                // For string args: call puts (raw C string pointer)
                // For integer args: call printf with "%ld\n"
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => {
                        let puts = self.module.get_function("puts")
                            .ok_or_else(|| "puts not declared".to_string())?;
                        self.builder.build_call(puts, args, "puts_call")
                            .map_err(|e| CompileError::Generic(format!("puts error: {}", e)))?;
                        return Ok(self.context.i64_type().const_int(0, false).into());
                    }
                    BasicMetadataValueEnum::IntValue(_) => "%ld\n",
                    BasicMetadataValueEnum::FloatValue(_) => "%f\n",
                    _ => "%p\n",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "fmt")
                    .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
                    .map_err(|e| CompileError::Generic(format!("printf error: {}", e)))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_print(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.is_empty() {
                    return Err(CompileError::WrongArgCount("print expects at least 1 argument".to_string()));
                }
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => "%s",
                    BasicMetadataValueEnum::IntValue(_) => "%ld",
                    BasicMetadataValueEnum::FloatValue(_) => "%f",
                    _ => "%p",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "fmt")
                    .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "printf_call")
                    .map_err(|e| CompileError::Generic(format!("printf error: {}", e)))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

    pub(super) fn compile_eprintln(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.is_empty() {
                    return Err(CompileError::WrongArgCount("eprintln expects at least 1 argument".to_string()));
                }
                let fmt_str = match args[0] {
                    BasicMetadataValueEnum::PointerValue(_) => "%s\n",
                    BasicMetadataValueEnum::IntValue(_) => "%ld\n",
                    BasicMetadataValueEnum::FloatValue(_) => "%f\n",
                    _ => "%p\n",
                };
                let fmt_global = self.builder.build_global_string_ptr(fmt_str, "efmt")
                    .map_err(|e| CompileError::Generic(format!("efmt error: {}", e)))?;
                let mut printf_args = vec![
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ];
                printf_args.extend_from_slice(args);
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &printf_args, "eprintf_call")
                    .map_err(|e| CompileError::Generic(format!("eprintf error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed\n", "assert_msg")
                    .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "assert_printf")
                    .map_err(|e| CompileError::Generic(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "assert_exit")
                    .map_err(|e| CompileError::Generic(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;

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
                            .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "cmp")
                            .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                        let strcmp_fn = self.module.get_function("strcmp")
                            .ok_or_else(|| "strcmp not declared".to_string())?;
                        let cmp_result = self.builder.build_call(strcmp_fn, &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ], "strcmp_call")
                            .map_err(|e| CompileError::Generic(format!("strcmp error: {}", e)))?
                            .try_as_basic_value().left()
                            .ok_or("strcmp returned void")?;
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp_result.into_int_value(), zero, "streq")
                            .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::TypeMismatch("assert_eq requires same types".to_string())),
                };
                let function = self.current_function().ok_or_else(|| "codegen: no current function for assert_eq".to_string())?;
                let ok_bb = self.context.append_basic_block(function, "aeq_ok");
                let fail_bb = self.context.append_basic_block(function, "aeq_fail");
                self.builder.build_conditional_branch(eq, ok_bb, fail_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values not equal\n", "aeq_msg")
                    .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "aeq_printf")
                    .map_err(|e| CompileError::Generic(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "aeq_exit")
                    .map_err(|e| CompileError::Generic(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;

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
                            .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "cmp")
                            .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::PointerValue(l), BasicMetadataValueEnum::PointerValue(r)) => {
                        let strcmp_fn = self.module.get_function("strcmp")
                            .ok_or_else(|| "strcmp not declared".to_string())?;
                        let cmp_result = self.builder.build_call(strcmp_fn, &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ], "strcmp_call")
                            .map_err(|e| CompileError::Generic(format!("strcmp error: {}", e)))?
                            .try_as_basic_value().left()
                            .ok_or("strcmp returned void")?;
                        let zero = self.context.i32_type().const_int(0, false);
                        self.builder.build_int_compare(inkwell::IntPredicate::NE, cmp_result.into_int_value(), zero, "strne")
                            .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::TypeMismatch("assert_ne requires same types".to_string())),
                };
                let function = self.current_function().ok_or_else(|| "codegen: no current function for assert_ne".to_string())?;
                let ok_bb = self.context.append_basic_block(function, "ane_ok");
                let fail_bb = self.context.append_basic_block(function, "ane_fail");
                self.builder.build_conditional_branch(ne, ok_bb, fail_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;

                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values are equal\n", "ane_msg")
                    .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "ane_printf")
                    .map_err(|e| CompileError::Generic(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "ane_exit")
                    .map_err(|e| CompileError::Generic(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;

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
                            .map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?
                    }
                    (BasicMetadataValueEnum::FloatValue(l), BasicMetadataValueEnum::FloatValue(r)) => {
                        let diff = self.builder.build_float_sub(l, r, "diff")
                            .map_err(|e| CompileError::Generic(format!("fsub error: {}", e)))?;
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
                            .map_err(|e| CompileError::Generic(format!("fabs error: {}", e)))?
                            .try_as_basic_value().left()
                            .ok_or("fabs returned void")?
                            .into_float_value();
                        let eps = self.context.f64_type().const_float(1e-6);
                        self.builder.build_float_compare(inkwell::FloatPredicate::OLT, abs_diff, eps, "approx")
                            .map_err(|e| CompileError::Generic(format!("fcmp error: {}", e)))?
                    }
                    _ => return Err(CompileError::TypeMismatch("assert_approx_eq requires same numeric types".to_string())),
                };
                let function = self.current_function().ok_or_else(|| "codegen: no current function for assert_approx_eq".to_string())?;
                let ok_bb = self.context.append_basic_block(function, "aaeq_ok");
                let fail_bb = self.context.append_basic_block(function, "aaeq_fail");
                self.builder.build_conditional_branch(eq, ok_bb, fail_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
                self.builder.position_at_end(fail_bb);
                let fmt_global = self.builder.build_global_string_ptr("assertion failed: values not approximately equal\n", "aaeq_msg")
                    .map_err(|e| CompileError::Generic(format!("fmt error: {}", e)))?;
                let printf = self.module.get_function("printf")
                    .ok_or_else(|| "printf not declared".to_string())?;
                self.builder.build_call(printf, &[
                    BasicMetadataValueEnum::PointerValue(fmt_global.as_pointer_value()),
                ], "aaeq_printf")
                    .map_err(|e| CompileError::Generic(format!("printf error: {}", e)))?;
                let exit_fn = self.module.get_function("exit")
                    .ok_or_else(|| "exit not declared".to_string())?;
                self.builder.build_call(exit_fn, &[
                    BasicMetadataValueEnum::IntValue(self.context.i32_type().const_int(1, false)),
                ], "aaeq_exit")
                    .map_err(|e| CompileError::Generic(format!("exit error: {}", e)))?;
                self.builder.build_unconditional_branch(ok_bb)
                    .map_err(|e| CompileError::Generic(format!("branch error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
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
                ).map_err(|e| CompileError::Generic(format!("load stdin error: {}", e)))?.into_pointer_value();
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
                    .map_err(|e| CompileError::Generic(format!("fgets error: {}", e)))?;
                // strlen(buf) for string struct length
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let str_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(buf),
                ], "strlen_call")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("strlen returned void")?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "input_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, str_len)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_file_exists(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("file_exists expects 1 argument".to_string())); }
                let path_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("file_exists expects a string".to_string())),
                };
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
                    .map_err(|e| CompileError::Generic(format!("access error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("access returned void")?;
                let zero = i32_ty.const_int(0, false);
                let cmp = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    ret.into_int_value(),
                    zero,
                    "exists"
                ).map_err(|e| CompileError::Generic(format!("cmp error: {}", e)))?;
                let ext: BasicValueEnum = self.builder.build_int_z_extend(cmp, self.context.i64_type(), "result")
                    .map_err(|e| CompileError::Generic(format!("zext error: {}", e)))?.into();
                Ok(ext)

    }

    pub(super) fn compile_read_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 { return Err(CompileError::WrongArgCount("read_file expects 1 argument".to_string())); }
                let path_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("read_file expects a string path".to_string())),
                };
                let i8_ptr_ty = self.context.i8_type().ptr_type(inkwell::AddressSpace::default());
                // fopen(path, "r")
                let mode_str = self.builder.build_global_string_ptr("r", "read_mode")
                    .map_err(|e| CompileError::Generic(format!("global string error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("fopen error: {}", e)))?
                    .try_as_basic_value().left()
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
                self.builder.build_call(fseek_fn, &[
                    BasicMetadataValueEnum::PointerValue(file),
                    BasicMetadataValueEnum::IntValue(self.context.i64_type().const_int(0, false)),
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(2, false)), // SEEK_END
                ], "fseek_call")
                    .map_err(|e| CompileError::Generic(format!("fseek error: {}", e)))?;
                // ftell(file) -> file size
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
                    .map_err(|e| CompileError::Generic(format!("ftell error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("ftell returned void")?
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
                    .map_err(|e| CompileError::Generic(format!("rewind error: {}", e)))?;
                // malloc(file_size + 1)
                let one = self.context.i64_type().const_int(1, false);
                let alloc_size = self.builder.build_int_add(file_size, one, "alloc_size")
                    .map_err(|e| CompileError::Generic(format!("add error: {}", e)))?;
                let malloc_fn = self.module.get_function("malloc")
                    .ok_or_else(|| "malloc not declared".to_string())?;
                let buf = self.builder.build_call(malloc_fn, &[
                    BasicMetadataValueEnum::IntValue(alloc_size),
                ], "read_malloc")
                    .map_err(|e| CompileError::Generic(format!("malloc error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("malloc returned void")?
                    .into_pointer_value();
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
                    .map_err(|e| CompileError::Generic(format!("fread error: {}", e)))?;
                // Null-terminate
                // Safety: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                let null_gep = unsafe {
                    self.builder.build_gep(
                        BasicTypeEnum::IntType(self.context.i8_type()),
                        buf,
                        &[file_size],
                        "null_byte"
                    )
                }.map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(null_gep, self.context.i8_type().const_int(0, false))
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("fclose error: {}", e)))?;
                // Build string struct { i8*, i64 }
                let string_ty = self.context.struct_type(&[
                    BasicTypeEnum::PointerType(i8_ptr_ty),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let str_alloca = self.builder.build_alloca(string_ty, "read_str")
                    .map_err(|e| CompileError::Generic(format!("alloca error: {}", e)))?;
                let ptr_gep = self.builder.build_struct_gep(string_ty, str_alloca, 0, "str_ptr")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(ptr_gep, buf)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                let len_gep = self.builder.build_struct_gep(string_ty, str_alloca, 1, "str_len")
                    .map_err(|e| CompileError::Generic(format!("gep error: {}", e)))?;
                self.builder.build_store(len_gep, file_size)
                    .map_err(|e| CompileError::Generic(format!("store error: {}", e)))?;
                Ok(str_alloca.into())

    }

    pub(super) fn compile_write_file(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err(CompileError::WrongArgCount("write_file expects 2 arguments".to_string())); }
                let path_ptr = match args[0] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("write_file: first arg must be string path".to_string())),
                };
                let content_ptr = match args[1] {
                    BasicMetadataValueEnum::PointerValue(pv) => pv,
                    _ => return Err(CompileError::TypeMismatch("write_file: second arg must be string content".to_string())),
                };
                // fopen(path, "w")
                let mode_str = self.builder.build_global_string_ptr("w", "write_mode")
                    .map_err(|e| CompileError::Generic(format!("global string error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("fopen error: {}", e)))?
                    .try_as_basic_value().left()
                    .ok_or("fopen returned void")?
                    .into_pointer_value();
                // strlen(content) for length
                let strlen_fn = self.module.get_function("strlen")
                    .ok_or_else(|| "strlen not declared".to_string())?;
                let content_len = self.builder.build_call(strlen_fn, &[
                    BasicMetadataValueEnum::PointerValue(content_ptr),
                ], "strlen_call")
                    .map_err(|e| CompileError::Generic(format!("strlen error: {}", e)))?
                    .try_as_basic_value().left()
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
                    .map_err(|e| CompileError::Generic(format!("fwrite error: {}", e)))?;
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
                    .map_err(|e| CompileError::Generic(format!("fclose error: {}", e)))?;
                Ok(self.context.i64_type().const_int(0, false).into())

    }

}
