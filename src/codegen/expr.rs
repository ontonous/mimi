use crate::ast::*;
use crate::codegen::call_try_basic_value;
use crate::codegen::CallSiteValueExt;
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue};
use std::collections::HashMap;

use super::CodeGenerator;
use super::VarEntry;

impl<'ctx> CodeGenerator<'ctx> {
    pub(super) fn compile_expr(
        &mut self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        match expr {
            Expr::Literal(lit) => self.compile_literal_expr(lit, vars),
            Expr::Ident(name) => self.compile_ident_expr(name, vars),
            Expr::Binary(op, lhs, rhs) => self.compile_binary_expr(*op, lhs, rhs, vars),
            Expr::Unary(op, inner) => self.compile_unary_expr(*op, inner, vars),
            Expr::Call(callee, args) => self.compile_call_expr(callee, args, vars),
            Expr::Turbofish(name, type_args, args) => self.compile_turbofish_expr(name, type_args, args, vars),
            Expr::Match(scrutinee, arms) => self.compile_match_expr(scrutinee, arms, vars),
            Expr::Record { ty, fields } => self.compile_record_expr(ty, fields, vars),
            Expr::Field(obj, field_name) => self.compile_field_expr(obj, field_name, vars),
            Expr::List(elems) => self.compile_list_expr(elems, vars),
            Expr::Index(obj, idx_expr) => self.compile_index_expr(obj, idx_expr, vars),
            Expr::Spawn(expr) => self.compile_spawn_expr(expr, vars),
            Expr::Await(expr) => self.compile_await_expr(expr, vars),
            Expr::Try(inner) => self.compile_try_expr(inner, vars),
            Expr::TypeOf(inner) => self.compile_typeof_expr(inner, vars),
            Expr::TypeInfo(ty) => self.compile_typeinfo_expr(ty, vars),
            Expr::Old(inner) => self.compile_old_expr(inner, vars),
            Expr::Tuple(elems) => self.compile_tuple_expr(elems, vars),
            Expr::TupleIndex(tuple_expr, index) => self.compile_tuple_index_expr(tuple_expr, *index, vars),
            Expr::If { cond, then_, else_ } => self.compile_if_expr(cond, then_, else_, vars),
            Expr::Range { start, end } => self.compile_range_expr(start, end, vars),
            Expr::SliceExpr { target, start, end } => self.compile_slice_expr(target, start, end, vars),
            Expr::Lambda { params, ret, body } => self.compile_lambda_expr(params, ret, body, vars),
            Expr::Comprehension { expr: comp_expr, var, iter, guard } => self.compile_comprehension_expr(comp_expr, var, iter, guard, vars),
            Expr::Arena(block) => {
                let function = self.current_function()
                    .ok_or_else(|| CompileError::LlvmError("arena outside function".to_string()))?;
                let arena_body_bb = self.context.append_basic_block(function, "arena_expr_body");
                let arena_cont_bb = self.context.append_basic_block(function, "arena_expr_cont");
                if !self.block_has_terminator() {
                    self.builder.build_unconditional_branch(arena_body_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                }
                self.builder.position_at_end(arena_body_bb);
                let saved = self.build_stacksave()?;
                let mut arena_vars = vars.clone();
                let val = self.compile_block_last_val(block, &mut arena_vars)?;
                self.build_stackrestore(saved)?;
                if !self.block_has_terminator() {
                    self.builder.build_unconditional_branch(arena_cont_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                }
                self.builder.position_at_end(arena_cont_bb);
                Ok(val)
            }
            Expr::Block(block) => {
                let mut block_vars = vars.clone();
                self.compile_block_last_val(block, &mut block_vars)
            }
            Expr::Comptime(block) => {
                // v0.28.21 — Fold the comptime block via the interpreter and
                // convert the resulting `Value` into an LLVM constant. The
                // interpreter has already been bootstrapped by
                // `fold_comptime_items` in `compile_file`, so any
                // `comptime func` calls inside the block resolve to their
                // pre-computed results.
                self.fold_comptime_block(block)
            }
            Expr::Quote(block) => {
                // v0.28.21 — three-stage fold for quote blocks:
                //   1. Pure literal/arithmetic: `compile_quote_fold` peels
                //      a constant value without going through QuotedAst.
                //   2. Anything that can be resolved through the
                //      interpreter at codegen time (no runtime-only
                //      captures): `fold_comptime_block` evaluates the
                //      block as a `comptime { ... }` would.
                //   3. Truly runtime-only quote blocks: emit real
                //      `mimi_quote_new_*` runtime calls that construct
                //      a heap-allocated `MimiQuotedAst` tree at runtime.
                //      The result is an `i8*` pointer to the root node.
                if let Some(val) = self.compile_quote_fold(block) {
                    return Ok(val);
                }
                if let Ok(val) = self.fold_quote_block(block) {
                    return Ok(val);
                }
                // Fall through to runtime QuotedAst construction.
                self.compile_quote_runtime(block)
            }
            Expr::QuoteInterpolate(inner) => {
                // v0.28.21 — `$(expr)` interpolations are evaluated at
                // codegen time. The resulting `Value` is then converted
                // to an LLVM constant via the same path as a literal.
                self.fold_quote_interpolate(inner)
            }
            Expr::MapLiteral { entries } => self.compile_map_literal(entries, vars),
            Expr::SetLiteral(elems) => self.compile_set_literal(elems, vars),
            Expr::NamedArg(name, _) => Err(CompileError::Generic(format!(
                "named argument '{}' in codegen: named arguments must be resolved before codegen (use positional args or evaluate at comptime)", name
            ))),
            Expr::Cast(inner, target_type) => self.compile_cast_expr(inner, target_type, vars),
            Expr::OptionalChain(inner, field) => self.compile_optional_chain(inner, field, vars),
            #[allow(unreachable_patterns)]
            _ => Err(format!("unsupported expression in codegen: {:?}", expr).into())
        }
    }

    fn compile_cast_expr(
        &mut self,
        inner: &Expr,
        target_type: &Type,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let val = self.compile_expr(inner, vars)?;
        let target_name = match target_type {
            Type::Name(name, _) => name.as_str(),
            _ => return Err("unsupported cast target type in codegen".into()),
        };
        match target_name {
            "i32" => match val {
                BasicValueEnum::IntValue(iv) => {
                    let i32_ty = self.context.i32_type();
                    if iv.get_type() == i32_ty {
                        Ok(val)
                    } else if iv.get_type().get_bit_width() > 32 {
                        Ok(self
                            .builder
                            .build_int_truncate(iv, i32_ty, "cast_i32")
                            .map_err(|e| CompileError::LlvmError(format!("truncate error: {}", e)))?
                            .into())
                    } else {
                        // A1: use s_extend for signed integers (width > 1),
                        // z_extend for bool (i1 — sign bit would make true = -1).
                        if iv.get_type().get_bit_width() == 1 {
                            Ok(self
                                .builder
                                .build_int_z_extend(iv, i32_ty, "cast_i32")
                                .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                                .into())
                        } else {
                            Ok(self
                                .builder
                                .build_int_s_extend(iv, i32_ty, "cast_i32")
                                .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?
                                .into())
                        }
                    }
                }
                BasicValueEnum::FloatValue(fv) => Ok(self
                    .builder
                    .build_float_to_signed_int(fv, self.context.i32_type(), "fptosi")
                    .map_err(|e| CompileError::LlvmError(format!("fptosi error: {}", e)))?
                    .into()),
                _ => Err("unsupported cast to i32".into()),
            },
            "i64" => match val {
                BasicValueEnum::IntValue(iv) => {
                    let i64_ty = self.context.i64_type();
                    if iv.get_type() == i64_ty {
                        Ok(val)
                    } else {
                        // A1: use s_extend for signed integers (width > 1),
                        // z_extend for bool (i1 — sign bit would make true = -1).
                        if iv.get_type().get_bit_width() == 1 {
                            Ok(self
                                .builder
                                .build_int_z_extend(iv, i64_ty, "cast_i64")
                                .map_err(|e| CompileError::LlvmError(format!("zext error: {}", e)))?
                                .into())
                        } else {
                            Ok(self
                                .builder
                                .build_int_s_extend(iv, i64_ty, "cast_i64")
                                .map_err(|e| CompileError::LlvmError(format!("sext error: {}", e)))?
                                .into())
                        }
                    }
                }
                BasicValueEnum::FloatValue(fv) => Ok(self
                    .builder
                    .build_float_to_signed_int(fv, self.context.i64_type(), "fptosi")
                    .map_err(|e| CompileError::LlvmError(format!("fptosi error: {}", e)))?
                    .into()),
                _ => Err("unsupported cast to i64".into()),
            },
            "f64" => match val {
                BasicValueEnum::IntValue(iv) => Ok(self
                    .builder
                    .build_signed_int_to_float(iv, self.context.f64_type(), "sitofp")
                    .map_err(|e| CompileError::LlvmError(format!("sitofp error: {}", e)))?
                    .into()),
                BasicValueEnum::FloatValue(fv) => {
                    let f64_ty = self.context.f64_type();
                    if fv.get_type() == f64_ty {
                        Ok(val)
                    } else {
                        Ok(self
                            .builder
                            .build_float_ext(fv, f64_ty, "fpext")
                            .map_err(|e| CompileError::LlvmError(format!("fpext error: {}", e)))?
                            .into())
                    }
                }
                _ => Err("unsupported cast to f64".into()),
            },
            "List" => {
                // Type annotation for lists — no runtime conversion needed
                Ok(val)
            }
            _ => Err(format!("unsupported cast target type: {}", target_name).into()),
        }
    }

    /// `x?.field` — if `x` is Option::Some (or Result::Ok), load field from
    /// payload and wrap as Some; if None/Err, return None.
    /// Type of `x?.field` is always `Option<field_ty>` (see infer_expr).
    /// Layout: Option/Result disc is i1 (Some/Ok = 1). Ok/Some payload is field 1.
    fn compile_optional_chain(
        &mut self,
        inner: &Expr,
        field: &str,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let result_val = self.compile_expr(inner, vars)?;
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for optional chain".to_string())?;
        let some_bb = self.context.append_basic_block(function, "optchain_some");
        let none_bb = self.context.append_basic_block(function, "optchain_none");
        let merge_bb = self.context.append_basic_block(function, "optchain_merge");

        let obj_type = self.infer_object_type(inner, vars);
        let is_result = obj_type.starts_with("Result<") || obj_type == "Result";
        let base_type = Self::strip_option_or_result_ok(&obj_type);

        // Built-in Option {i1, i64} / Result {i1, i64, i64} for load-from-ptr.
        // Actual payload may be a struct; extract_value still works on the
        // concrete struct value when we already hold a StructValue.
        let option_load_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );
        let result_load_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(bool_ty),
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::IntType(i64_ty),
            ],
            false,
        );

        let sv = match result_val {
            BasicValueEnum::StructValue(sv) => sv,
            BasicValueEnum::PointerValue(pv) => {
                let load_ty = if is_result {
                    BasicTypeEnum::StructType(result_load_ty)
                } else {
                    BasicTypeEnum::StructType(option_load_ty)
                };
                self.builder
                    .build_load(load_ty, pv, "optchain_load")
                    .map_err(|e| CompileError::LlvmError(format!("optchain load: {}", e)))?
                    .into_struct_value()
            }
            _ => {
                return Err(CompileError::Generic(format!(
                    "optional chain `?.{}` requires Option or Result value",
                    field
                )));
            }
        };

        let disc = self
            .builder
            .build_extract_value(sv, 0, "optchain_disc")
            .map_err(|e| CompileError::LlvmError(format!("extract disc: {}", e)))?
            .into_int_value();
        // Some/Ok = disc != 0 (i1 true or non-zero).
        let is_none = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                disc,
                bool_ty.const_int(0, false),
                "optchain_is_none",
            )
            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
        self.builder
            .build_conditional_branch(is_none, none_bb, some_bb)
            .map_err(|e| CompileError::LlvmError(format!("br: {}", e)))?;

        // Always emit canonical Option {i1, i64} so phi types match regardless
        // of field payload shape (int/float/ptr/struct → i64 slot).
        let canon = BasicTypeEnum::StructType(option_load_ty);

        // ── Some/Ok path: extract payload, load field, wrap as Some ──
        self.builder.position_at_end(some_bb);
        let payload = self
            .builder
            .build_extract_value(sv, 1, "optchain_payload")
            .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;

        let field_val = self.load_optional_chain_field(payload, &base_type, field)?;
        let some_slot = self.build_alloca(canon, "opt_some_slot")?;
        // disc = 1
        self.build_store(
            self.gep()
                .build_struct_gep(canon, some_slot, 0, "sd")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            bool_ty.const_int(1, false),
        )?;
        let pay_i64 = self.option_payload_to_i64(field_val)?;
        self.build_store(
            self.gep()
                .build_struct_gep(canon, some_slot, 1, "sp")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            pay_i64,
        )?;
        let some_val = self.build_load(canon, some_slot, "some")?;
        self.build_br(merge_bb)?;
        let some_bb_end = self.builder.get_insert_block().unwrap_or(some_bb);

        // ── None/Err path: return None ──
        self.builder.position_at_end(none_bb);
        let none_slot = self.build_alloca(canon, "opt_none_slot")?;
        self.build_store(
            self.gep()
                .build_struct_gep(canon, none_slot, 0, "nd")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            bool_ty.const_int(0, false),
        )?;
        self.build_store(
            self.gep()
                .build_struct_gep(canon, none_slot, 1, "np")
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?,
            i64_ty.const_int(0, false),
        )?;
        let none_val = self.build_load(canon, none_slot, "none")?;
        self.build_br(merge_bb)?;
        let none_bb_end = self.builder.get_insert_block().unwrap_or(none_bb);

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(canon, "optchain_phi")
            .map_err(|e| CompileError::LlvmError(format!("phi: {}", e)))?;
        phi.add_incoming(&[(&some_val, some_bb_end), (&none_val, none_bb_end)]);
        Ok(phi.as_basic_value())
    }

    /// Pack a field value into the i64 payload slot of a canonical Option.
    fn option_payload_to_i64(
        &mut self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        match val {
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw < 64 {
                    Ok(self
                        .builder
                        .build_int_s_extend(iv, i64_ty, "opt_pay_sext")
                        .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))?
                        .into())
                } else if bw > 64 {
                    Ok(self
                        .builder
                        .build_int_truncate(iv, i64_ty, "opt_pay_trunc")
                        .map_err(|e| CompileError::LlvmError(format!("trunc: {}", e)))?
                        .into())
                } else {
                    Ok(iv.into())
                }
            }
            BasicValueEnum::PointerValue(pv) => Ok(self
                .builder
                .build_ptr_to_int(pv, i64_ty, "opt_pay_ptr")
                .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?
                .into()),
            BasicValueEnum::FloatValue(fv) => {
                // f64 bit-pattern into i64; f32 → f64 first.
                let f64_ty = self.context.f64_type();
                let as_f64 = if fv.get_type().get_bit_width() == 64 {
                    fv
                } else {
                    self.builder
                        .build_float_ext(fv, f64_ty, "opt_f32_to_f64")
                        .map_err(|e| CompileError::LlvmError(format!("fpext: {}", e)))?
                };
                Ok(self
                    .builder
                    .build_bit_cast(as_f64, i64_ty, "opt_f64_bits")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?)
            }
            BasicValueEnum::StructValue(sv) => {
                // Spill struct (e.g. string {ptr,len}) and store pointer as i64.
                let sty = sv.get_type();
                let tmp = self.build_alloca(BasicTypeEnum::StructType(sty), "opt_pay_struct")?;
                self.build_store(tmp, sv)?;
                Ok(self
                    .builder
                    .build_ptr_to_int(tmp, i64_ty, "opt_struct_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?
                    .into())
            }
            other => {
                let tmp = self.build_alloca(other.get_type(), "opt_pay_tmp")?;
                self.build_store(tmp, other)?;
                Ok(self
                    .builder
                    .build_ptr_to_int(tmp, i64_ty, "opt_other_ptr")
                    .map_err(|e| CompileError::LlvmError(format!("ptrtoint: {}", e)))?
                    .into())
            }
        }
    }

    /// Strip `Option<T>` / `Result<T,E>` wrappers → `T` (for field lookup).
    fn strip_option_or_result_ok(obj_type: &str) -> String {
        if let Some(rest) = obj_type.strip_prefix("Option<") {
            // Drop matching outer `>`; handle nested generics.
            let mut depth = 0i32;
            let mut end = rest.len();
            for (i, ch) in rest.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' => {
                        if depth == 0 {
                            end = i;
                            break;
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            return rest[..end].trim().to_string();
        }
        if let Some(rest) = obj_type.strip_prefix("Result<") {
            let mut depth = 0i32;
            let mut end = rest.len();
            for (i, ch) in rest.char_indices() {
                match ch {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    ',' if depth == 0 => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            return rest[..end].trim().to_string();
        }
        obj_type.to_string()
    }

    /// Load `field` from an Option/Result payload value of type `base_type`.
    fn load_optional_chain_field(
        &mut self,
        payload: BasicValueEnum<'ctx>,
        base_type: &str,
        field: &str,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        let base = if let Some(lt) = base_type.find('<') {
            &base_type[..lt]
        } else {
            base_type
        };

        // Numeric tuple index: payload is a tuple struct value.
        if let Ok(idx) = field.parse::<u32>() {
            let field_ptr = match payload {
                BasicValueEnum::PointerValue(pv) => pv,
                BasicValueEnum::StructValue(psv) => {
                    let sty = psv.get_type();
                    let tmp = self.build_alloca(BasicTypeEnum::StructType(sty), "opt_tup")?;
                    self.build_store(tmp, psv)?;
                    tmp
                }
                BasicValueEnum::IntValue(iv) => self
                    .builder
                    .build_int_to_ptr(
                        iv,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "opt_tup_ptr",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?,
                _ => {
                    return Err(CompileError::Generic(format!(
                        "optional chain `?.{}`: unsupported tuple payload",
                        field
                    )));
                }
            };
            // Prefer registered struct type; else use payload's own struct type.
            let sty = self
                .expect_struct_type(base)
                .or_else(|_| match payload {
                    BasicValueEnum::StructValue(psv) => Ok(psv.get_type()),
                    BasicValueEnum::PointerValue(_) => self.expect_struct_type(base),
                    _ => Err(CompileError::Generic(format!(
                        "optional chain `?.{}`: cannot resolve tuple type `{}`",
                        field, base_type
                    ))),
                })?;
            let gep = self
                .gep()
                .build_struct_gep(sty, field_ptr, idx, field)
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            let loaded = self.build_load(BasicTypeEnum::IntType(i64_ty), gep, field)?;
            return Ok(loaded);
        }

        let td = self.type_defs.get(base).ok_or_else(|| {
            CompileError::Generic(format!(
                "optional chain `?.{}`: cannot resolve payload type `{}`",
                field, base_type
            ))
        })?;
        let fields = match &td.kind {
            TypeDefKind::Record(fields) => fields,
            _ => {
                return Err(CompileError::Generic(format!(
                    "optional chain `?.{}`: payload type `{}` is not a record",
                    field, base_type
                )));
            }
        };
        let idx = fields.iter().position(|f| f.name == *field).ok_or_else(|| {
            CompileError::Generic(format!(
                "optional chain: no field `{}` on type `{}`",
                field, base_type
            ))
        })?;

        let field_ptr = match payload {
            BasicValueEnum::PointerValue(pv) => pv,
            BasicValueEnum::IntValue(iv) => self
                .builder
                .build_int_to_ptr(
                    iv,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "optchain_payload_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?,
            BasicValueEnum::StructValue(psv) => {
                let sty = psv.get_type();
                let tmp = self.build_alloca(BasicTypeEnum::StructType(sty), "opt_rec")?;
                self.build_store(tmp, psv)?;
                tmp
            }
            _ => {
                return Err(CompileError::Generic(format!(
                    "optional chain `?.{}`: unsupported payload shape",
                    field
                )));
            }
        };

        let sty = self.expect_struct_type(base)?;
        let gep = self
            .gep()
            .build_struct_gep(sty, field_ptr, idx as u32, field)
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        // i32 fields: load as i32 then sext to i64 (same as compile_field_expr).
        let (load_ty, ext) = match &fields[idx].ty {
            Type::Name(n, _) if n == "i32" => (BasicTypeEnum::IntType(self.context.i32_type()), true),
            _ => (
                self.llvm_type_for(&fields[idx].ty)
                    .unwrap_or(BasicTypeEnum::IntType(i64_ty)),
                false,
            ),
        };
        let loaded = self.build_load(load_ty, gep, field)?;
        if ext {
            if let BasicValueEnum::IntValue(iv) = loaded {
                return Ok(self
                    .builder
                    .build_int_s_extend(iv, i64_ty, "opt_i32_sext")
                    .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))?
                    .into());
            }
        }
        Ok(loaded)
    }

    fn compile_ident_expr(
        &mut self,
        name: &String,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if let Some(&(alloca, ty)) = vars.get(name) {
            if self.shared_var_names.contains(name.as_str()) {
                // Shared variable: the alloca stores a T* pointer to heap memory.
                // First load the pointer, then load the value from the heap.
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let heap_ptr = self.builder.build_load(ptr_ty, alloca, name).map_err(|e| {
                    CompileError::LlvmError(format!("shared heap ptr load error: {}", e))
                })?;
                let heap_pointer = heap_ptr.into_pointer_value();
                self.builder
                    .build_load(ty, heap_pointer, name)
                    .map_err(|e| CompileError::LlvmError(format!("shared value load error: {}", e)))
            } else {
                self.builder
                    .build_load(ty, alloca, name)
                    .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))
            }
        } else if self.cap_type_names.contains(name.as_str()) {
            // Cap literal: call mimi_cap_register(name) to get handle
            if let Some(register_fn) = self.module.get_function("mimi_cap_register") {
                let name_global = self
                    .builder
                    .build_global_string_ptr(&format!("{}\0", name), &format!("cap_name_{}", name))
                    .map_err(|e| CompileError::LlvmError(format!("string global error: {}", e)))?;
                let name_ptr = name_global.as_pointer_value();
                let handle = self
                    .builder
                    .build_call(
                        register_fn,
                        &[BasicMetadataValueEnum::PointerValue(name_ptr)],
                        &format!("cap_register_{}", name),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cap_register error: {}", e)))?
                    .try_as_basic_value_opt()
                    .ok_or("mimi_cap_register returned void")?;
                Ok(handle)
            } else {
                Err(format!("cap literal '{}' requires mimi_cap_register runtime", name).into())
            }
        } else if self.find_variant_owner(name).is_some() {
            // Unit enum variant used as a value (e.g. `Yes` or `Pending`)
            self.compile_call(name, &[], vars)
        } else if name == "None" {
            // Bare built-in None constructor (e.g. `let x: Option<i32> = None`)
            self.compile_constructor("None", vec![])
        } else if let Some(function) = self.module.get_function(name) {
            // First-class function reference: return function pointer as value
            Ok(function.as_global_value().as_pointer_value().into())
        } else if let Some(const_expr) = self.const_values.get(name).cloned() {
            // Const value: compile the expression
            self.compile_expr(&const_expr, vars)
        } else {
            Err(format!("undefined variable '{}'", name).into())
        }
    }

    fn compile_old_expr(
        &mut self,
        inner: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        // old(expr): snapshot value at function entry.
        // Merge old snapshots into the vars map so variable references within
        // old() resolve to the entry-time alloca, not the current value.
        if self.old_snapshots.is_empty() {
            self.compile_expr(inner, vars)
        } else {
            let mut old_vars = vars.clone();
            for (name, entry) in &self.old_snapshots {
                old_vars.insert(name.clone(), *entry);
            }
            self.compile_expr(inner, &old_vars)
        }
    }

    #[allow(clippy::only_used_in_recursion)]
    pub(super) fn infer_object_type(
        &self,
        expr: &Expr,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> String {
        match expr {
            Expr::Literal(Lit::String(_)) | Expr::Literal(Lit::FString(_)) => "string".to_string(),
            Expr::Literal(Lit::Int(_)) => "i32".to_string(),
            Expr::Literal(Lit::Float(_)) => "f64".to_string(),
            Expr::Literal(Lit::Bool(_)) => "bool".to_string(),
            Expr::Literal(Lit::Unit) => "unit".to_string(),
            Expr::Ident(name) => {
                // Look up variable's type name from our tracking map
                if let Some(ty_name) = self.var_type_names.get(name) {
                    return ty_name.clone();
                }
                // Fallback: derive a type name from the variable's LLVM slot type.
                // This helps method dispatch on local variables whose type was not
                // explicitly annotated (e.g. `let total_secs = self / MILLIS_PER_SECOND`).
                if let Some(entry) = vars.get(name) {
                    let llvm_ty = entry.1;
                    if let Some(ty_name) = Self::llvm_type_to_object_name(&llvm_ty) {
                        return ty_name;
                    }
                }
                name.clone()
            }
            Expr::Record { ty: Some(name), .. } => name.clone(),
            Expr::Call(callee, args) => {
                if let Expr::Ident(name) = callee.as_ref() {
                    // Try to strip _new suffix used by our codegen constructors
                    if let Some(stripped) = name.strip_suffix("_new") {
                        return stripped.to_string();
                    }
                    if let Some(ret_name) = self.infer_call_return_type_name(name) {
                        return ret_name;
                    }
                    name.clone()
                } else if let Expr::Field(obj, method) = callee.as_ref() {
                    // Method call result: infer the return type of string methods
                    // so that chained calls like s.trim().to_upper() work.
                    let obj_type = self.infer_object_type(obj, vars);
                    if obj_type == "string" {
                        self.infer_string_method_return_type(method)
                    } else if let Expr::Ident(flow_name) = obj.as_ref() {
                        // Flow::transition(from, ...) → to-state of the matching overload.
                        // Prefer from_state match so fallbacks (→ Fault) win over earlier defs.
                        if let Some(flow) = self.flow_defs.get(flow_name) {
                            let from_type = args
                                .first()
                                .map(|a| self.infer_object_type(a, vars))
                                .unwrap_or_default();
                            let t = flow
                                .transitions
                                .iter()
                                .find(|t| t.name == *method && t.from_state == from_type)
                                .or_else(|| flow.transitions.iter().find(|t| t.name == *method));
                            if let Some(t) = t {
                                return t.to_states.first().cloned().unwrap_or_default();
                            }
                        }
                        String::new()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            }
            Expr::Field(obj, field_name) => {
                let obj_type = self.infer_object_type(obj, vars);
                if let Some(td) = self.type_defs.get(&obj_type) {
                    if let crate::ast::TypeDefKind::Record(fields) = &td.kind {
                        if let Some(f) = fields.iter().find(|f| f.name == *field_name) {
                            return crate::core::fmt_type(&f.ty);
                        }
                    }
                }
                obj_type
            }
            // PA-H3: `x?.field` has type Option<field_ty>; track as Option<…>
            // so nested optional chains and match can resolve payload types.
            Expr::OptionalChain(inner, field_name) => {
                let obj_type = self.infer_object_type(inner, vars);
                let base = Self::strip_option_or_result_ok(&obj_type);
                let base_key = if let Some(lt) = base.find('<') {
                    &base[..lt]
                } else {
                    base.as_str()
                };
                let field_ty = if let Some(td) = self.type_defs.get(base_key) {
                    if let crate::ast::TypeDefKind::Record(fields) = &td.kind {
                        fields
                            .iter()
                            .find(|f| f.name == *field_name)
                            .map(|f| crate::core::fmt_type(&f.ty))
                    } else {
                        None
                    }
                } else {
                    None
                };
                match field_ty {
                    Some(ft) => format!("Option<{}>", ft),
                    None => "Option".to_string(),
                }
            }
            Expr::Index(obj, _) => {
                // Index into a List<T> returns T. Infer the list's element type.
                let obj_type = self.infer_object_type(obj, vars);
                if let Some(inner) = obj_type.strip_prefix("List<") {
                    let mut depth = 0u32;
                    for (i, ch) in inner.char_indices() {
                        match ch {
                            '<' => depth += 1,
                            '>' => {
                                if depth == 0 {
                                    return inner[..i].trim().to_string();
                                }
                                depth -= 1;
                            }
                            _ => {}
                        }
                    }
                    inner.trim().to_string()
                } else {
                    String::new()
                }
            }
            Expr::List(elems) => {
                if let Some(first) = elems.first() {
                    let elem = self.infer_object_type(first, vars);
                    if elem.is_empty() {
                        "List".into()
                    } else {
                        format!("List<{}>", elem)
                    }
                } else {
                    "List".into()
                }
            }
            Expr::Block(block) => block
                .last()
                .and_then(|last| {
                    if let Stmt::Expr(e) = last {
                        Some(self.infer_object_type(e, vars))
                    } else {
                        None
                    }
                })
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Strip one level of `List<...>` from a type name, respecting nested
    /// generic brackets. Returns `None` if `s` does not start with `List<`.
    pub(super) fn strip_list_element_type(s: &str) -> Option<String> {
        let inner = s.strip_prefix("List<")?;
        let mut depth = 0u32;
        for (i, ch) in inner.char_indices() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    if depth == 0 {
                        return Some(inner[..i].trim().to_string());
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        Some(inner.trim().to_string())
    }

    /// True iff `s` names a `List<...>` type (used by v0.28.29 to detect when
    /// a builtin call argument needs the original alloca instead of a loaded
    /// StructValue so in-place list mutations are visible to the caller).
    pub(super) fn is_list_type_name(&self, s: &str) -> bool {
        Self::strip_list_element_type(s).is_some()
    }

    /// Map a stored LLVM value type back to a coarse Mimi type name for method
    /// dispatch. This is intentionally approximate: it only needs to distinguish
    /// the builtin scalar types and common struct layouts (string, List, Option,
    /// Result) when the variable was not explicitly annotated.
    fn llvm_type_to_object_name(llvm_ty: &BasicTypeEnum<'ctx>) -> Option<String> {
        match llvm_ty {
            BasicTypeEnum::IntType(t) => {
                let width = t.get_bit_width();
                if width == 1 {
                    Some("bool".to_string())
                } else if width == 32 {
                    Some("i32".to_string())
                } else {
                    Some("i64".to_string())
                }
            }
            BasicTypeEnum::FloatType(_) => Some("f64".to_string()),
            BasicTypeEnum::StructType(sty) => {
                let fields = sty.get_field_types();
                match fields.as_slice() {
                    // Mimi string: {i8*, i64}
                    [BasicTypeEnum::PointerType(_), BasicTypeEnum::IntType(t)]
                        if t.get_bit_width() == 64 =>
                    {
                        Some("string".to_string())
                    }
                    // Mimi List<T>: {i64 len, ptr}
                    [BasicTypeEnum::IntType(t), BasicTypeEnum::PointerType(_)]
                        if t.get_bit_width() == 64 =>
                    {
                        Some("List<unknown>".to_string())
                    }
                    // Option<T>: {i1 disc, T payload}; approximate as Option
                    [BasicTypeEnum::IntType(t), _] if t.get_bit_width() == 1 => {
                        Some("Option".to_string())
                    }
                    // Result<T, E>: {i1 disc, T ok, E err}; approximate as Result
                    // When E = string (Result<T, string>), the err field is {i8*, i64}.
                    // When E = i64 (Result<T, i64>), the err field is just i64.
                    [BasicTypeEnum::IntType(t), _, _] if t.get_bit_width() == 1 => {
                        Some("Result".to_string())
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Map a function name to the Mimi type-name of its return value. This is
    /// used by `infer_object_type` so that method calls on call expressions
    /// (e.g. `str_index_of(...).unwrap_or(-1)`, `getenv(...).is_ok()`) can be
    /// dispatched even when the result is not bound to a variable.
    /// Infer the return type of a string method for use in method chain resolution.
    pub(super) fn infer_string_method_return_type(&self, method: &str) -> String {
        match method {
            "trim" | "to_upper" | "to_lower" | "repeat" | "replace" | "char_at" | "substring" => {
                "string".to_string()
            }
            "contains" | "starts_with" | "ends_with" => "bool".to_string(),
            "len" => "i32".to_string(),
            "split" => "List<string>".to_string(),
            "parse_int" => "Result<i64,string>".to_string(),
            "parse_float" => "Result<f64,string>".to_string(),
            "index_of" => "Option<i32>".to_string(),
            _ => String::new(),
        }
    }

    fn infer_call_return_type_name(&self, name: &str) -> Option<String> {
        // Built-ins whose return type is not obvious from the name alone.
        match name {
            "getenv" | "base64_decode" => return Some("Result<string,string>".to_string()),
            "str_index_of" => return Some("Option<i32>".to_string()),
            "str_replace" | "str_substring" | "str_join" | "str_trim" | "str_to_upper"
            | "str_to_lower" | "str_repeat" | "to_string" | "int_to_string" | "float_to_string"
            | "chr" | "type_name" | "c_str_to_string" | "from_json" => {
                return Some("string".to_string())
            }
            "input" | "read_file" | "write_file" | "write_file_bytes" => {
                return Some("Result<string,string>".to_string())
            }
            "listdir" | "walk_dir" | "str_split" | "keys" | "values" | "sort_str" => {
                return Some("List<string>".to_string())
            }
            "sort_f64" => return Some("List<f64>".to_string()),
            "exec" | "exec_safe" => return Some("ExecResult".to_string()),
            "file_stat" => return Some("StatResult".to_string()),
            _ => {}
        }
        // User-defined functions
        if let Some(fdef) = self.func_defs.get(name) {
            if let Some(ret_ty) = &fdef.ret {
                // Check if this is a newtype constructor — return the newtype name
                // (not the unfolded inner type) so trait method dispatch works.
                if matches!(ret_ty, crate::ast::Type::Newtype(n, _) if n == name) {
                    return Some(name.to_string());
                }
                return Some(crate::core::fmt_type(ret_ty));
            }
        }
        // Extern functions
        if let Some(ef) = self.extern_func_defs.get(name) {
            if let Some(ret_ty) = &ef.ret {
                return Some(crate::core::fmt_type(ret_ty));
            }
        }
        None
    }

    /// Extract a raw C string pointer (i8*) from a Mimi string argument.
    /// Mimi strings are represented as either:
    ///   - An i8* raw C string (from string literals)
    ///   - A {i8*, i64} struct (from string variables)
    pub(super) fn extract_raw_str_ptr(
        &self,
        arg: &BasicMetadataValueEnum<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        match arg {
            BasicMetadataValueEnum::PointerValue(pv) => {
                // Could be a raw C string pointer OR a pointer to a Mimi string struct {i8*, i64}.
                // For now, assume it's a raw C string pointer (string literal case).
                // String variables that hold recv() results produce struct values, not pointers.
                Ok(*pv)
            }
            BasicMetadataValueEnum::StructValue(sv) => {
                let extracted = self.build_extract_value((*sv).into(), 0, "str_ptr")?;
                match extracted {
                    BasicValueEnum::PointerValue(pv) => Ok(pv),
                    _ => Err("string struct field 0 is not a pointer".into()),
                }
            }
            _ => Err("expected a string argument".into()),
        }
    }

    /// Return an error if running in no_std mode for a builtin that depends on libc.
    pub(super) fn require_std(&self, builtin: &str) -> Result<(), CompileError> {
        if self.no_std {
            Err(CompileError::Generic(format!(
                "[E0750] '{}' requires libc (not available in no_std mode)",
                builtin
            )))
        } else {
            Ok(())
        }
    }

    /// Compile-time fold a literal-only quote! block.
    /// quote! { 42 } → returns i64(42), bypassing QuotedAst construction.
    /// v0.28.21: extended to recursively fold literal-only arithmetic
    /// and unary expressions (e.g. `quote! { 10 + 20 }` → 30).
    fn compile_quote_fold(&self, block: &Block) -> Option<BasicValueEnum<'ctx>> {
        match block.as_slice() {
            [Stmt::Expr(expr)] => self.compile_quote_fold_expr(expr),
            _ => None,
        }
    }

    /// v0.28.21 — Fold a `comptime { ... }` block by spinning up a fresh
    /// interpreter over the file currently being compiled. The interpreter
    /// reuses the `comptime_results` pre-populated by `fold_comptime_items`,
    /// so any `comptime func` calls inside the block already have their
    /// values cached. The resulting `Value` is converted to an LLVM
    /// constant via `value_to_llvm_const` and returned as the block's value.
    fn fold_comptime_block(
        &mut self,
        block: &crate::ast::Block,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let file_rc = match self.comptime_file.clone() {
            Some(rc) => rc,
            None => {
                return Err(CompileError::Generic(
                    "comptime { ... } block encountered before compile_file stored the file context"
                        .to_string(),
                ));
            }
        };
        let mut interp = crate::interp::Interpreter::new(file_rc.as_ref());
        // Pre-load any pre-computed `comptime func` results so calls
        // inside the block resolve to the values already folded by
        // `fold_comptime_items`.
        for (name, value) in self.comptime_values.clone() {
            interp.inject_comptime_result(name, value);
        }
        let result = interp
            .eval_comptime_block(block)
            .map_err(|e| CompileError::Generic(format!("comptime block fold failed: {}", e)))?;
        self.value_to_llvm_const(&result)
    }

    /// v0.28.21 — Convert a small `interp::Value` scalar into an LLVM
    /// constant. Supports the shapes the v0.28.21 L1 tests need: int,
    /// float, bool, unit, and string. Tuples / lists are intentionally
    /// not yet supported; those will land in v0.28.22 alongside the
    /// `QuotedAst` codegen path.
    pub(crate) fn value_to_llvm_const(
        &self,
        v: &crate::interp::Value,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        use crate::interp::Value;
        let i64_ty = self.context.i64_type();
        let f64_ty = self.context.f64_type();
        match v {
            Value::Int(i) => Ok(BasicValueEnum::IntValue(i64_ty.const_int(*i as u64, true))),
            Value::Float(f) => Ok(BasicValueEnum::FloatValue(f64_ty.const_float(*f))),
            Value::Bool(b) => Ok(BasicValueEnum::IntValue(
                i64_ty.const_int(if *b { 1 } else { 0 }, false),
            )),
            Value::Unit => Ok(BasicValueEnum::IntValue(i64_ty.const_int(0, false))),
            Value::String(s) => {
                // Allocate a heap copy of the string instead of a .rodata global,
                // so that the memory can be safely freed by free_heap_allocs.
                let len = self.context.i64_type().const_int(s.len() as u64, false);
                // B4: OOM-safe heap copy for comptime string values.
                let heap_ptr = self.malloc_or_abort(len, "comptime_str_malloc")?;
                let i8_ty = self.context.i8_type();
                for (idx, &byte) in s.as_bytes().iter().enumerate() {
                    let gep = self.build_in_bounds_gep(
                        i8_ty,
                        heap_ptr,
                        &[self.context.i64_type().const_int(idx as u64, false)],
                        "comptime_str_gep",
                    )?;
                    self.build_store(gep, i8_ty.const_int(byte as u64, false))?;
                }
                let struct_ty = self.context.struct_type(
                    &[
                        BasicTypeEnum::PointerType(
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                        ),
                        BasicTypeEnum::IntType(self.context.i64_type()),
                    ],
                    false,
                );
                let sv = self
                    .builder
                    .build_insert_value(struct_ty.get_undef(), heap_ptr, 0, "comptime_str_data")
                    .map_err(|e| CompileError::LlvmError(format!("insert str data: {}", e)))?
                    .into_struct_value();
                let sv = self
                    .builder
                    .build_insert_value(sv, len, 1, "comptime_str_len")
                    .map_err(|e| CompileError::LlvmError(format!("insert str len: {}", e)))?
                    .into_struct_value();
                Ok(BasicValueEnum::StructValue(sv))
            }
            other => Err(CompileError::Generic(format!(
                "comptime fold: unsupported runtime value type {:?}; \
                 only Int/Float/Bool/Unit/String are folded in v0.28.21",
                std::mem::discriminant(other)
            ))),
        }
    }

    fn compile_quote_fold_expr(&self, expr: &Expr) -> Option<BasicValueEnum<'ctx>> {
        match expr {
            Expr::Literal(lit) => self.compile_literal_const(lit),
            Expr::Block(block) => match block.as_slice() {
                [Stmt::Expr(e)] => self.compile_quote_fold_expr(e),
                _ => None,
            },
            Expr::Binary(op, l, r) => {
                let lv = self.compile_quote_fold_expr(l)?;
                let rv = self.compile_quote_fold_expr(r)?;
                self.fold_const_binary(*op, lv, rv)
            }
            Expr::Unary(op, inner) => {
                let v = self.compile_quote_fold_expr(inner)?;
                self.fold_const_unary(*op, v)
            }
            _ => None,
        }
    }

    /// v0.28.21 — Fallback fold for a `quote! { ... }` block whose body
    /// isn't pure literal/arithmetic. Goes through the interpreter:
    ///   1. Convert the AST into a `QuotedAst` via `quote_block`.
    ///   2. Run `eval_quoted_ast` against the interpreter, which
    ///      resolves identifiers against the (codegen-time) scope.
    ///
    /// The result is then converted to an LLVM constant the same way as
    /// a literal fold. Variable bindings from `comptime func` results
    /// are seeded ahead of time so calls inside the quote block resolve
    /// without surprises.
    fn fold_quote_block(
        &mut self,
        block: &crate::ast::Block,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let file_rc = match self.comptime_file.clone() {
            Some(rc) => rc,
            None => {
                return Err(CompileError::Generic(
                    "quote! block encountered before compile_file stored the file context"
                        .to_string(),
                ));
            }
        };
        let mut interp = crate::interp::Interpreter::new(file_rc.as_ref());
        for (name, value) in self.comptime_values.clone() {
            interp.inject_comptime_result(name, value);
        }
        // Construct the QuotedAst from the block.
        let qa = interp.quote_block(block).map_err(|e| {
            CompileError::Generic(format!("quote! block construction failed: {}", e))
        })?;
        // Evaluate it. eval_quoted_ast will look up identifiers in the
        // interpreter's own scope (which starts empty at this point but
        // receives the seeded `comptime_results` above). Anything truly
        // runtime-only will surface as an InterpError.
        let result = interp.eval_quoted_ast(&qa).map_err(|e| {
            CompileError::Generic(format!(
                "quote! block fold: ast_eval failed: {} \
                 (v0.28.21 cannot yet lower this construct to a constant; \
                  if all variables are comptime-known, refactor to \
                  `comptime {{ ... }}` so the value can be folded directly)",
                e
            ))
        })?;
        self.value_to_llvm_const(&result)
    }

    /// v0.28.21 — Fold a `$(expr)` interpolation. The inner expression
    /// is evaluated through the interpreter at codegen time; its
    /// resulting `Value` becomes the splice point in the surrounding
    /// `quote!` block (which is itself evaluated by `fold_quote_block`
    /// or the explicit `ast_eval` builtin).
    fn fold_quote_interpolate(
        &mut self,
        inner: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let file_rc = match self.comptime_file.clone() {
            Some(rc) => rc,
            None => {
                return Err(CompileError::Generic(
                    "$() interpolation encountered before compile_file stored the file context"
                        .to_string(),
                ));
            }
        };
        let mut interp = crate::interp::Interpreter::new(file_rc.as_ref());
        for (name, value) in self.comptime_values.clone() {
            interp.inject_comptime_result(name, value);
        }
        let result = interp
            .eval_expr(inner)
            .map_err(|e| CompileError::Generic(format!("$() interpolation fold failed: {}", e)))?;
        self.value_to_llvm_const(&result)
    }

    /// v0.28.21 — Apply a binary op to two LLVM constant values at codegen
    /// time. Returns `None` if the operator or types are unsupported.
    fn fold_const_binary(
        &self,
        op: crate::ast::BinOp,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
    ) -> Option<BasicValueEnum<'ctx>> {
        use crate::ast::BinOp;
        // Helper: only fold when BOTH operands are compile-time constants.
        // If either is a runtime value, the fold must refuse rather than
        // silently substituting 0 — that would be a silent miscompilation.
        let (a, b) = match (l, r) {
            (BasicValueEnum::IntValue(a), BasicValueEnum::IntValue(b)) => (
                a.get_zero_extended_constant()?,
                b.get_zero_extended_constant()?,
            ),
            (BasicValueEnum::FloatValue(a), BasicValueEnum::FloatValue(b)) => {
                // audit (MEDIUM): fold float constants at compile time.
                // get_constant() returns Option<(f64, bool)> where bool is
                // the "is lossy" flag — we only care about the value.
                let (fa, _) = a.get_constant()?;
                let (fb, _) = b.get_constant()?;
                let result = match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        let v = match op {
                            BinOp::Add => fa + fb,
                            BinOp::Sub => fa - fb,
                            BinOp::Mul => fa * fb,
                            BinOp::Div => {
                                if fb == 0.0 {
                                    return None;
                                } else {
                                    fa / fb
                                }
                            }
                            _ => return None,
                        };
                        Some(BasicValueEnum::FloatValue(
                            self.context.f64_type().const_float(v),
                        ))
                    }
                    BinOp::EqCmp | BinOp::NeCmp | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        let b = match op {
                            BinOp::EqCmp => fa == fb,
                            BinOp::NeCmp => fa != fb,
                            BinOp::Lt => fa < fb,
                            BinOp::Le => fa <= fb,
                            BinOp::Gt => fa > fb,
                            BinOp::Ge => fa >= fb,
                            _ => return None,
                        };
                        Some(BasicValueEnum::IntValue(
                            self.context.bool_type().const_int(b as u64, false),
                        ))
                    }
                    _ => None,
                };
                return result;
            }
            _ => return None,
        };
        match op {
            BinOp::Add => Some(
                self.context
                    .i64_type()
                    .const_int(a.wrapping_add(b), false)
                    .into(),
            ),
            BinOp::Sub => Some(
                self.context
                    .i64_type()
                    .const_int(a.wrapping_sub(b), false)
                    .into(),
            ),
            BinOp::Mul => Some(
                self.context
                    .i64_type()
                    .const_int(a.wrapping_mul(b), false)
                    .into(),
            ),
            BinOp::Div => {
                if b == 0 {
                    return None;
                }
                Some(self.context.i64_type().const_int(a / b, false).into())
            }
            BinOp::Mod => {
                if b == 0 {
                    return None;
                }
                Some(self.context.i64_type().const_int(a % b, false).into())
            }
            BinOp::EqCmp => Some(
                self.context
                    .bool_type()
                    .const_int((a == b) as u64, false)
                    .into(),
            ),
            BinOp::NeCmp => Some(
                self.context
                    .bool_type()
                    .const_int((a != b) as u64, false)
                    .into(),
            ),
            BinOp::Lt => Some(
                self.context
                    .bool_type()
                    .const_int((a < b) as u64, false)
                    .into(),
            ),
            BinOp::Le => Some(
                self.context
                    .bool_type()
                    .const_int((a <= b) as u64, false)
                    .into(),
            ),
            BinOp::Gt => Some(
                self.context
                    .bool_type()
                    .const_int((a > b) as u64, false)
                    .into(),
            ),
            BinOp::Ge => Some(
                self.context
                    .bool_type()
                    .const_int((a >= b) as u64, false)
                    .into(),
            ),
            BinOp::And | BinOp::BitAnd => Some(
                self.context
                    .bool_type()
                    .const_int(((a != 0) && (b != 0)) as u64, false)
                    .into(),
            ),
            BinOp::Or | BinOp::BitOr => Some(
                self.context
                    .bool_type()
                    .const_int(((a != 0) || (b != 0)) as u64, false)
                    .into(),
            ),
            _ => None,
        }
    }

    /// v0.28.21 — Apply a unary op to a constant value at codegen time.
    fn fold_const_unary(
        &self,
        op: crate::ast::UnOp,
        v: BasicValueEnum<'ctx>,
    ) -> Option<BasicValueEnum<'ctx>> {
        use crate::ast::UnOp;
        match op {
            UnOp::Neg => match v {
                BasicValueEnum::IntValue(iv) => {
                    // Only fold if the operand is a compile-time constant.
                    // If `get_zero_extended_constant` returns None, the value
                    // is a runtime computation and we must not pretend to
                    // know its value — returning Some(0) here would be a
                    // silent miscompilation.
                    //
                    // audit (MEDIUM — fold_const_unary unsigned negation):
                    // Previously used `(!n).wrapping_add(1)` on the
                    // unsigned value from `get_zero_extended_constant()`.
                    // While the bit-level arithmetic is correct for
                    // two's complement, it is fragile and unclear.  We
                    // now interpret the constant as a signed i64 and
                    // use `wrapping_neg()` for a straightforward signed
                    // negation, then pass the sign-extended bit pattern
                    // to `const_int`.
                    let n = iv.get_sign_extended_constant()?;
                    Some(BasicValueEnum::IntValue(
                        self.context
                            .i64_type()
                            .const_int(n.wrapping_neg() as u64, true),
                    ))
                }
                BasicValueEnum::FloatValue(_) => {
                    // Float constant fold not yet supported (see
                    // fold_const_binary note).
                    None
                }
                _ => None,
            },
            UnOp::Not => match v {
                BasicValueEnum::IntValue(iv) => {
                    // Only fold if the operand is a compile-time constant;
                    // see §21 red line 2 (silent error swallow).
                    //
                    // audit (MEDIUM — fold_const_unary unsigned):
                    // Uses `get_zero_extended_constant()` (unsigned) here.
                    // For `Not` (logical negation) this is safe because
                    // the comparison `n == 0` gives the same result
                    // regardless of signedness.
                    let n = iv.get_zero_extended_constant()?;
                    Some(BasicValueEnum::IntValue(
                        self.context.i64_type().const_int((n == 0) as u64, false),
                    ))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn compile_literal_const(&self, lit: &Lit) -> Option<BasicValueEnum<'ctx>> {
        match lit {
            Lit::Int(v) => Some(self.context.i64_type().const_int(*v as u64, true).into()),
            Lit::Float(v) => Some(self.context.f64_type().const_float(*v).into()),
            Lit::Bool(v) => Some(self.context.bool_type().const_int(*v as u64, false).into()),
            Lit::String(s) => {
                let global = self.builder.build_global_string_ptr(s, "str").ok()?;
                Some(global.as_pointer_value().into())
            }
            Lit::Unit => Some(self.context.i64_type().const_int(0, false).into()),
            Lit::FString(_) => None,
        }
    }

    pub(super) fn compile_map_literal(
        &mut self,
        entries: &[(Expr, Expr)],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let map_new = self.get_runtime_fn("mimi_map_new")?;
        let result = self.build_call(map_new, &[], "map_new_call")?;
        let map_handle = call_try_basic_value(&result)
            .ok_or_else(|| CompileError::LlvmError("mimi_map_new returned void".to_string()))?
            .into_int_value();

        let map_set = self.get_runtime_fn("mimi_map_set")?;

        for (key, value) in entries {
            let key_val = self.compile_expr(key, vars)?;
            let val_val = self.compile_expr(value, vars)?;
            // Key must be a string pointer
            let key_ptr = match &key_val {
                BasicValueEnum::PointerValue(pv) => *pv,
                BasicValueEnum::StructValue(sv) => self
                    .build_extract_value((*sv).into(), 0, "key_str_ptr")?
                    .into_pointer_value(),
                _ => return Err("map literal key must be a string".into()),
            };
            // Value is cast to i64 (ValueHandle) for storage
            let val_i64 = self.any_value_to_handle(val_val)?;
            self.build_call(
                map_set,
                &[
                    BasicMetadataValueEnum::IntValue(map_handle),
                    BasicMetadataValueEnum::PointerValue(key_ptr),
                    BasicMetadataValueEnum::IntValue(val_i64),
                ],
                "map_set_call",
            )?;
        }

        Ok(BasicValueEnum::IntValue(map_handle))
    }

    pub(super) fn compile_set_literal(
        &mut self,
        elems: &[Expr],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let set_new = self.get_runtime_fn("mimi_set_new")?;
        let result = self.build_call(set_new, &[], "set_new_call")?;
        let set_handle = call_try_basic_value(&result)
            .ok_or_else(|| CompileError::LlvmError("mimi_set_new returned void".to_string()))?
            .into_int_value();

        let set_insert = self.get_runtime_fn("mimi_set_insert")?;

        for elem in elems {
            let val = self.compile_expr(elem, vars)?;
            let val_i64 = self.any_value_to_handle(val)?;
            self.build_call(
                set_insert,
                &[
                    BasicMetadataValueEnum::IntValue(set_handle),
                    BasicMetadataValueEnum::IntValue(val_i64),
                ],
                "set_insert_call",
            )?;
        }

        Ok(BasicValueEnum::IntValue(set_handle))
    }

    /// Convert any basic value to an i64 ValueHandle for map/set storage.
    /// Integers are stored directly (no tagging). Pointers are stored as ptrtoint.
    /// The runtime's `mimi_any_to_string` handles both tagged and untagged values.
    fn any_value_to_handle(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<IntValue<'ctx>, CompileError> {
        Ok(match val {
            BasicValueEnum::IntValue(iv) => {
                let i64_ty = self.context.i64_type();
                // Extend i32 (or narrower) to i64 for consistent storage.
                if iv.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_ty, "any_sext")
                        .map_err(|e| CompileError::LlvmError(format!("s_ext error: {}", e)))?
                } else {
                    iv
                }
            }
            BasicValueEnum::PointerValue(pv) => {
                self.build_ptr_to_int(pv, self.context.i64_type(), "ptr_to_handle")?
            }
            BasicValueEnum::StructValue(sv) => {
                // Extract first field (string struct has ptr at 0)
                let field = self.build_extract_value(sv.into(), 0, "struct_field")?;
                match field {
                    BasicValueEnum::PointerValue(pv) => {
                        self.build_ptr_to_int(pv, self.context.i64_type(), "struct_ptr_to_handle")?
                    }
                    BasicValueEnum::IntValue(iv) => iv,
                    _ => return Err("unsupported struct field type for map value handle".into()),
                }
            }
            BasicValueEnum::FloatValue(fv) => self
                .build_bit_cast(fv.into(), self.context.i64_type().into(), "float_to_handle")?
                .into_int_value(),
            _ => return Err("unsupported value type for map storage".into()),
        })
    }

    // ===================================================================
    // v0.28.21 — Runtime QuotedAst construction (malloc + tagged union)
    //
    // These methods complement the compile-time folding path by emitting
    // `mimi_quote_new_*` runtime calls that build a heap-allocated
    // `MimiQuotedAst` tree. Used when `Expr::Quote(block)` references
    // runtime-only symbols that cannot be folded at codegen time.
    // ===================================================================

    /// Entry point: build a runtime QuotedAst block node.
    fn compile_quote_runtime(
        &mut self,
        block: &crate::ast::Block,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let i8_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let len = block.len();
        // Allocate a stack array to hold child pointers.
        let children_alloca = self.build_alloca(i8_ty.array_type(len as u32), "quote_children")?;
        for (i, stmt) in block.iter().enumerate() {
            let child_ptr = self.compile_quote_runtime_stmt(stmt)?; // returns i8*
            let gep = self
                .gep()
                .build_in_bounds_gep(
                    i8_ty,
                    children_alloca,
                    &[i64_ty.const_int(i as u64, false)],
                    "quote_child_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("quote child gep: {}", e)))?;
            self.build_store(gep, child_ptr)?;
        }
        let children_ptr = self
            .build_load(i8_ty, children_alloca, "quote_children_load")?
            .into_pointer_value();
        // Call mimi_quote_new_list(QAST_BLOCK=14, children_ptr, len)
        let new_list = self
            .module
            .get_function("mimi_quote_new_list")
            .ok_or("mimi_quote_new_list not declared")?;
        let result = self
            .builder
            .build_call(
                new_list,
                &[
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(14, false)),
                    BasicMetadataValueEnum::PointerValue(children_ptr),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(len as u64, false)),
                ],
                "quote_block_ptr",
            )
            .map_err(|e| CompileError::LlvmError(format!("quote new list: {}", e)))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_quote_new_list void")?)
    }

    /// Emit a runtime QuotedAst node for a single statement.
    fn compile_quote_runtime_stmt(
        &mut self,
        stmt: &crate::ast::Stmt,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        use crate::ast::Stmt;
        let i8_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let null_i8 = i8_ty.const_zero();

        match stmt {
            Stmt::Expr(e) => self.compile_quote_runtime_expr(e),
            Stmt::Block(block) => self.compile_quote_runtime(block),
            Stmt::Return(e) => {
                let inner = if let Some(e) = e {
                    self.compile_quote_runtime_expr(e)?
                } else {
                    BasicValueEnum::PointerValue(null_i8)
                };
                self.call_quote_new_node(1, inner, null_i8.into(), 0) // QAST_RETURN=1
            }
            Stmt::Continue => self.call_quote_new_leaf(19, self.i64_const(0)), // QAST_CONTINUE
            Stmt::Let { pat, init, .. } => {
                let name = match pat {
                    crate::ast::Pattern::Variable(n) => n.clone(),
                    _ => return Err("let pattern not supported in runtime quote".into()),
                };
                let name_ptr = self
                    .builder
                    .build_global_string_ptr(&name, "q_let_name")
                    .map_err(|e| CompileError::LlvmError(format!("quote name: {}", e)))?;
                let value = if let Some(init) = init {
                    self.compile_quote_runtime_expr(init)?
                } else {
                    BasicValueEnum::PointerValue(null_i8)
                };
                self.call_quote_new_node(
                    16, // QAST_LET
                    BasicValueEnum::PointerValue(name_ptr.as_pointer_value()),
                    value,
                    0,
                )
            }
            Stmt::Loop(body) => {
                let b = self.compile_quote_runtime(body)?;
                self.call_quote_new_node(23, b, null_i8.into(), 0) // QAST_LOOP
            }
            _ => Err(CompileError::Generic(format!(
                "unsupported statement in runtime QuotedAst: {:?}",
                stmt
            ))),
        }
    }

    /// Emit a runtime QuotedAst node for a single expression.
    fn compile_quote_runtime_expr(
        &mut self,
        expr: &crate::ast::Expr,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        use crate::ast::{Expr, Lit};
        let i8_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let null_i8 = i8_ty.const_zero();

        match expr {
            Expr::Literal(lit) => match lit {
                Lit::Int(v) => self.call_quote_new_leaf(0, self.i64_const(*v)), // QAST_INT
                Lit::Float(v) => self.call_quote_new_leaf(1, self.i64_const(v.to_bits() as i64)), // QAST_FLOAT
                Lit::Bool(v) => {
                    self.call_quote_new_leaf(2, self.i64_const(if *v { 1 } else { 0 }))
                } // QAST_BOOL
                Lit::String(s) => {
                    let global = self
                        .builder
                        .build_global_string_ptr(s, "q_str")
                        .map_err(|e| CompileError::LlvmError(format!("quote str: {}", e)))?;
                    // CG-C1: emit real ptrtoint so the runtime leaf stores the string pointer.
                    let ptr_i64 = self.build_ptr_to_int(
                        global.as_pointer_value(),
                        self.context.i64_type(),
                        "q_str_i64",
                    )?;
                    self.call_quote_new_leaf(3, ptr_i64)
                }
                Lit::Unit => self.call_quote_new_leaf(4, self.i64_const(0)), // QAST_UNIT
                Lit::FString(_) => Err("f-string in runtime QuotedAst not supported".into()),
            },
            Expr::Ident(name) => {
                let global = self
                    .builder
                    .build_global_string_ptr(name, "q_ident")
                    .map_err(|e| CompileError::LlvmError(format!("quote ident: {}", e)))?;
                // CG-C1: emit real ptrtoint for identifier name string.
                let ptr_i64 = self.build_ptr_to_int(
                    global.as_pointer_value(),
                    self.context.i64_type(),
                    "q_ident_i64",
                )?;
                self.call_quote_new_leaf(5, ptr_i64)
            }
            Expr::Binary(op, l, r) => {
                let lv = self.compile_quote_runtime_expr(l)?;
                let rv = self.compile_quote_runtime_expr(r)?;
                self.call_quote_new_node(6, lv, rv, *op as i64) // QAST_BINARY
            }
            Expr::Unary(op, inner) => {
                let v = self.compile_quote_runtime_expr(inner)?;
                self.call_quote_new_node(7, v, null_i8.into(), *op as i64) // QAST_UNARY
            }
            Expr::Block(block) => self.compile_quote_runtime(block),
            Expr::QuoteInterpolate(inner) => {
                // Evaluate interpolation at codegen time and wrap as leaf.
                let val = self.fold_quote_interpolate(inner)?;
                let val_i64 = self.basic_value_to_i64(val, "q_interp")?;
                self.call_quote_new_leaf(15, val_i64) // QAST_INTERP
            }
            Expr::Tuple(items) => {
                let children = self.build_quote_children_list(items)?;
                self.call_quote_new_list(11, children, items.len()) // QAST_TUPLE
            }
            _ => Err(CompileError::Generic(format!(
                "unsupported expression in runtime QuotedAst: {:?}",
                expr
            ))),
        }
    }

    // ---------- Helper methods ----------

    /// Emit `mimi_quote_new_leaf(tag, value) -> i8*`.
    ///
    /// `value` is an LLVM i64 SSA value (constant or `ptrtoint` result).
    /// CG-C1: previously took a Rust `i64` and forced pointers through a
    /// compile-time-zero stub — string/ident leaves lost their data.
    fn call_quote_new_leaf(
        &self,
        tag: i32,
        value: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let func = self
            .module
            .get_function("mimi_quote_new_leaf")
            .ok_or("mimi_quote_new_leaf not declared")?;
        let i32_ty = self.context.i32_type();
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(tag as u64, true)),
                    BasicMetadataValueEnum::IntValue(value),
                ],
                "q_leaf",
            )
            .map_err(|e| CompileError::LlvmError(format!("q leaf: {}", e)))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_quote_new_leaf void")?)
    }

    fn i64_const(&self, v: i64) -> inkwell::values::IntValue<'ctx> {
        self.context.i64_type().const_int(v as u64, false)
    }

    /// Convert a BasicValueEnum to i64 for quote leaf payloads (CG-C1).
    fn basic_value_to_i64(
        &self,
        val: BasicValueEnum<'ctx>,
        name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
        let i64_ty = self.context.i64_type();
        match val {
            BasicValueEnum::IntValue(iv) => {
                let bw = iv.get_type().get_bit_width();
                if bw == 64 {
                    Ok(iv)
                } else if bw == 1 {
                    self.builder
                        .build_int_z_extend(iv, i64_ty, &format!("{}_zext", name))
                        .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))
                } else if bw < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_ty, &format!("{}_sext", name))
                        .map_err(|e| CompileError::LlvmError(format!("sext: {}", e)))
                } else {
                    self.builder
                        .build_int_truncate(iv, i64_ty, &format!("{}_trunc", name))
                        .map_err(|e| CompileError::LlvmError(format!("trunc: {}", e)))
                }
            }
            BasicValueEnum::PointerValue(pv) => {
                self.build_ptr_to_int(pv, i64_ty, &format!("{}_ptr", name))
            }
            BasicValueEnum::FloatValue(fv) => self
                .builder
                .build_bit_cast(fv, i64_ty, &format!("{}_fbits", name))
                .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))
                .map(|v| v.into_int_value()),
            _ => Ok(i64_ty.const_zero()),
        }
    }

    /// Emit `mimi_quote_new_node(tag, child0, child1, extra) -> i8*`.
    fn call_quote_new_node(
        &self,
        tag: i32,
        child0: BasicValueEnum<'ctx>,
        child1: BasicValueEnum<'ctx>,
        extra: i64,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let func = self
            .module
            .get_function("mimi_quote_new_node")
            .ok_or("mimi_quote_new_node not declared")?;
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let c0_ptr = self.to_i8_ptr(child0);
        let c1_ptr = self.to_i8_ptr(child1);
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(tag as u64, true)),
                    BasicMetadataValueEnum::PointerValue(c0_ptr),
                    BasicMetadataValueEnum::PointerValue(c1_ptr),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(extra as u64, false)),
                ],
                "q_node",
            )
            .map_err(|e| CompileError::LlvmError(format!("q node: {}", e)))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_quote_new_node void")?)
    }

    /// Emit `mimi_quote_new_list(tag, children_ptr, len) -> i8*`.
    fn call_quote_new_list(
        &self,
        tag: i32,
        children_ptr: inkwell::values::PointerValue<'ctx>,
        len: usize,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let func = self
            .module
            .get_function("mimi_quote_new_list")
            .ok_or("mimi_quote_new_list not declared")?;
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let result = self
            .builder
            .build_call(
                func,
                &[
                    BasicMetadataValueEnum::IntValue(i32_ty.const_int(tag as u64, false)),
                    BasicMetadataValueEnum::PointerValue(children_ptr),
                    BasicMetadataValueEnum::IntValue(i64_ty.const_int(len as u64, false)),
                ],
                "q_list",
            )
            .map_err(|e| CompileError::LlvmError(format!("q list: {}", e)))?;
        Ok(call_try_basic_value(&result).ok_or("mimi_quote_new_list void")?)
    }

    /// Convert a `BasicValueEnum` to an `i8*` (for child pointers).
    fn to_i8_ptr(&self, val: BasicValueEnum<'ctx>) -> inkwell::values::PointerValue<'ctx> {
        match val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => self
                .context
                .ptr_type(inkwell::AddressSpace::default())
                .const_zero(),
        }
    }

    /// Build a children pointer array from a list of expressions.
    fn build_quote_children_list(
        &mut self,
        items: &[crate::ast::Expr],
    ) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
        let i8_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let alloca = self.build_alloca(i8_ty.array_type(items.len() as u32), "q_children")?;
        for (i, item) in items.iter().enumerate() {
            let child = self.compile_quote_runtime_expr(item)?;
            // SAFETY: ptr is the alloca just above, indices are compile-time
            // constant in-bounds values into that array, and result_type
            // matches. Delegates to CheckedGepBuilder to absorb the unsafe
            // boundary in one place.
            let gep = self
                .gep()
                .build_in_bounds_gep(
                    i8_ty,
                    alloca,
                    &[i64_ty.const_int(i as u64, false)],
                    "q_child_gep",
                )
                .map_err(|e| CompileError::LlvmError(format!("q child gep: {}", e)))?;
            self.build_store(gep, child)?;
        }
        Ok(self
            .build_load(i8_ty, alloca, "q_children_load")?
            .into_pointer_value())
    }
}

mod access;
pub(super) mod call;
mod control;
mod lambda;
mod literal;
mod r#match;
mod operator;
mod record;
mod try_expr;
mod type_expr;
