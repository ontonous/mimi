use super::CodeGenerator;
use crate::error::{CompileError, MimiResult};
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

impl<'ctx> CodeGenerator<'ctx> {

    pub(super) fn compile_abs(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 {
                    return Err("abs expects 1 argument".into());
                }
                match args[0] {
                    BasicMetadataValueEnum::IntValue(iv) => {
                        // abs(x) = x < 0 ? -x : x
                        let zero = self.context.i64_type().const_int(0, true);
                        let neg = self.builder.build_int_sub(zero, iv, "neg")
                            .map_err(|e| format!("neg error: {}", e))?;
                        let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, iv, self.context.i64_type().const_int(0, false), "is_neg")
                            .map_err(|e| format!("cmp error: {}", e))?;
                        let result = self.builder.build_select(cmp, neg, iv, "abs_val")
                            .map_err(|e| format!("select error: {}", e))?;
                        Ok(result)
                    }
                    BasicMetadataValueEnum::FloatValue(_fv) => {
                        // Use fabs
                        let fabs_fn = self.module.get_function("fabs")
                            .unwrap_or_else(|| {
                                let fabs_ty = self.context.f64_type().fn_type(
                                    &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                                self.module.add_function("fabs", fabs_ty, Some(inkwell::module::Linkage::External))
                            });
                        let call = self.builder.build_call(fabs_fn, args, "fabs_call")
                            .map_err(|e| format!("fabs error: {}", e))?;
                        Ok(self.expect_basic_value(&call, "fabs")?)
                    }
                    _ => Err("abs requires numeric type".into()),
                }

    }

    pub(super) fn compile_sqrt(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 1 {
                    return Err("sqrt expects 1 argument".into());
                }
                let sqrt_fn = self.module.get_function("sqrt")
                    .unwrap_or_else(|| {
                        let sqrt_ty = self.context.f64_type().fn_type(
                            &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                        self.module.add_function("sqrt", sqrt_ty, Some(inkwell::module::Linkage::External))
                    });
                let call = self.builder.build_call(sqrt_fn, args, "sqrt_call")
                    .map_err(|e| format!("sqrt error: {}", e))?;
                self.expect_basic_value(&call, "sqrt")

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
                    _ => return Err(CompileError::TypeMismatch("min/max requires integer types".into())),
                };
                let b = match args[1] {
                    BasicMetadataValueEnum::IntValue(iv) => iv,
                    _ => return Err(CompileError::TypeMismatch("min/max requires integer types".into())),
                };
                let pred = if name == "min" {
                    inkwell::IntPredicate::SLT
                } else {
                    inkwell::IntPredicate::SGT
                };
                let cmp = self.builder.build_int_compare(pred, a, b, "cmp")
                    .map_err(|e| format!("cmp error: {}", e))?;
                let result = self.builder.build_select(cmp, a, b, "minmax")
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
                let c_fn = self.module.get_function(fn_name)
                    .unwrap_or_else(|| {
                        let ty = self.context.f64_type().fn_type(
                            &[inkwell::types::BasicMetadataTypeEnum::FloatType(self.context.f64_type())], false);
                        self.module.add_function(fn_name, ty, Some(inkwell::module::Linkage::External))
                    });
                let call = self.builder.build_call(c_fn, args, &format!("{}_call", fn_name))
                    .map_err(|e| format!("{} error: {}", fn_name, e))?;
                self.expect_basic_value(&call, fn_name)

    }

    pub(super) fn compile_pow(
        &self,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                if args.len() != 2 { return Err("pow expects 2 arguments".into()); }
                let f64_ty = self.context.f64_type();
                let a = match args[0] {
                    BasicMetadataValueEnum::FloatValue(fv) => fv,
                    BasicMetadataValueEnum::IntValue(iv) => {
                        self.builder.build_signed_int_to_float(iv, f64_ty, "a_f64")
                            .map_err(|e| format!("int_to_float error: {}", e))?
                    }
                    _ => return Err(CompileError::TypeMismatch("pow requires numeric arguments".into())),
                };
                let b = match args[1] {
                    BasicMetadataValueEnum::FloatValue(fv) => fv,
                    BasicMetadataValueEnum::IntValue(iv) => {
                        self.builder.build_signed_int_to_float(iv, f64_ty, "b_f64")
                            .map_err(|e| format!("int_to_float error: {}", e))?
                    }
                    _ => return Err(CompileError::TypeMismatch("pow requires numeric arguments".into())),
                };
                let pow_fn = self.module.get_function("pow")
                    .unwrap_or_else(|| {
                        let ty = f64_ty.fn_type(&[
                            BasicMetadataTypeEnum::FloatType(f64_ty),
                            BasicMetadataTypeEnum::FloatType(f64_ty),
                        ], false);
                        self.module.add_function("pow", ty, Some(inkwell::module::Linkage::External))
                    });
                let call = self.builder.build_call(pow_fn, &[
                    BasicMetadataValueEnum::FloatValue(a),
                    BasicMetadataValueEnum::FloatValue(b),
                ], "pow_call")
                    .map_err(|e| format!("pow error: {}", e))?;
                self.expect_basic_value(&call, "pow")

    }

    pub(super) fn compile_random(
        &self,
        _args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // Call libc random() and normalize to f64 in [0, 1)
                let f64_ty = self.context.f64_type();
                let i64_ty = self.context.i64_type();
                let random_fn = self.module.get_function("random")
                    .unwrap_or_else(|| {
                        let ty = i64_ty.fn_type(&[], false);
                        self.module.add_function("random", ty, Some(inkwell::module::Linkage::External))
                    });
                let call = self.builder.build_call(random_fn, &[], "random_call")
                    .map_err(|e| format!("random error: {}", e))?;
                let raw = self.expect_basic_value(&call, "random")?.into_int_value();
                let raw_f = self.builder.build_signed_int_to_float(raw, f64_ty, "rand_f")
                    .map_err(|e| format!("random int_to_float error: {}", e))?;
                // RAND_MAX from glibc = 2^31-1 = 2147483647
                let rand_max = f64_ty.const_float(2147483647.0);
                let result = self.builder.build_float_div(raw_f, rand_max, "rand_norm")
                    .map_err(|e| format!("random div error: {}", e))?;
                Ok(result.into())

    }

    pub(super) fn compile_pi(
        &self,
        _args: &[BasicMetadataValueEnum<'ctx>],
    ) -> MimiResult<BasicValueEnum<'ctx>> {
                // Return constant pi as f64
                Ok(self.context.f64_type().const_float(std::f64::consts::PI).into())

    }

}
