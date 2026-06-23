use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {

    /// Extract a string data pointer from a Mimi string value.
    fn extract_string_ptr(&self, val: &BasicValueEnum<'ctx>) -> Option<inkwell::values::PointerValue<'ctx>> {
        match val {
            BasicValueEnum::PointerValue(pv) => Some(*pv),
            BasicValueEnum::StructValue(sv) => {
                if let Ok(BasicValueEnum::PointerValue(pv)) = self.builder.build_extract_value(*sv, 0, "str_data") {
                    Some(pv)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub(in crate::codegen) fn compile_binary_expr(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let l = self.compile_expr(lhs, vars)?;
        let r = self.compile_expr(rhs, vars)?;
        self.compile_binop(op, l, r)
    }


    pub(in crate::codegen) fn compile_unary_expr(
        &mut self,
        op: UnOp,
        inner: &Box<Expr>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let v = self.compile_expr(inner, vars)?;
        match op {
            UnOp::Neg => {
                if let BasicValueEnum::IntValue(iv) = v {
                    let zero = self.context.i64_type().const_int(0, true);
                    Ok(self.builder.build_int_sub(zero, iv, "neg")
                        .map_err(|e| CompileError::LlvmError(format!("neg error: {}", e)))?.into())
                } else if let BasicValueEnum::FloatValue(fv) = v {
                    let zero = self.context.f64_type().const_float(0.0);
                    Ok(self.builder.build_float_sub(zero, fv, "fneg")
                        .map_err(|e| CompileError::LlvmError(format!("neg error: {}", e)))?.into())
                } else {
                    let ty_desc = match v.get_type() {
                        inkwell::types::BasicTypeEnum::IntType(_) => "int",
                        inkwell::types::BasicTypeEnum::FloatType(_) => "float",
                        inkwell::types::BasicTypeEnum::PointerType(_) => "pointer",
                        inkwell::types::BasicTypeEnum::ArrayType(_) => "array",
                        inkwell::types::BasicTypeEnum::StructType(_) => "struct",
                        inkwell::types::BasicTypeEnum::VectorType(_) => "vector",
                        inkwell::types::BasicTypeEnum::ScalableVectorType(_) => "scalable_vector",
                    };
                    Err(format!("negation requires numeric type, got {}", ty_desc).into())
                }
            }
            UnOp::Not => {
                if let BasicValueEnum::IntValue(iv) = v {
                    Ok(self.builder.build_not(iv, "not")
                        .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?.into())
                } else {
                    let ty_desc = match v.get_type() {
                        inkwell::types::BasicTypeEnum::IntType(_) => "int",
                        inkwell::types::BasicTypeEnum::FloatType(_) => "float",
                        inkwell::types::BasicTypeEnum::PointerType(_) => "pointer",
                        inkwell::types::BasicTypeEnum::ArrayType(_) => "array",
                        inkwell::types::BasicTypeEnum::StructType(_) => "struct",
                        inkwell::types::BasicTypeEnum::VectorType(_) => "vector",
                        inkwell::types::BasicTypeEnum::ScalableVectorType(_) => "scalable_vector",
                    };
                    Err(format!("'not' requires bool, got {}", ty_desc).into())
                }
            }
            UnOp::Ref | UnOp::RefMut => {
                let ty = v.get_type();
                let alloca = self.builder.build_alloca(ty, "ref")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, v)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(alloca.into())
            }
            UnOp::Deref => {
                if let BasicValueEnum::PointerValue(ptr) = v {
                    // Try to determine the pointee type from the inner expression's variable entry
                    let pointee_ty = match inner.as_ref() {
                        Expr::Ident(name) => {
                            if let Some(&(_, ty)) = vars.get(name) {
                                ty
                            } else {
                                BasicTypeEnum::IntType(self.context.i64_type())
                            }
                        }
                        _ => BasicTypeEnum::IntType(self.context.i64_type()),
                    };
                    Ok(self.builder.build_load(pointee_ty, ptr, "deref")
                        .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?)
                } else {
                    let ty_desc = match v.get_type() {
                        inkwell::types::BasicTypeEnum::IntType(_) => "int",
                        inkwell::types::BasicTypeEnum::FloatType(_) => "float",
                        inkwell::types::BasicTypeEnum::PointerType(_) => "pointer",
                        inkwell::types::BasicTypeEnum::ArrayType(_) => "array",
                        inkwell::types::BasicTypeEnum::StructType(_) => "struct",
                        inkwell::types::BasicTypeEnum::VectorType(_) => "vector",
                        inkwell::types::BasicTypeEnum::ScalableVectorType(_) => "scalable_vector",
                    };
                    Err(format!("dereference requires pointer type, got {}", ty_desc).into())
                }
            }
        }
    }


    pub(in crate::codegen) fn compile_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let (lhs, rhs) = match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let lw = l.get_type().get_bit_width();
                let rw = r.get_type().get_bit_width();
                if lw == rw {
                    (lhs, rhs)
                } else if lw < rw {
                    let ext = self.builder.build_int_s_extend(l, r.get_type(), "promote")
                        .map_err(|e| CompileError::LlvmError(format!("int promote error: {}", e)))?;
                    (ext.into(), rhs)
                } else {
                    let ext = self.builder.build_int_s_extend(r, l.get_type(), "promote")
                        .map_err(|e| CompileError::LlvmError(format!("int promote error: {}", e)))?;
                    (lhs, ext.into())
                }
            }
            // Mixed integer/float operands: promote the integer side to float.
            (BasicValueEnum::IntValue(i), BasicValueEnum::FloatValue(f)) => {
                let promoted = self.builder.build_signed_int_to_float(i, f.get_type(), "promote_float")
                    .map_err(|e| CompileError::LlvmError(format!("float promote error: {}", e)))?;
                (promoted.into(), f.into())
            }
            (BasicValueEnum::FloatValue(f), BasicValueEnum::IntValue(i)) => {
                let promoted = self.builder.build_signed_int_to_float(i, f.get_type(), "promote_float")
                    .map_err(|e| CompileError::LlvmError(format!("float promote error: {}", e)))?;
                (f.into(), promoted.into())
            }
            _ => (lhs, rhs),
        };
        match op {
            BinOp::Add => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_add(l, r, "add").map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_add(l, r, "fadd").map_err(|e| CompileError::LlvmError(format!("add error: {}", e)))?.into()),
                (BasicValueEnum::PointerValue(l), BasicValueEnum::PointerValue(r)) => {
                    // String concatenation: use mimi_str_concat
                    let concat_fn = self.module.get_function("mimi_str_concat")
                        .ok_or_else(|| CompileError::LlvmError("mimi_str_concat not declared".to_string()))?;
                    let result = self.builder.build_call(concat_fn, &[
                        BasicMetadataValueEnum::PointerValue(l),
                        BasicMetadataValueEnum::PointerValue(r),
                    ], "str_concat")
                        .map_err(|e| CompileError::LlvmError(format!("str_concat error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("mimi_str_concat returned void".to_string()))?;
                    Ok(result)
                }
                _ => {
                    // Try extracting string pointers from struct-typed strings (function params)
                    if let (Some(l), Some(r)) = (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs)) {
                        let concat_fn = self.module.get_function("mimi_str_concat")
                            .ok_or_else(|| CompileError::LlvmError("mimi_str_concat not declared".to_string()))?;
                        let result = self.builder.build_call(concat_fn, &[
                            BasicMetadataValueEnum::PointerValue(l),
                            BasicMetadataValueEnum::PointerValue(r),
                        ], "str_concat")
                            .map_err(|e| CompileError::LlvmError(format!("str_concat error: {}", e)))?
                            .try_as_basic_value_opt()
                            .ok_or_else(|| CompileError::LlvmError("mimi_str_concat returned void".to_string()))?;
                        return Ok(result);
                    }
                    Err("add requires same numeric types".into())
                }
            },
            BinOp::Sub => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_sub(l, r, "sub").map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_sub(l, r, "fsub").map_err(|e| CompileError::LlvmError(format!("sub error: {}", e)))?.into()),
                _ => Err("sub requires same numeric types".into()),
            },
            BinOp::Mul => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_mul(l, r, "mul").map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_mul(l, r, "fmul").map_err(|e| CompileError::LlvmError(format!("mul error: {}", e)))?.into()),
                _ => Err("mul requires same numeric types".into()),
            },
            BinOp::Div => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_div(l, r, "div").map_err(|e| CompileError::LlvmError(format!("div error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_div(l, r, "fdiv").map_err(|e| CompileError::LlvmError(format!("div error: {}", e)))?.into()),
                _ => Err("div requires same numeric types".into()),
            },
            BinOp::Mod => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_signed_rem(l, r, "rem").map_err(|e| CompileError::LlvmError(format!("rem error: {}", e)))?.into()),
                _ => Err("mod requires integer types".into()),
            },
            BinOp::EqCmp => match (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs)) {
                (Some(l), Some(r)) => {
                    let strcmp_fn = self.module.get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let result = self.builder.build_call(strcmp_fn, &[
                        BasicMetadataValueEnum::PointerValue(l),
                        BasicMetadataValueEnum::PointerValue(r),
                    ], "strcmp_call")
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| "strcmp returned void".to_string())?;
                    let cmp = result.into_int_value();
                    let zero = self.context.i32_type().const_int(0, false);
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::EQ, cmp, zero, "streq")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into())
                }
                _ => match (lhs, rhs) {
                    (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                        Ok(self.builder.build_int_compare(inkwell::IntPredicate::EQ, l, r, "eq").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                    (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                        Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, l, r, "feq").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                    _ => Err("eq requires same types".into()),
                },
            },
            BinOp::NeCmp => match (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs)) {
                (Some(l), Some(r)) => {
                    let strcmp_fn = self.module.get_function("strcmp")
                        .ok_or_else(|| "strcmp not declared".to_string())?;
                    let result = self.builder.build_call(strcmp_fn, &[
                        BasicMetadataValueEnum::PointerValue(l),
                        BasicMetadataValueEnum::PointerValue(r),
                    ], "strcmp_call")
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| "strcmp returned void".to_string())?;
                    let cmp = result.into_int_value();
                    let zero = self.context.i32_type().const_int(0, false);
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::NE, cmp, zero, "strne")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into())
                }
                _ => match (lhs, rhs) {
                    (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                        Ok(self.builder.build_int_compare(inkwell::IntPredicate::NE, l, r, "ne").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                    (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                        Ok(self.builder.build_float_compare(inkwell::FloatPredicate::ONE, l, r, "fne").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                    _ => Err("ne requires same types".into()),
                },
            },
            BinOp::Lt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLT, l, r, "lt").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OLT, l, r, "flt").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                _ => Err("lt requires same numeric types".into()),
            },
            BinOp::Gt => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGT, l, r, "gt").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OGT, l, r, "fgt").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                _ => Err("gt requires same numeric types".into()),
            },
            BinOp::Le => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SLE, l, r, "le").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OLE, l, r, "fle").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                _ => Err("le requires same numeric types".into()),
            },
            BinOp::Ge => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_int_compare(inkwell::IntPredicate::SGE, l, r, "ge").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) =>
                    Ok(self.builder.build_float_compare(inkwell::FloatPredicate::OGE, l, r, "fge").map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?.into()),
                _ => Err("ge requires same numeric types".into()),
            },
            BinOp::And => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_and(l, r, "and").map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?.into()),
                _ => Err("and requires boolean types".into()),
            },
            BinOp::Or => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_or(l, r, "or").map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?.into()),
                _ => Err("or requires boolean types".into()),
            },
            BinOp::Range => {
                let start_iv = match lhs {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("range start must be i64".into()),
                };
                let end_iv = match rhs {
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("range end must be i64".into()),
                };
                // Create a range struct { start: i64, end: i64 }
                let i64_ty = self.context.i64_type();
                let range_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(i64_ty),
                    BasicTypeEnum::IntType(i64_ty),
                ], false);
                let alloca = self.builder.build_alloca(range_ty, "range")
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                let start_gep = self.gep().build_struct_gep(range_ty, alloca, 0, "range_start")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(start_gep, start_iv)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                let end_gep = self.gep().build_struct_gep(range_ty, alloca, 1, "range_end")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                self.builder.build_store(end_gep, end_iv)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                Ok(alloca.into())
            }
            BinOp::Pow => match (lhs, rhs) {
                (BasicValueEnum::IntValue(base), BasicValueEnum::IntValue(exp)) => {
                    let pow_fn_name = "__mimi_pow_i64";
                    let i64_ty = self.context.i64_type();
                    let fn_ty = i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false);
                    let pow_fn = self.module.get_function(pow_fn_name)
                        .unwrap_or_else(|| {
                            self.module.add_function(pow_fn_name, fn_ty, Some(inkwell::module::Linkage::External))
                        });
                    Ok(self.builder.build_call(pow_fn, &[
                        BasicMetadataValueEnum::IntValue(base),
                        BasicMetadataValueEnum::IntValue(exp),
                    ], "pow_i64_call")
                        .map_err(|e| CompileError::LlvmError(format!("pow error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("pow returned void")?
                        .into())
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let pow_fn = self.module.get_function("llvm.pow.f64")
                        .ok_or_else(|| "llvm.pow.f64 not declared".to_string())?;
                    Ok(self.builder.build_call(pow_fn, &[
                        BasicMetadataValueEnum::FloatValue(l),
                        BasicMetadataValueEnum::FloatValue(r),
                    ], "pow_f64")
                        .map_err(|e| CompileError::LlvmError(format!("pow error: {}", e)))?
                        .try_as_basic_value_opt()
                        .ok_or("pow returned void")?
                        .into())
                }
                _ => Err("pow requires matching numeric types".into()),
            },
            BinOp::BitAnd => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_and(l, r, "bitand").map_err(|e| CompileError::LlvmError(format!("and error: {}", e)))?.into()),
                _ => Err("bitand requires integer types".into()),
            },
            BinOp::BitOr => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_or(l, r, "bitor").map_err(|e| CompileError::LlvmError(format!("or error: {}", e)))?.into()),
                _ => Err("bitor requires integer types".into()),
            },
            BinOp::BitXor => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_xor(l, r, "bitxor").map_err(|e| CompileError::LlvmError(format!("xor error: {}", e)))?.into()),
                _ => Err("bitxor requires integer types".into()),
            },
            BinOp::Shl => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_left_shift(l, r, "shl").map_err(|e| CompileError::LlvmError(format!("shl error: {}", e)))?.into()),
                _ => Err("shl requires integer types".into()),
            },
            BinOp::Shr => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) =>
                    Ok(self.builder.build_right_shift(l, r, true, "shr").map_err(|e| CompileError::LlvmError(format!("shr error: {}", e)))?.into()),
                _ => Err("shr requires integer types".into()),
            },
            _ => Err(format!("unsupported binary operator {:?}", op).into()),
        }
    }

}
