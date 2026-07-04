use crate::ast::*;
use crate::codegen::{CallSiteValueExt, CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{
    AggregateValueEnum, BasicMetadataValueEnum, BasicValueEnum, FloatValue, IntValue, PointerValue,
};
use std::collections::HashMap;

impl<'ctx> CodeGenerator<'ctx> {
    /// Wrap a raw C string pointer into a Mimi string struct `{ ptr, i64 }`.
    /// Calls `strlen` to compute the length, then builds the struct.
    pub(in crate::codegen) fn wrap_c_string(
        &self,
        raw_ptr: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let string_struct_ty = self.context.struct_type(
            &[
                BasicTypeEnum::PointerType(i8_ptr_ty),
                BasicTypeEnum::IntType(self.context.i64_type()),
            ],
            false,
        );

        // Call strlen to get the length
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let length = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                "strlen_call",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".into()))?
            .into_int_value();

        // Build the struct { data_ptr, len }
        let str_val = self
            .builder
            .build_insert_value(string_struct_ty.get_undef(), raw_ptr, 0, "str_data")
            .map_err(|e| CompileError::LlvmError(format!("insert str ptr: {}", e)))?;
        let str_val = self
            .builder
            .build_insert_value(str_val, length, 1, "str_len")
            .map_err(|e| CompileError::LlvmError(format!("insert str len: {}", e)))?;

        Ok(str_val.into_struct_value().into())
    }

    /// Extract a string data pointer from a Mimi string value.
    pub(in crate::codegen) fn extract_string_ptr(
        &self,
        val: &BasicValueEnum<'ctx>,
    ) -> Option<PointerValue<'ctx>> {
        match val {
            BasicValueEnum::PointerValue(pv) => Some(*pv),
            BasicValueEnum::StructValue(sv) => {
                if let Ok(BasicValueEnum::PointerValue(pv)) =
                    self.build_extract_value(AggregateValueEnum::StructValue(*sv), 0, "str_data")
                {
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
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let v = self.compile_expr(inner, vars)?;
        match op {
            UnOp::Neg => {
                if let BasicValueEnum::IntValue(iv) = v {
                    let zero = self.context.i64_type().const_int(0, true);
                    Ok(self
                        .builder
                        .build_int_sub(zero, iv, "neg")
                        .map_err(|e| CompileError::LlvmError(format!("neg error: {}", e)))?
                        .into())
                } else if let BasicValueEnum::FloatValue(fv) = v {
                    let zero = self.context.f64_type().const_float(0.0);
                    Ok(self
                        .builder
                        .build_float_sub(zero, fv, "fneg")
                        .map_err(|e| CompileError::LlvmError(format!("neg error: {}", e)))?
                        .into())
                } else {
                    let ty_desc = type_description(&v.get_type());
                    Err(format!("negation requires numeric type, got {}", ty_desc).into())
                }
            }
            UnOp::Not => {
                if let BasicValueEnum::IntValue(iv) = v {
                    if iv.get_type().get_bit_width() == 1 {
                        Ok(self
                            .builder
                            .build_not(iv, "not")
                            .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?
                            .into())
                    } else {
                        // Some builtins (e.g. contains) return i64 for bool.
                        // Normalize to i1 with `x == 0` so it can feed `if`.
                        let zero = iv.get_type().const_int(0, false);
                        Ok(self
                            .builder
                            .build_int_compare(inkwell::IntPredicate::EQ, iv, zero, "not")
                            .map_err(|e| CompileError::LlvmError(format!("not error: {}", e)))?
                            .into())
                    }
                } else {
                    let ty_desc = type_description(&v.get_type());
                    Err(format!("'not' requires bool, got {}", ty_desc).into())
                }
            }
            UnOp::Ref | UnOp::RefMut => {
                // Borrowed index: for scalar lists, return a pointer directly into
                // the list's data slot rather than copying the element value.
                if let Expr::Index(obj, idx_expr) = inner {
                    let obj_type = self.infer_object_type(obj, vars);
                    let is_scalar_list = obj_type
                        .strip_prefix("List<")
                        .and_then(|rest| rest.strip_suffix('>'))
                        .map(|elem| matches!(elem, "i32" | "i64" | "bool"))
                        .unwrap_or(false);
                    if is_scalar_list {
                        return self
                            .compile_index_addr(obj, idx_expr, vars)
                            .map(|p| p.into());
                    }
                }
                let ty = v.get_type();
                let alloca = self.build_alloca(ty, "ref")?;
                self.build_store(alloca, v)?;
                Ok(alloca.into())
            }
            UnOp::Deref => {
                if let BasicValueEnum::PointerValue(ptr) = v {
                    // Try to determine the pointee type from the inner expression's variable entry
                    let pointee_ty = match inner {
                        Expr::Ident(name) => {
                            if let Some(&(_, ty)) = vars.get(name) {
                                ty
                            } else {
                                BasicTypeEnum::IntType(self.context.i64_type())
                            }
                        }
                        _ => BasicTypeEnum::IntType(self.context.i64_type()),
                    };
                    Ok(self.build_load(pointee_ty, ptr, "deref")?)
                } else {
                    let ty_desc = type_description(&v.get_type());
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
        let (lhs, rhs) = self.promote_binop_operands(lhs, rhs)?;
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                self.compile_arithmetic_binop(op, lhs, rhs)
            }
            BinOp::Mod => self.compile_mod_binop(lhs, rhs),
            BinOp::EqCmp | BinOp::NeCmp => self.compile_equality_binop(op, lhs, rhs),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                self.compile_comparison_binop(op, lhs, rhs)
            }
            BinOp::And | BinOp::Or => self.compile_logical_binop(op, lhs, rhs),
            BinOp::Range => self.compile_range_binop(lhs, rhs),
            BinOp::Pow => self.compile_pow_binop(lhs, rhs),
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                self.compile_bitwise_binop(op, lhs, rhs)
            }
            _ => Err(format!("unsupported binary operator {:?}", op).into()),
        }
    }

    /// Promote integer operands to a common width and integer operands to float
    /// when mixed with a float operand.
    fn promote_binop_operands(
        &self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<(BasicValueEnum<'ctx>, BasicValueEnum<'ctx>), CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let lw = l.get_type().get_bit_width();
                let rw = r.get_type().get_bit_width();
                if lw == rw {
                    Ok((lhs, rhs))
                } else if lw < rw {
                    // i1 (bool) values must be zero-extended so `true` stays 1,
                    // not sign-extended which would produce -1 (all 1s).
                    let ext = if lw == 1 {
                        self.builder.build_int_z_extend(l, r.get_type(), "promote")
                    } else {
                        self.builder.build_int_s_extend(l, r.get_type(), "promote")
                    }
                    .map_err(|e| CompileError::LlvmError(format!("int promote error: {}", e)))?;
                    Ok((ext.into(), rhs))
                } else {
                    let ext = if rw == 1 {
                        self.builder.build_int_z_extend(r, l.get_type(), "promote")
                    } else {
                        self.builder.build_int_s_extend(r, l.get_type(), "promote")
                    }
                    .map_err(|e| CompileError::LlvmError(format!("int promote error: {}", e)))?;
                    Ok((lhs, ext.into()))
                }
            }
            // Mixed integer/float operands: promote the integer side to float.
            (BasicValueEnum::IntValue(i), BasicValueEnum::FloatValue(f)) => {
                let promoted = self
                    .builder
                    .build_signed_int_to_float(i, f.get_type(), "promote_float")
                    .map_err(|e| CompileError::LlvmError(format!("float promote error: {}", e)))?;
                Ok((promoted.into(), f.into()))
            }
            (BasicValueEnum::FloatValue(f), BasicValueEnum::IntValue(i)) => {
                let promoted = self
                    .builder
                    .build_signed_int_to_float(i, f.get_type(), "promote_float")
                    .map_err(|e| CompileError::LlvmError(format!("float promote error: {}", e)))?;
                Ok((f.into(), promoted.into()))
            }
            _ => Ok((lhs, rhs)),
        }
    }

    /// Dispatch arithmetic operators (`+`, `-`, `*`, `/`) to the appropriate
    /// integer, float, or string implementation.
    fn compile_arithmetic_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                self.compile_int_binop(op, l, r)
            }
            (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                self.compile_float_binop(op, l, r)
            }
            (BasicValueEnum::PointerValue(l), BasicValueEnum::PointerValue(r))
                if op == BinOp::Add =>
            {
                self.compile_string_binop(l, r)
            }
            _ => {
                if op == BinOp::Add {
                    if let (Some(l), Some(r)) =
                        (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs))
                    {
                        return self.compile_string_binop(l, r);
                    }
                }
                let msg = match op {
                    BinOp::Add => "add requires same numeric types",
                    BinOp::Sub => "sub requires same numeric types",
                    BinOp::Mul => "mul requires same numeric types",
                    BinOp::Div => "div requires same numeric types",
                    _ => "arithmetic requires same numeric types",
                };
                Err(msg.into())
            }
        }
    }

    /// Integer arithmetic (`+`, `-`, `*`, `/`).
    fn compile_int_binop(
        &self,
        op: BinOp,
        l: IntValue<'ctx>,
        r: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let res = match op {
            BinOp::Add => self.builder.build_int_add(l, r, "add"),
            BinOp::Sub => self.builder.build_int_sub(l, r, "sub"),
            BinOp::Mul => self.builder.build_int_mul(l, r, "mul"),
            BinOp::Div => self.builder.build_int_signed_div(l, r, "div"),
            _ => return Err(format!("unsupported integer arithmetic operator {:?}", op).into()),
        };
        Ok(res
            .map_err(|e| CompileError::LlvmError(format!("{} error: {}", op_name(op), e)))?
            .into())
    }

    /// Floating-point arithmetic (`+`, `-`, `*`, `/`).
    fn compile_float_binop(
        &self,
        op: BinOp,
        l: FloatValue<'ctx>,
        r: FloatValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let res = match op {
            BinOp::Add => self.builder.build_float_add(l, r, "fadd"),
            BinOp::Sub => self.builder.build_float_sub(l, r, "fsub"),
            BinOp::Mul => self.builder.build_float_mul(l, r, "fmul"),
            BinOp::Div => self.builder.build_float_div(l, r, "fdiv"),
            _ => return Err(format!("unsupported float arithmetic operator {:?}", op).into()),
        };
        Ok(res
            .map_err(|e| CompileError::LlvmError(format!("{} error: {}", op_name(op), e)))?
            .into())
    }

    /// Integer remainder (`%`).
    fn compile_mod_binop(
        &self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => Ok(self
                .builder
                .build_int_signed_rem(l, r, "rem")
                .map_err(|e| CompileError::LlvmError(format!("rem error: {}", e)))?
                .into()),
            _ => Err("mod requires integer types".into()),
        }
    }

    /// String concatenation (`+`).
    fn compile_string_binop(
        &self,
        l: PointerValue<'ctx>,
        r: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let concat_fn = self.get_runtime_fn("mimi_str_concat")?;
        let raw_result = self
            .build_call(concat_fn, &[l.into(), r.into()], "str_concat")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("mimi_str_concat returned void".to_string()))?;
        let raw_ptr = raw_result.into_pointer_value();
        // Register the heap allocation so it is freed at scope exit when the
        // result is used directly. `let` bindings transfer ownership by popping
        // this entry and registering the variable slot instead.
        self.register_heap_alloc(raw_ptr);
        self.wrap_c_string(raw_ptr)
    }

    /// Equality and inequality (`==`, `!=`).
    fn compile_equality_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs)) {
            (Some(l), Some(r)) => self.compile_string_comparison_binop(op, l, r),
            _ => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                    let pred = match op {
                        BinOp::EqCmp => inkwell::IntPredicate::EQ,
                        BinOp::NeCmp => inkwell::IntPredicate::NE,
                        _ => return Err(format!("unsupported equality operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_int_compare(pred, l, r, "eq")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let pred = match op {
                        BinOp::EqCmp => inkwell::FloatPredicate::OEQ,
                        BinOp::NeCmp => inkwell::FloatPredicate::ONE,
                        _ => return Err(format!("unsupported equality operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_float_compare(pred, l, r, "feq")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                _ => Err("eq requires same types".into()),
            },
        }
    }

    /// Ordered comparison (`<`, `>`, `<=`, `>=`).
    fn compile_comparison_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (self.extract_string_ptr(&lhs), self.extract_string_ptr(&rhs)) {
            (Some(l), Some(r)) => self.compile_string_comparison_binop(op, l, r),
            _ => match (lhs, rhs) {
                (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                    let pred = match op {
                        BinOp::Lt => inkwell::IntPredicate::SLT,
                        BinOp::Gt => inkwell::IntPredicate::SGT,
                        BinOp::Le => inkwell::IntPredicate::SLE,
                        BinOp::Ge => inkwell::IntPredicate::SGE,
                        _ => return Err(format!("unsupported comparison operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_int_compare(pred, l, r, cmp_name(op))
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                    let pred = match op {
                        BinOp::Lt => inkwell::FloatPredicate::OLT,
                        BinOp::Gt => inkwell::FloatPredicate::OGT,
                        BinOp::Le => inkwell::FloatPredicate::OLE,
                        BinOp::Ge => inkwell::FloatPredicate::OGE,
                        _ => return Err(format!("unsupported comparison operator {:?}", op).into()),
                    };
                    Ok(self
                        .builder
                        .build_float_compare(pred, l, r, fcmp_name(op))
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
                        .into())
                }
                _ => Err("lt requires same numeric types".into()),
            },
        }
    }

    /// String comparison using `strcmp`.
    fn compile_string_comparison_binop(
        &self,
        op: BinOp,
        l: PointerValue<'ctx>,
        r: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let strcmp_fn = self.get_runtime_fn("strcmp")?;
        let result = self
            .build_call(strcmp_fn, &[l.into(), r.into()], "strcmp_call")?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strcmp returned void".into()))?
            .into_int_value();
        let zero = self.context.i32_type().const_int(0, false);
        let pred = match op {
            BinOp::EqCmp => inkwell::IntPredicate::EQ,
            BinOp::NeCmp => inkwell::IntPredicate::NE,
            BinOp::Lt => inkwell::IntPredicate::SLT,
            BinOp::Gt => inkwell::IntPredicate::SGT,
            BinOp::Le => inkwell::IntPredicate::SLE,
            BinOp::Ge => inkwell::IntPredicate::SGE,
            _ => return Err(format!("unsupported string comparison operator {:?}", op).into()),
        };
        let name = match op {
            BinOp::EqCmp => "streq",
            BinOp::NeCmp => "strne",
            BinOp::Lt => "strlt",
            BinOp::Gt => "strgt",
            BinOp::Le => "strle",
            BinOp::Ge => "strge",
            _ => "strcmp",
        };
        Ok(self
            .builder
            .build_int_compare(pred, result, zero, name)
            .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?
            .into())
    }

    /// Boolean logical operators (`&&`, `||`).
    fn compile_logical_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let res = match op {
                    BinOp::And => self.builder.build_and(l, r, "and"),
                    BinOp::Or => self.builder.build_or(l, r, "or"),
                    _ => return Err(format!("unsupported logical operator {:?}", op).into()),
                };
                Ok(res
                    .map_err(|e| CompileError::LlvmError(format!("{} error: {}", op_name(op), e)))?
                    .into())
            }
            _ => {
                let msg = match op {
                    BinOp::And => "and requires boolean types",
                    BinOp::Or => "or requires boolean types",
                    _ => "logical operator requires boolean types",
                };
                Err(msg.into())
            }
        }
    }

    /// Range constructor (`..`).
    fn compile_range_binop(
        &self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
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
        let range_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let alloca = self.build_alloca(range_ty, "range")?;
        let start_gep = self
            .gep()
            .build_struct_gep(range_ty, alloca, 0, "range_start")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(start_gep, start_iv)?;
        let end_gep = self
            .gep()
            .build_struct_gep(range_ty, alloca, 1, "range_end")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(end_gep, end_iv)?;
        Ok(alloca.into())
    }

    /// Power operator (`**`).
    fn compile_pow_binop(
        &self,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(base), BasicValueEnum::IntValue(exp)) => {
                let pow_fn_name = "__mimi_pow_i64";
                let i64_ty = self.context.i64_type();
                let fn_ty = i64_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false);
                let pow_fn = self.module.get_function(pow_fn_name).unwrap_or_else(|| {
                    self.module.add_function(
                        pow_fn_name,
                        fn_ty,
                        Some(inkwell::module::Linkage::External),
                    )
                });
                Ok(self
                    .build_call(pow_fn, &[base.into(), exp.into()], "pow_i64_call")?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("pow returned void".into()))?)
            }
            (BasicValueEnum::FloatValue(l), BasicValueEnum::FloatValue(r)) => {
                let pow_fn = self.get_runtime_fn("llvm.pow.f64")?;
                Ok(self
                    .build_call(pow_fn, &[l.into(), r.into()], "pow_f64")?
                    .try_as_basic_value_opt()
                    .ok_or_else(|| CompileError::LlvmError("pow returned void".into()))?)
            }
            _ => Err("pow requires matching numeric types".into()),
        }
    }

    /// Bitwise operators (`&`, `|`, `^`, `<<`, `>>`).
    fn compile_bitwise_binop(
        &self,
        op: BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match (lhs, rhs) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let res = match op {
                    BinOp::BitAnd => self.builder.build_and(l, r, "bitand"),
                    BinOp::BitOr => self.builder.build_or(l, r, "bitor"),
                    BinOp::BitXor => self.builder.build_xor(l, r, "bitxor"),
                    BinOp::Shl => self.builder.build_left_shift(l, r, "shl"),
                    BinOp::Shr => self.builder.build_right_shift(l, r, true, "shr"),
                    _ => return Err(format!("unsupported bitwise operator {:?}", op).into()),
                };
                let name = match op {
                    BinOp::BitAnd => "and",
                    BinOp::BitOr => "or",
                    BinOp::BitXor => "xor",
                    BinOp::Shl => "shl",
                    BinOp::Shr => "shr",
                    _ => "bitwise",
                };
                Ok(res
                    .map_err(|e| CompileError::LlvmError(format!("{} error: {}", name, e)))?
                    .into())
            }
            _ => {
                let msg = match op {
                    BinOp::BitAnd => "bitand requires integer types",
                    BinOp::BitOr => "bitor requires integer types",
                    BinOp::BitXor => "bitxor requires integer types",
                    BinOp::Shl => "shl requires integer types",
                    BinOp::Shr => "shr requires integer types",
                    _ => "bitwise operator requires integer types",
                };
                Err(msg.into())
            }
        }
    }
}

/// Human-readable description of an LLVM basic type.
fn type_description(ty: &BasicTypeEnum<'_>) -> &'static str {
    match ty {
        BasicTypeEnum::IntType(_) => "int",
        BasicTypeEnum::FloatType(_) => "float",
        BasicTypeEnum::PointerType(_) => "pointer",
        BasicTypeEnum::ArrayType(_) => "array",
        BasicTypeEnum::StructType(_) => "struct",
        BasicTypeEnum::VectorType(_) => "vector",
        BasicTypeEnum::ScalableVectorType(_) => "scalable_vector",
    }
}

/// Short operator name used in LLVM instruction names / error messages.
fn op_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
        BinOp::And => "and",
        BinOp::Or => "or",
        _ => "op",
    }
}

/// LLVM instruction name for an integer comparison operator.
fn cmp_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Lt => "lt",
        BinOp::Gt => "gt",
        BinOp::Le => "le",
        BinOp::Ge => "ge",
        _ => "cmp",
    }
}

/// LLVM instruction name for a floating-point comparison operator.
fn fcmp_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Lt => "flt",
        BinOp::Gt => "fgt",
        BinOp::Le => "fle",
        BinOp::Ge => "fge",
        _ => "fcmp",
    }
}
