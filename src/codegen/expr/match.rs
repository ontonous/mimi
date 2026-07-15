use crate::ast::*;
use crate::codegen::CallSiteValueExt;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use std::collections::HashMap;

/// Immutable context shared between match dispatch and body compilation.
struct MatchArmEnv<'ctx> {
    scrutinee_val: BasicValueEnum<'ctx>,
    scrutinee_iv: Option<IntValue<'ctx>>,
    merge_bb: BasicBlock<'ctx>,
    else_bb: BasicBlock<'ctx>,
}

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn bind_pattern_variables(
        &mut self,
        arm: &MatchArm,
        scrutinee_val: BasicValueEnum<'ctx>,
        scrutinee_iv: Option<inkwell::values::IntValue<'ctx>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<HashMap<String, VarEntry<'ctx>>, CompileError> {
        let mut local_vars = vars.clone();
        // Bind variables from pattern
        match &arm.pat {
            Pattern::Variable(name) => {
                // Uppercase identifiers that name enum variants are treated as
                // unit constructor patterns, not variable bindings.
                if self.find_variant_ordinal(name).is_ok() {
                    return Ok(local_vars);
                }
                let (val, ty) = if let Some(iv) = scrutinee_iv {
                    (iv.into(), BasicTypeEnum::IntType(iv.get_type()))
                } else {
                    match scrutinee_val {
                        BasicValueEnum::IntValue(iv) => {
                            (iv.into(), BasicTypeEnum::IntType(iv.get_type()))
                        }
                        _ => {
                            // For non-integer scrutinees bind the value directly.
                            let ty = scrutinee_val.get_type();
                            (scrutinee_val, ty)
                        }
                    }
                };
                self.bind_pattern_var(&mut local_vars, name, val, ty)?;
            }
            Pattern::Constructor(name, inner_patterns) => {
                // Newtypes are transparent: the constructor pattern binds the
                // inner variable directly to the scrutinee value.
                if let Some(td) = self.type_defs.get(name) {
                    if matches!(td.kind, crate::ast::TypeDefKind::Newtype(_)) {
                        if let Some(first) = inner_patterns.first() {
                            self.compile_pattern_bind(&first.1, scrutinee_val, &mut local_vars)?;
                        }
                        return Ok(local_vars);
                    }
                }
                // For constructor patterns, bind inner variables from the payload field.
                // Most enum-like representations put the tag at index 0 and the payload
                // at index 1. Built-in Result<T,E> is special: Ok uses index 1, Err uses
                // index 2 for its error payload.
                // Built-in Result<T,E> uses {bool disc, T ok, i64 err} layout
                // where Err's payload is at index 2. Custom enums use {i32 tag, payload}
                // where all payload variants use index 1.
                let payload_idx = if name == "Err"
                    && !self.type_defs.values().any(|td|
                        matches!(&td.kind, TypeDefKind::Enum(v) if v.iter().any(|va| va.name == "Err"))
                    )
                {
                    // "Err" not owned by any custom enum → built-in Result<T,E> layout
                    2
                } else {
                    1
                };
                // P0-2: For custom enums, multi-arg variants pack their fields into
                // a struct that lives at the i64 payload slot (ptrtoint-encoded).
                // We need to decode that struct and bind each inner pattern to its
                // respective field, instead of binding the entire payload to every
                // inner pattern variable.
                let variant_owner = self.find_variant_owner(name);
                let variant_arg_tys: Option<Vec<crate::ast::Type>> =
                    variant_owner.as_ref().and_then(|(owner, _)| {
                        self.type_defs.get(owner).and_then(|td| {
                            if let TypeDefKind::Enum(variants) = &td.kind {
                                variants.iter().find(|v| v.name == *name).and_then(|v| {
                                    match &v.payload {
                                        Some(VariantPayload::Tuple(ts)) if ts.len() > 1 => {
                                            Some(ts.clone())
                                        }
                                        _ => None,
                                    }
                                })
                            } else {
                                None
                            }
                        })
                    });
                let (payload, payload_ty) = match scrutinee_val {
                    BasicValueEnum::StructValue(sv) => {
                        let payload_val = self
                            .builder
                            .build_extract_value(sv, payload_idx, "payload")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("extract payload: {}", e))
                            })?;
                        // Check if the variant's payload is a struct type (ptrtoint encoded)
                        let (decoded, ty) = self.decode_payload_struct(name, payload_val, None)?;
                        (decoded, ty)
                    }
                    BasicValueEnum::PointerValue(pv) => {
                        // Use the actual registered struct type from type_llvm if
                        // available, instead of the synthetic {i32,i64} which is
                        // a UB type mismatch when the real layout differs (e.g.
                        // {i32, i32, i32} for 2-field payload or {i32, f64}).
                        let real_ty = variant_owner
                            .as_ref()
                            .and_then(|(owner, _)| self.type_llvm.get(owner))
                            .and_then(|bt| match bt {
                                BasicTypeEnum::StructType(st) => Some(*st),
                                _ => None,
                            });
                        let struct_ty = real_ty.unwrap_or_else(|| {
                            let i32_ty = BasicTypeEnum::IntType(self.context.i32_type());
                            let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
                            self.context.struct_type(&[i32_ty, i64_ty], false)
                        });
                        let loaded = self.build_load(
                            BasicTypeEnum::StructType(struct_ty),
                            pv,
                            "enum_loaded",
                        )?;
                        let sv = match loaded {
                            BasicValueEnum::StructValue(sv) => sv,
                            _ => {
                                return Err(
                                    "constructor pattern: expected struct from pointer".into()
                                )
                            }
                        };
                        let payload_val = self
                            .builder
                            .build_extract_value(sv, payload_idx, "payload")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("extract payload: {}", e))
                            })?;
                        let (decoded, ty) = self.decode_payload_struct(name, payload_val, None)?;
                        (decoded, ty)
                    }
                    BasicValueEnum::IntValue(iv) => {
                        // Legacy/compact representation: some enum values are passed as a
                        // single integer (e.g. nested enum payloads). Bind the payload to the
                        // integer itself so that nested pattern matches still compile.
                        (iv.into(), BasicTypeEnum::IntType(iv.get_type()))
                    }
                    _ => return Err("constructor pattern requires enum struct value".into()),
                };
                if let Some(arg_tys) = variant_arg_tys {
                    // P0-2: Multi-arg variant — the constructor packed the
                    // args into a struct on the heap and stored the ptrtoint
                    // result in the i64 payload slot. Int-toptr + load to
                    // recover the struct, then bind each inner pattern to
                    // the corresponding field.
                    let payload_int = match payload {
                        BasicValueEnum::IntValue(iv) => iv,
                        BasicValueEnum::PointerValue(pv) => self
                            .builder
                            .build_ptr_to_int(pv, self.context.i64_type(), "payload_int_recover")
                            .map_err(|e| CompileError::LlvmError(format!("ptr2int: {}", e)))?,
                        _ => {
                            return Err("multi-arg constructor pattern: expected int payload".into())
                        }
                    };
                    let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(arg_tys.len());
                    let mut all_known = true;
                    for t in &arg_tys {
                        if let Some(ty) = self.llvm_type_for(t) {
                            field_tys.push(ty);
                        } else {
                            all_known = false;
                            break;
                        }
                    }
                    if !all_known || field_tys.is_empty() {
                        return Err(
                            "multi-arg constructor pattern: cannot resolve payload field types"
                                .into(),
                        );
                    }
                    let packed_ty = self.context.struct_type(&field_tys, false);
                    let packed_ty_enum = BasicTypeEnum::StructType(packed_ty);
                    let ptr = self
                        .builder
                        .build_int_to_ptr(
                            payload_int,
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                            "multi_payload_ptr",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?;
                    let payload_sv = self
                        .builder
                        .build_load(packed_ty_enum, ptr, "multi_payload_struct")
                        .map_err(|e| {
                            CompileError::LlvmError(format!("load multi payload struct: {}", e))
                        })?
                        .into_struct_value();
                    let payload_ptr = self.build_alloca(packed_ty_enum, "multi_payload_alloca")?;
                    self.build_store(payload_ptr, payload_sv)?;
                    for (j, (_, inner_pat)) in inner_patterns.iter().enumerate() {
                        if let Pattern::Variable(pname) = inner_pat {
                            if j >= arg_tys.len() {
                                break;
                            }
                            let elem_ty = packed_ty
                                .get_field_type_at_index(j as u32)
                                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                            let gep = self
                                .gep()
                                .build_struct_gep(
                                    packed_ty_enum,
                                    payload_ptr,
                                    j as u32,
                                    &format!("multi_el{}", j),
                                )
                                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                            let val = self.build_load(elem_ty, gep, &format!("multi_v{}", j))?;
                            self.bind_pattern_var(&mut local_vars, pname, val, elem_ty)?;
                        }
                    }
                } else {
                    // Single-arg constructor: bind payload and register List
                    // element types when payload is List<Enum>/List<Record>
                    // so xs[i] can reconstruct ptrtoint-encoded structs.
                    let payload_ast = variant_owner.as_ref().and_then(|(owner, _)| {
                        self.type_defs.get(owner).and_then(|td| {
                            if let TypeDefKind::Enum(variants) = &td.kind {
                                variants.iter().find(|v| v.name == *name).and_then(|v| {
                                    match &v.payload {
                                        Some(VariantPayload::Tuple(ts)) if ts.len() == 1 => {
                                            Some(ts[0].clone())
                                        }
                                        _ => None,
                                    }
                                })
                            } else {
                                None
                            }
                        })
                    });
                    for (_, inner_pat) in inner_patterns {
                        if let Pattern::Variable(bind_name) = inner_pat {
                            self.bind_pattern_var(
                                &mut local_vars,
                                bind_name,
                                payload,
                                payload_ty,
                            )?;
                            if let Some(ref ast_ty) = payload_ast {
                                self.var_types.insert(bind_name.clone(), ast_ty.clone());
                                if let Some(full) = self.get_full_type_name(ast_ty) {
                                    self.var_type_names.insert(bind_name.clone(), full);
                                }
                                self.register_list_elem_type(bind_name, ast_ty);
                            }
                        }
                    }
                }
            }
            Pattern::Tuple(inner_pats) => {
                // For tuple patterns, bind inner variables by loading from struct.
                // Prefer the actual struct type from the scrutinee value when available,
                // falling back to tuple_type_stack only for PointerValue scrutinees.
                let (struct_ty, scrutinee_ptr) = match scrutinee_val {
                    BasicValueEnum::StructValue(sv) => {
                        let actual_ty = sv.get_type();
                        let alloca = self.build_alloca(actual_ty, "tuple_alloca")?;
                        self.build_store(alloca, sv)?;
                        (actual_ty, alloca)
                    }
                    BasicValueEnum::PointerValue(pv) => {
                        let stack_ty = *self.tuple_type_stack.last().ok_or_else(|| {
                            CompileError::LlvmError(
                                "tuple_type_stack empty for tuple pattern bind".to_string(),
                            )
                        })?;
                        (stack_ty, pv)
                    }
                    _ => return Ok(local_vars),
                };
                let struct_ty_enum = BasicTypeEnum::StructType(struct_ty);
                for (j, inner_pat) in inner_pats.iter().enumerate() {
                    if let Pattern::Variable(name) = inner_pat {
                        let elem_ty = struct_ty
                            .get_field_type_at_index(j as u32)
                            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                        let gep = self
                            .gep()
                            .build_struct_gep(
                                struct_ty_enum,
                                scrutinee_ptr,
                                j as u32,
                                &format!("tuple_{}", j),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        let val = self.build_load(elem_ty, gep, &format!("tup_{}", j))?;
                        self.bind_pattern_var(&mut local_vars, name, val, elem_ty)?;
                    }
                }
            }
            Pattern::Array(inner_pats) => {
                // For array patterns, bind inner variables by loading from list data
                let scrutinee_ptr = match scrutinee_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Ok(local_vars),
                };
                let data_ptr = self.load_list_data_ptr(scrutinee_ptr)?;
                self.bind_list_prefix(data_ptr, inner_pats, &mut local_vars)?;
            }
            Pattern::Slice(inner_pats, rest) => {
                // For slice patterns, bind prefix variables and rest as list
                let scrutinee_ptr = match scrutinee_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Ok(local_vars),
                };
                let data_ptr = self.load_list_data_ptr(scrutinee_ptr)?;
                self.bind_list_prefix(data_ptr, inner_pats, &mut local_vars)?;

                // Bind rest as remaining list (simplified: bind as empty list)
                if let Some(rest_pat) = rest.as_ref() {
                    if let Pattern::Variable(name) = rest_pat.as_ref() {
                        let i64_ty = self.context.i64_type();
                        let empty_list: BasicValueEnum = i64_ty.const_int(0, false).into();
                        self.bind_pattern_var(
                            &mut local_vars,
                            name,
                            empty_list,
                            BasicTypeEnum::IntType(i64_ty),
                        )?;
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) => {
                // Wildcard and literal patterns: no variable binding needed
            }
        }
        Ok(local_vars)
    }

    /// Bind a single pattern variable to a fresh alloca.
    fn bind_pattern_var(
        &self,
        local_vars: &mut HashMap<String, VarEntry<'ctx>>,
        name: &str,
        val: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
    ) -> Result<(), CompileError> {
        let alloca = self.build_alloca(ty, name)?;
        self.build_store(alloca, val)?;
        local_vars.insert(name.to_string(), (alloca, ty));
        Ok(())
    }

    /// Load the i64 data pointer from a list struct pointer.
    fn load_list_data_ptr(
        &self,
        scrutinee_ptr: PointerValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, CompileError> {
        let list_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i64_type()),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            ],
            false,
        );
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_i8 = self
            .build_load(
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                data_gep,
                "data",
            )?
            .into_pointer_value();
        let data_ptr = self
            .builder
            .build_bit_cast(
                data_i8,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "data_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?
            .into_pointer_value();
        Ok(data_ptr)
    }

    /// Bind prefix variables of a list pattern by loading from an i64 data pointer.
    fn bind_list_prefix(
        &self,
        data_ptr: PointerValue<'ctx>,
        inner_pats: &[Pattern],
        local_vars: &mut HashMap<String, VarEntry<'ctx>>,
    ) -> Result<(), CompileError> {
        let i64_ty = self.context.i64_type();
        for (j, inner_pat) in inner_pats.iter().enumerate() {
            if let Pattern::Variable(name) = inner_pat {
                let idx = i64_ty.const_int(j as u64, false);
                let elem_ptr = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], &format!("arr_{}", j))
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let val = self.build_load(
                    BasicTypeEnum::IntType(i64_ty),
                    elem_ptr,
                    &format!("arrv_{}", j),
                )?;
                self.bind_pattern_var(local_vars, name, val, BasicTypeEnum::IntType(i64_ty))?;
            }
        }
        Ok(())
    }

    /// Generate element-wise comparison for a tuple pattern.
    /// Returns `Some(i1)` if any element requires comparison, `None` for wildcard-only patterns.
    fn compile_tuple_pattern(
        &self,
        scrutinee: BasicValueEnum<'ctx>,
        inner_pats: &[Pattern],
    ) -> Result<Option<inkwell::values::IntValue<'ctx>>, CompileError> {
        let i64_ty = self.context.i64_type();
        // Normalize to a struct value so we can use the real struct type for GEPs.
        let (tuple_ptr, struct_ty) = match scrutinee {
            BasicValueEnum::PointerValue(pv) => {
                let struct_ty = *self.tuple_type_stack.last().ok_or_else(|| {
                    CompileError::LlvmError("tuple_type_stack empty for tuple pattern".to_string())
                })?;
                let loaded = self.build_load(struct_ty, pv, "tuple_pat_loaded")?;
                let sv = match loaded {
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Err("tuple pattern: expected struct from pointer".into()),
                };
                let alloca = self.build_alloca(struct_ty, "tuple_alloca")?;
                self.build_store(alloca, sv)?;
                (alloca, struct_ty)
            }
            BasicValueEnum::StructValue(sv) => {
                let struct_ty = sv.get_type();
                let alloca = self.build_alloca(struct_ty, "tuple_alloca")?;
                self.build_store(alloca, sv)?;
                (alloca, struct_ty)
            }
            _ => return Err("tuple pattern requires struct value".into()),
        };
        let struct_ty_enum = BasicTypeEnum::StructType(struct_ty);
        let mut agg: Option<inkwell::values::IntValue<'ctx>> = None;
        for (j, pat) in inner_pats.iter().enumerate() {
            let lit_val = match pat {
                Pattern::Literal(lit) => match lit {
                    Lit::Int(n) => Some(i64_ty.const_int(*n as u64, true)),
                    Lit::Bool(b) => Some(i64_ty.const_int(*b as u64, false)),
                    Lit::Unit => Some(i64_ty.const_int(0, false)),
                    _ => return Err("unsupported tuple element literal type".into()),
                },
                _ => None,
            };
            if let Some(expected) = lit_val {
                let elem_ptr = self
                    .gep()
                    .build_struct_gep(struct_ty_enum, tuple_ptr, j as u32, &format!("tup_el{}", j))
                    .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                let elem_ty = struct_ty
                    .get_field_type_at_index(j as u32)
                    .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                let elem_val = self
                    .build_load(elem_ty, elem_ptr, &format!("tup_v{}", j))?
                    .into_int_value();
                let eq = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        elem_val,
                        expected,
                        &format!("tup_cmp{}", j),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                agg = Some(match agg {
                    Some(prev) => self
                        .builder
                        .build_and(prev, eq, "tup_and")
                        .map_err(|e| CompileError::LlvmError(format!("and: {}", e)))?,
                    None => eq,
                });
            }
        }
        Ok(agg)
    }

    /// Generate element-wise comparison for an array pattern.
    /// Returns `Some(i1)` if any element requires comparison, `None` for wildcard-only patterns.
    fn compile_array_pattern(
        &self,
        scrutinee: BasicValueEnum<'ctx>,
        inner_pats: &[Pattern],
    ) -> Result<Option<inkwell::values::IntValue<'ctx>>, CompileError> {
        let i64_ty = self.context.i64_type();
        let scrutinee_ptr = match scrutinee {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => return Ok(None),
        };
        let list_ty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(i64_ty),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            ],
            false,
        );
        let data_gep = self
            .gep()
            .build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let data_i8 = self
            .build_load(
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                data_gep,
                "data",
            )?
            .into_pointer_value();
        let data_ptr = self
            .builder
            .build_bit_cast(
                data_i8,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "data_i64",
            )
            .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?
            .into_pointer_value();
        let mut agg: Option<inkwell::values::IntValue<'ctx>> = None;
        for (j, pat) in inner_pats.iter().enumerate() {
            let lit_val = match pat {
                Pattern::Literal(lit) => match lit {
                    Lit::Int(n) => Some(i64_ty.const_int(*n as u64, true)),
                    Lit::Bool(b) => Some(i64_ty.const_int(*b as u64, false)),
                    Lit::Unit => Some(i64_ty.const_int(0, false)),
                    _ => return Err("unsupported array element literal type".into()),
                },
                _ => None,
            };
            if let Some(expected) = lit_val {
                let idx = i64_ty.const_int(j as u64, false);
                // SAFETY: pointer derived from valid list data allocation
                let elem_ptr = {
                    self.gep()
                        .build_gep(i64_ty, data_ptr, &[idx], &format!("arr_el{}", j))
                }
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                let elem_val = self
                    .build_load(
                        BasicTypeEnum::IntType(i64_ty),
                        elem_ptr,
                        &format!("arr_v{}", j),
                    )?
                    .into_int_value();
                let eq = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        elem_val,
                        expected,
                        &format!("arr_cmp{}", j),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                agg = Some(match agg {
                    Some(prev) => self
                        .builder
                        .build_and(prev, eq, "arr_and")
                        .map_err(|e| CompileError::LlvmError(format!("and: {}", e)))?,
                    None => eq,
                });
            }
        }
        Ok(agg)
    }

    /// Check if a variant's payload i64 was ptrtoint-encoded from a struct type,
    /// and if so, decode it back to the struct value.
    ///
    /// Built-in `Result`/`Option` variants (`Ok`, `Err`, `Some`) store their
    /// payload directly in the variant struct layout, so the extracted value
    /// already has the correct LLVM type. Only custom enum variants use the
    /// compact `{i32 tag, i64 payload}` representation that may be
    /// ptrtoint-encoded.
    fn decode_payload_struct(
        &self,
        variant_name: &str,
        payload_val: BasicValueEnum<'ctx>,
        expected_ty: Option<BasicTypeEnum<'ctx>>,
    ) -> Result<(BasicValueEnum<'ctx>, BasicTypeEnum<'ctx>), CompileError> {
        let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());

        // Built-in Result/Option payloads are stored at their natural LLVM type
        // (e.g. `{ptr, i64}` for `Result<string, E>`), not as a ptrtoint-encoded
        // i64. Use the extracted value's type directly.
        // Built-in Err on Result<T, string> stores ptrtoint(heap_{ptr,len}_struct)
        // in the i64 error slot. Reconstruct the string struct if expected.
        let is_builtin_result_or_option = matches!(variant_name, "Ok" | "Err" | "Some")
            && self.find_variant_owner(variant_name).is_none();
        if is_builtin_result_or_option {
            if variant_name == "Err"
                && matches!(payload_val, BasicValueEnum::IntValue(_))
                && expected_ty
                    .as_ref()
                    .is_some_and(|t| matches!(t, BasicTypeEnum::StructType(_)))
            {
                let Some(ty) = expected_ty else {
                    return Err(
                        "decode_payload_struct: expected_ty is None for Err string payload".into(),
                    );
                };
                let ptr = self
                    .builder
                    .build_int_to_ptr(
                        payload_val.into_int_value(),
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        "err_str_ptr",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("err string inttoptr: {}", e)))?;
                let loaded = self
                    .builder
                    .build_load(ty, ptr, "err_str_struct")
                    .map_err(|e| CompileError::LlvmError(format!("err string load: {}", e)))?;
                return Ok((loaded, ty));
            }
            return Ok((payload_val, payload_val.get_type()));
        }

        let payload_info = self
            .find_variant_owner(variant_name)
            .and_then(|(owner, _)| {
                self.type_defs.get(&owner).and_then(|td| {
                    if let TypeDefKind::Enum(variants) = &td.kind {
                        variants
                            .iter()
                            .find(|v| v.name == *variant_name)
                            .and_then(|v| {
                                if let Some(VariantPayload::Tuple(types)) = &v.payload {
                                    if types.len() == 1 {
                                        self.llvm_type_for(&types[0]).map(|t| {
                                            (matches!(t, BasicTypeEnum::StructType(_)), Some(t))
                                        })
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            })
                    } else {
                        None
                    }
                })
            });
        if let Some((true, Some(data_ty))) = payload_info {
            // Struct-typed single payload: inttoptr then load the struct.
            let payload_int = payload_val.into_int_value();
            let ptr = self
                .builder
                .build_int_to_ptr(
                    payload_int,
                    self.context.ptr_type(inkwell::AddressSpace::default()),
                    "payload_ptr",
                )
                .map_err(|e| CompileError::LlvmError(format!("inttoptr: {}", e)))?;
            let loaded_struct = self
                .builder
                .build_load(data_ty, ptr, "payload_struct")
                .map_err(|e| CompileError::LlvmError(format!("load payload struct: {}", e)))?;
            Ok((loaded_struct, data_ty))
        } else if let Some((false, Some(natural_ty))) = payload_info {
            // P0-2: Single primitive payload (e.g. f64, i32). The constructor
            // stored the value (sign-extended for ints) into the i64 payload slot.
            // Recover the natural type:
            //   - i64: pass through (already correct)
            //   - i32 or narrower: truncate i64→iN (bitcast across widths is invalid)
            //   - f64: bitcast i64→f64 (same width, valid)
            if natural_ty == BasicTypeEnum::IntType(self.context.i64_type()) {
                Ok((payload_val, natural_ty))
            } else if let BasicTypeEnum::IntType(nat_int_ty) = natural_ty {
                // Truncate i64 payload back to the natural int width.
                let truncated = self
                    .builder
                    .build_int_truncate(
                        payload_val.into_int_value(),
                        nat_int_ty,
                        "payload_trunc_back",
                    )
                    .map_err(|e| CompileError::LlvmError(format!("trunc payload back: {}", e)))?;
                Ok((truncated.into(), natural_ty))
            } else {
                let decoded = self
                    .builder
                    .build_bit_cast(payload_val, natural_ty, "payload_bc_back")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast payload back: {}", e)))?;
                Ok((decoded, natural_ty))
            }
        } else {
            Ok((payload_val, i64_ty))
        }
    }

    fn compile_slice_pattern(
        &self,
        scrutinee: BasicValueEnum<'ctx>,
        inner_pats: &[Pattern],
        _rest: &Option<Box<Pattern>>,
    ) -> Result<Option<inkwell::values::IntValue<'ctx>>, CompileError> {
        // Same data access as array patterns, only comparing prefix elements
        self.compile_array_pattern(scrutinee, inner_pats)
    }

    pub(in crate::codegen) fn compile_match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        vars: &HashMap<String, VarEntry<'ctx>>,
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        let scrutinee_val = self.compile_expr(scrutinee, vars)?;
        // Check if the scrutinee is a string (which needs strcmp-based comparison).
        // Use type inference rather than extract_string_ptr because the latter
        // returns Some for any pointer value (including ADT pointers).
        let inferred_type = self.infer_object_type(scrutinee, vars);
        let is_string_scrutinee = inferred_type == "string";
        // Only integer/enum matches need a tag value. Tuple/array/slice/string matches
        // work directly on the scrutinee value.
        let needs_tag = if is_string_scrutinee {
            false
        } else {
            arms.iter()
                .any(|arm| matches!(arm.pat, Pattern::Constructor(_, _) | Pattern::Literal(_)))
        };
        let scrutinee_iv: Option<inkwell::values::IntValue<'ctx>> = match scrutinee_val {
            BasicValueEnum::IntValue(iv) => Some(iv),
            BasicValueEnum::StructValue(sv) => {
                if is_string_scrutinee {
                    None
                } else {
                    let tag = self
                        .builder
                        .build_extract_value(sv, 0, "enum_tag")
                        .map_err(|e| CompileError::LlvmError(format!("extract enum tag: {}", e)))?
                        .into_int_value();
                    Some(
                        self.builder
                            .build_int_z_extend(tag, self.context.i64_type(), "tag_ext")
                            .map_err(|e| CompileError::LlvmError(format!("extend tag: {}", e)))?,
                    )
                }
            }
            BasicValueEnum::PointerValue(pv) if needs_tag => {
                // Tag is always at index 0 as an i32 regardless of payload type.
                let i32_ty = BasicTypeEnum::IntType(self.context.i32_type());
                let i64_ty = BasicTypeEnum::IntType(self.context.i64_type());
                let enum_ty = self.context.struct_type(&[i32_ty, i64_ty], false);
                let tag_gep = self
                    .gep()
                    .build_struct_gep(BasicTypeEnum::StructType(enum_ty), pv, 0, "tag_gep")
                    .map_err(|e| CompileError::LlvmError(format!("tag gep: {}", e)))?;
                let tag = self
                    .build_load(
                        BasicTypeEnum::IntType(self.context.i32_type()),
                        tag_gep,
                        "tag_load",
                    )?
                    .into_int_value();
                Some(
                    self.builder
                        .build_int_z_extend(tag, self.context.i64_type(), "tag_ext")
                        .map_err(|e| CompileError::LlvmError(format!("extend tag: {}", e)))?,
                )
            }
            _ => None,
        };

        let function = self
            .current_function()
            .ok_or_else(|| "codegen: no current function for match".to_string())?;
        let merge_bb = self.context.append_basic_block(function, "matchcont");
        let mut else_bb = self.context.append_basic_block(function, "matchelse");

        // Branch from current block to the dispatch (matchelse)
        self.build_br(else_bb)?;
        self.builder.position_at_end(else_bb);

        let mut incoming_vals = Vec::new();
        let mut incoming_bbs = Vec::new();

        // Build if-else chain for each arm
        for (i, arm) in arms.iter().enumerate() {
            let (arm_bb, next_else_bb) =
                self.compile_match_arm_dispatch(i, arm, scrutinee_val, scrutinee_iv, else_bb)?;
            // Guard failure must continue to the next arm's dispatch block, so
            // update else_bb before compiling the arm body.
            else_bb = next_else_bb;
            let env = MatchArmEnv {
                scrutinee_val,
                scrutinee_iv,
                merge_bb,
                else_bb,
            };
            let (arm_val, body_bb) = self.compile_match_arm_body(i, arm, arm_bb, vars, &env)?;
            incoming_vals.push(arm_val);
            incoming_bbs.push(body_bb);
        }

        // Unreachable else block. Call mimi_match_panic (runtime trap) before
        // build_unreachable() so that if a non-exhaustive match is reached at
        // runtime, the program prints a diagnostic and aborts instead of UB.
        self.builder.position_at_end(else_bb);
        let match_panic_fn = self
            .module
            .get_function("mimi_match_panic")
            .ok_or("mimi_match_panic not declared")?;
        self.builder
            .build_call(match_panic_fn, &[], "match_panic")
            .map_err(|e| CompileError::LlvmError(format!("match_panic call: {}", e)))?;
        self.builder
            .build_unreachable()
            .map_err(|e| CompileError::LlvmError(format!("match else unreachable: {}", e)))?;

        // Merge block - use phi to select the right value
        self.build_match_phi(merge_bb, &incoming_vals, &incoming_bbs)
    }

    /// Compile a single match arm's dispatch block: create the arm block, build
    /// the conditional/unconditional branch from `else_bb` to it, and return the
    /// arm block plus the next dispatch block.
    fn compile_match_arm_dispatch(
        &mut self,
        arm_idx: usize,
        arm: &MatchArm,
        scrutinee_val: BasicValueEnum<'ctx>,
        scrutinee_iv: Option<IntValue<'ctx>>,
        else_bb: BasicBlock<'ctx>,
    ) -> Result<(BasicBlock<'ctx>, BasicBlock<'ctx>), CompileError> {
        let function = else_bb.get_parent().ok_or_else(|| {
            CompileError::LlvmError("match arm dispatch has no parent function".to_string())
        })?;
        let arm_bb = self
            .context
            .append_basic_block(function, &format!("arm{}", arm_idx));
        self.builder.position_at_end(else_bb);

        match &arm.pat {
            Pattern::Wildcard | Pattern::Variable(_) => {
                // If the variable name is actually an enum variant, treat it as a
                // unit constructor pattern and compare the tag.
                let is_variant = if let Pattern::Variable(name) = &arm.pat {
                    self.find_variant_ordinal(name).is_ok()
                } else {
                    false
                };
                if is_variant {
                    let scrutinee_iv = scrutinee_iv.ok_or_else(|| {
                        CompileError::LlvmError(
                            "constructor match arm requires an enum scrutinee".to_string(),
                        )
                    })?;
                    let ordinal = self
                        .find_variant_ordinal(if let Pattern::Variable(name) = &arm.pat {
                            name
                        } else {
                            ""
                        })
                        .map_err(|e| {
                            CompileError::LlvmError(format!("match arm variant lookup: {}", e))
                        })?;
                    let tag_val = self.context.i64_type().const_int(ordinal, false);
                    let cmp = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::EQ, scrutinee_iv, tag_val, "cmp")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                    let next_bb = self
                        .context
                        .append_basic_block(function, &format!("next{}", arm_idx));
                    self.build_cond_br(cmp, arm_bb, next_bb)?;
                    Ok((arm_bb, next_bb))
                } else {
                    // Always matches - jump to arm body
                    self.build_br(arm_bb)?;
                    // Create a fresh else_bb so the after-loop code doesn't
                    // double-terminate the block we just wrote to.
                    let wccont_bb = self
                        .context
                        .append_basic_block(function, &format!("wccont{}", arm_idx));
                    Ok((arm_bb, wccont_bb))
                }
            }
            Pattern::Literal(lit) => {
                // String literals need strcmp-based comparison instead of tag matching.
                if let Lit::String(s) = lit {
                    let scrutinee_ptr =
                        self.extract_string_ptr(&scrutinee_val).ok_or_else(|| {
                            CompileError::LlvmError(
                                "string match requires a string scrutinee".to_string(),
                            )
                        })?;
                    let global = self
                        .builder
                        .build_global_string_ptr(s, "match_str")
                        .map_err(|e| CompileError::LlvmError(format!("global string: {}", e)))?;
                    let lit_ptr = global.as_pointer_value();
                    let strcmp_fn = self.get_runtime_fn("strcmp")?;
                    let result = self
                        .build_call(
                            strcmp_fn,
                            &[scrutinee_ptr.into(), lit_ptr.into()],
                            "match_strcmp",
                        )?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| CompileError::LlvmError("strcmp returned void".to_string()))?
                        .into_int_value();
                    let zero = self.context.i32_type().const_int(0, false);
                    let eq = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::EQ, result, zero, "match_streq")
                        .map_err(|e| CompileError::LlvmError(format!("strcmp error: {}", e)))?;
                    let next_bb = self
                        .context
                        .append_basic_block(function, &format!("next{}", arm_idx));
                    self.build_cond_br(eq, arm_bb, next_bb)?;
                    Ok((arm_bb, next_bb))
                } else {
                    let scrutinee_iv = scrutinee_iv.ok_or_else(|| {
                        CompileError::LlvmError(
                            "literal match arm requires an integer or enum scrutinee".to_string(),
                        )
                    })?;
                    let lit_val = match lit {
                        // Match the scrutinee's integer width — i32 scrutinees need i32 constants.
                        Lit::Int(n) => {
                            let bw = scrutinee_iv.get_type().get_bit_width();
                            if bw < 64 {
                                self.context.i32_type().const_int(*n as u64, true)
                            } else {
                                self.context.i64_type().const_int(*n as u64, true)
                            }
                        }
                        Lit::Bool(b) => {
                            let b_val = self.context.bool_type().const_int(*b as u64, false);
                            // Match scrutinee width for bool comparison too.
                            let bw = scrutinee_iv.get_type().get_bit_width();
                            let target = if bw < 64 {
                                self.context.i32_type()
                            } else {
                                self.context.i64_type()
                            };
                            self.builder
                                .build_int_z_extend(b_val, target, "bool_ext")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("zext error: {}", e))
                                })?
                        }
                        Lit::Unit => {
                            let bw = scrutinee_iv.get_type().get_bit_width();
                            if bw < 64 {
                                self.context.i32_type().const_int(0, false)
                            } else {
                                self.context.i64_type().const_int(0, false)
                            }
                        }
                        _ => return Err("unsupported match literal type".into()),
                    };
                    let cmp = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::EQ, scrutinee_iv, lit_val, "cmp")
                        .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                    let next_bb = self
                        .context
                        .append_basic_block(function, &format!("next{}", arm_idx));
                    self.build_cond_br(cmp, arm_bb, next_bb)?;
                    Ok((arm_bb, next_bb))
                }
            }
            Pattern::Constructor(name, _) => {
                // Newtypes are transparent and have a single constructor, so
                // the arm always matches.
                if self
                    .type_defs
                    .get(name)
                    .is_some_and(|td| matches!(td.kind, crate::ast::TypeDefKind::Newtype(_)))
                {
                    self.build_br(arm_bb)?;
                    let next_bb = self
                        .context
                        .append_basic_block(function, &format!("next{}", arm_idx));
                    return Ok((arm_bb, next_bb));
                }
                // Constructor pattern: compare tag using ordinal index
                let scrutinee_iv = scrutinee_iv.ok_or_else(|| {
                    CompileError::LlvmError(
                        "constructor match arm requires an enum scrutinee".to_string(),
                    )
                })?;
                // Look up the variant ordinal index from type definitions
                let ordinal = self.find_variant_ordinal(name).map_err(|e| {
                    CompileError::LlvmError(format!("match arm variant lookup: {}", e))
                })?;
                let tag_val = self.context.i64_type().const_int(ordinal, false);
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, scrutinee_iv, tag_val, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                self.build_cond_br(cmp, arm_bb, next_bb)?;
                Ok((arm_bb, next_bb))
            }
            Pattern::Tuple(inner_pats) => {
                let match_cmp = self.compile_tuple_pattern(scrutinee_val, inner_pats)?;
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                match match_cmp {
                    Some(cmp) => self.build_cond_br(cmp, arm_bb, next_bb)?,
                    None => self.build_br(arm_bb)?,
                }
                Ok((arm_bb, next_bb))
            }
            Pattern::Array(inner_pats) => {
                let match_cmp = self.compile_array_pattern(scrutinee_val, inner_pats)?;
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                match match_cmp {
                    Some(cmp) => self.build_cond_br(cmp, arm_bb, next_bb)?,
                    None => self.build_br(arm_bb)?,
                }
                Ok((arm_bb, next_bb))
            }
            Pattern::Slice(inner_pats, rest) => {
                let match_cmp = self.compile_slice_pattern(scrutinee_val, inner_pats, rest)?;
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                match match_cmp {
                    Some(cmp) => self.build_cond_br(cmp, arm_bb, next_bb)?,
                    None => self.build_br(arm_bb)?,
                }
                Ok((arm_bb, next_bb))
            }
        }
    }

    /// Compile a single match arm body: bind pattern variables, evaluate the
    /// optional guard, and build a branch to the merge block. Returns the arm
    /// value and the block in which it was produced.
    fn compile_match_arm_body(
        &mut self,
        arm_idx: usize,
        arm: &MatchArm,
        arm_bb: BasicBlock<'ctx>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        env: &MatchArmEnv<'ctx>,
    ) -> Result<(BasicValueEnum<'ctx>, BasicBlock<'ctx>), CompileError> {
        let function = env.merge_bb.get_parent().ok_or_else(|| {
            CompileError::LlvmError("match arm body has no parent function".to_string())
        })?;
        self.builder.position_at_end(arm_bb);

        let local_vars =
            self.bind_pattern_variables(arm, env.scrutinee_val, env.scrutinee_iv, vars)?;
        match &arm.guard {
            Some(guard) => {
                let guard_val = self.compile_expr(guard, &local_vars)?;
                let guard_bool = match guard_val {
                    BasicValueEnum::IntValue(iv) => {
                        let zero = iv.get_type().const_int(0, false);
                        self.builder
                            .build_int_compare(inkwell::IntPredicate::NE, iv, zero, "guard_cmp")
                            .map_err(|e| CompileError::LlvmError(format!("guard cmp: {}", e)))?
                    }
                    BasicValueEnum::PointerValue(pv) => {
                        // Not-null means truthy (non-null pointers are valid values)
                        let is_null = self
                            .builder
                            .build_is_null(pv, "guard_null")
                            .map_err(|e| CompileError::LlvmError(format!("guard null: {}", e)))?;
                        let zero = self.context.bool_type().const_int(0, false);
                        self.builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                is_null,
                                zero,
                                "guard_notnull",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("guard notnull: {}", e)))?
                    }
                    _ => return Err("match guard must be boolean or pointer".into()),
                };
                let arm_body_bb = self
                    .context
                    .append_basic_block(function, &format!("arm_body{}", arm_idx));
                self.build_cond_br(guard_bool, arm_body_bb, env.else_bb)?;
                self.builder.position_at_end(arm_body_bb);
                let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                let guarded_body_bb = self.builder.get_insert_block().ok_or_else(|| {
                    CompileError::LlvmError("no insert block after guard arm body".to_string())
                })?;
                self.build_br(env.merge_bb)?;
                Ok((arm_val, guarded_body_bb))
            }
            None => {
                let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                let body_bb = self.builder.get_insert_block().ok_or_else(|| {
                    CompileError::LlvmError("no insert block after arm body".to_string())
                })?;
                self.build_br(env.merge_bb)?;
                Ok((arm_val, body_bb))
            }
        }
    }

    /// Build the final phi node in the merge block that selects the value
    /// produced by the matching arm. The else_bb calls mimi_match_panic
    /// before build_unreachable() so that a non-exhaustive match at runtime
    /// triggers a diagnostic + abort instead of UB. The else_bb is NOT a
    /// predecessor of merge_bb, so it contributes no phi entry.
    /// CG-C1: Fixed — mimi_match_panic traps instead of silent undef.
    fn build_match_phi(
        &self,
        merge_bb: BasicBlock<'ctx>,
        incoming_vals: &[BasicValueEnum<'ctx>],
        incoming_bbs: &[BasicBlock<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, CompileError> {
        if incoming_vals.is_empty() {
            self.builder.position_at_end(merge_bb);
            return Err("empty match expression".into());
        }
        // Unify integer widths: if some arms produce i32 and others i64,
        // s_extend all to the widest width so the phi node has a consistent type.
        // The caller (adjust_int_val) will truncate back if the function returns i32.
        let max_bw = incoming_vals
            .iter()
            .map(|v| match v {
                BasicValueEnum::IntValue(iv) => iv.get_type().get_bit_width(),
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        let needs_unify = incoming_vals
            .iter()
            .any(|v| matches!(v, BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() != max_bw));

        // For width unification, s_ext must be emitted in the PREDECESSOR block
        // (where the value is defined), NOT in the merge block — otherwise the
        // sext doesn't dominate all uses when another predecessor doesn't define it.
        let mut unified_vals: Vec<BasicValueEnum<'ctx>> = if needs_unify && max_bw > 0 {
            let target_ty = if max_bw <= 32 {
                self.context.i32_type()
            } else {
                self.context.i64_type()
            };
            incoming_vals
                .iter()
                .zip(incoming_bbs.iter())
                .map(|(v, pred_bb)| match v {
                    BasicValueEnum::IntValue(iv) => {
                        if iv.get_type().get_bit_width() < max_bw {
                            // Position in predecessor block and insert s_ext BEFORE
                            // the terminator (br). Otherwise the instruction goes
                            // after the terminator which is invalid IR.
                            self.builder.position_at_end(*pred_bb);
                            let term = pred_bb.get_terminator();
                            if let Some(term) = term {
                                self.builder.position_before(&term);
                            }
                            let extended = self
                                .builder
                                .build_int_s_extend(*iv, target_ty, "phi_sext")
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("phi s_ext: {}", e))
                                })?;
                            Ok(BasicValueEnum::IntValue(extended))
                        } else {
                            Ok(*v)
                        }
                    }
                    _ => Ok(*v),
                })
                .collect::<Result<_, CompileError>>()?
        } else {
            incoming_vals.to_vec()
        };

        // String-return arms: one arm may yield Mimi `{ptr,len}` while another
        // yields a raw string-literal `i8*`. Phi of mixed types → LLVM
        // "Cannot emit physreg copy instruction". Prefer the string struct;
        // wrap raw pointers in the predecessor block.
        let is_mimi_string_struct = |v: BasicValueEnum<'ctx>| -> bool {
            if let BasicValueEnum::StructValue(sv) = v {
                let fields = sv.get_type().get_field_types();
                fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(
                        fields[1],
                        BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                    )
            } else {
                false
            }
        };
        let has_string_struct = unified_vals.iter().copied().any(is_mimi_string_struct);
        let has_raw_ptr = unified_vals
            .iter()
            .any(|v| matches!(v, BasicValueEnum::PointerValue(_)));
        if has_string_struct && has_raw_ptr {
            let wrap_idxs: Vec<(usize, PointerValue<'ctx>, BasicBlock<'ctx>)> = unified_vals
                .iter()
                .enumerate()
                .filter_map(|(i, v)| match v {
                    BasicValueEnum::PointerValue(pv) => Some((i, *pv, incoming_bbs[i])),
                    _ => None,
                })
                .collect();
            for (i, pv, pred_bb) in wrap_idxs {
                self.builder.position_at_end(pred_bb);
                if let Some(term) = pred_bb.get_terminator() {
                    self.builder.position_before(&term);
                }
                unified_vals[i] = self.wrap_raw_string_ptr(pv)?;
            }
        }

        // Prefer a StructValue arm as the phi type when present (e.g. after
        // string wrap, or when int-width unify already left mixed kinds).
        let ty = unified_vals
            .iter()
            .find_map(|v| match v {
                BasicValueEnum::StructValue(sv) => Some(BasicTypeEnum::StructType(sv.get_type())),
                _ => None,
            })
            .unwrap_or_else(|| unified_vals[0].get_type());

        // Last-chance: coerce any still-mismatched predecessor to the phi type
        // so we never emit a type-mismatched phi.
        let mismatch_idxs: Vec<(usize, BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = unified_vals
            .iter()
            .enumerate()
            .filter(|(_, v)| v.get_type() != ty)
            .map(|(i, v)| (i, *v, incoming_bbs[i]))
            .collect();
        for (i, v, pred_bb) in mismatch_idxs {
            self.builder.position_at_end(pred_bb);
            if let Some(term) = pred_bb.get_terminator() {
                self.builder.position_before(&term);
            }
            if let (BasicValueEnum::PointerValue(pv), BasicTypeEnum::StructType(st)) = (v, ty) {
                let fields = st.get_field_types();
                let is_string = fields.len() == 2
                    && matches!(fields[0], BasicTypeEnum::PointerType(_))
                    && matches!(
                        fields[1],
                        BasicTypeEnum::IntType(it) if it.get_bit_width() == 64
                    );
                if is_string {
                    unified_vals[i] = self.wrap_raw_string_ptr(pv)?;
                    continue;
                }
            }
            unified_vals[i] = self.const_zero_for_type(ty);
        }

        // Now build the phi in the merge block.
        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(ty, "match.result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        let phi_incoming: Vec<_> = unified_vals
            .iter()
            .zip(incoming_bbs.iter())
            .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
            .collect();
        phi.add_incoming(&phi_incoming);
        Ok(phi.as_basic_value())
    }
}
