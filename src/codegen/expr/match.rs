use crate::ast::*;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;
use std::collections::HashMap;

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
                        BasicValueEnum::IntValue(iv) => (iv.into(), BasicTypeEnum::IntType(iv.get_type())),
                        _ => {
                            // For non-integer scrutinees bind the value directly.
                            let ty = scrutinee_val.get_type();
                            (scrutinee_val, ty)
                        }
                    }
                };
                let alloca = self.builder.build_alloca(ty, name)
                    .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                self.builder.build_store(alloca, val)
                    .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                local_vars.insert(name.clone(), (alloca, ty));
            }
            Pattern::Constructor(_, inner_patterns) => {
                // For constructor patterns, bind inner variables from the payload field.
                // All enum-like representations in this codegen put the tag at index 0
                // and the first payload field at index 1.
                let (payload, payload_ty) = match scrutinee_val {
                    BasicValueEnum::StructValue(sv) => {
                        let payload = self.builder.build_extract_value(sv, 1, "payload")
                            .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;
                        let ty = sv.get_type()
                            .get_field_type_at_index(1)
                            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                        (payload, ty)
                    }
                    BasicValueEnum::PointerValue(pv) => {
                        // Enum values stored as pointers use the common {i32 tag, i64 payload}
                        // layout produced by register_type_def.
                        let enum_ty = self.context.struct_type(&[
                            BasicTypeEnum::IntType(self.context.i32_type()),
                            BasicTypeEnum::IntType(self.context.i64_type()),
                        ], false);
                        let loaded = self.builder.build_load(enum_ty, pv, "enum_loaded")
                            .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?;
                        let sv = match loaded {
                            BasicValueEnum::StructValue(sv) => sv,
                            _ => return Err("constructor pattern: expected struct from pointer".into()),
                        };
                        let payload = self.builder.build_extract_value(sv, 1, "payload")
                            .map_err(|e| CompileError::LlvmError(format!("extract payload: {}", e)))?;
                        (payload, BasicTypeEnum::IntType(self.context.i64_type()))
                    }
                    BasicValueEnum::IntValue(iv) => {
                        // Legacy/compact representation: some enum values are passed as a
                        // single integer (e.g. nested enum payloads). Bind the payload to the
                        // integer itself so that nested pattern matches still compile.
                        (iv.into(), BasicTypeEnum::IntType(iv.get_type()))
                    }
                    _ => return Err("constructor pattern requires enum struct value".into()),
                };
                for inner_pat in inner_patterns {
                    if let Pattern::Variable(name) = inner_pat {
                        let alloca = self.builder.build_alloca(payload_ty, name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        self.builder.build_store(alloca, payload)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        local_vars.insert(name.clone(), (alloca, payload_ty));
                    }
                }
            }
            Pattern::Tuple(inner_pats) => {
                // For tuple patterns, bind inner variables by loading from struct.
                // Use the tuple type stack to obtain the real element types.
                let struct_ty = *self.tuple_type_stack.last()
                    .ok_or_else(|| CompileError::LlvmError("tuple_type_stack empty for tuple pattern bind".to_string()))?;
                let struct_ty_enum = BasicTypeEnum::StructType(struct_ty);
                let scrutinee_ptr = match scrutinee_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    BasicValueEnum::StructValue(sv) => {
                        let alloca = self.builder.build_alloca(struct_ty, "tuple_alloca")
                            .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                        self.builder.build_store(alloca, sv)
                            .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                        alloca
                    }
                    _ => return Ok(local_vars),
                };
                for (j, inner_pat) in inner_pats.iter().enumerate() {
                    if let Pattern::Variable(name) = inner_pat {
                        let elem_ty = struct_ty.get_field_type_at_index(j as u32)
                            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
                        let gep = self.builder.build_struct_gep(
                            struct_ty_enum, scrutinee_ptr, j as u32,
                            &format!("tuple_{}", j),
                        ).map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        let val = self.builder.build_load(
                            elem_ty, gep, &format!("tup_{}", j),
                        ).map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        let alloca = self.builder.build_alloca(elem_ty, name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        self.builder.build_store(alloca, val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        local_vars.insert(name.clone(), (alloca, elem_ty));
                    }
                }
            }
            Pattern::Array(inner_pats) => {
                // For array patterns, bind inner variables by loading from list data
                let scrutinee_ptr = match scrutinee_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Ok(local_vars),
                };
                // Load data pointer from list struct
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let data_gep = self.builder.build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data").map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let i64_ty = self.context.i64_type();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?.into_pointer_value();
                for (j, inner_pat) in inner_pats.iter().enumerate() {
                    if let Pattern::Variable(name) = inner_pat {
                        let idx = i64_ty.const_int(j as u64, false);
                        // SAFETY: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                        let elem_ptr = unsafe {
                            self.builder.build_gep(i64_ty, data_ptr, &[idx], &format!("arr_{}", j))
                        }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        let val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, &format!("arrv_{}", j))
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        self.builder.build_store(alloca, val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                    }
                }
            }
            Pattern::Slice(inner_pats, rest) => {
                // For slice patterns, bind prefix variables and rest as list
                let scrutinee_ptr = match scrutinee_val {
                    BasicValueEnum::PointerValue(pv) => pv,
                    _ => return Ok(local_vars),
                };
                let list_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i64_type()),
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                ], false);
                let data_gep = self.builder.build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
                    .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                let data_i8 = self.builder.build_load(
                    BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
                    data_gep, "data").map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?.into_pointer_value();
                let i64_ty = self.context.i64_type();
                let data_ptr = self.builder.build_bit_cast(data_i8,
                    i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
                    .map_err(|e| CompileError::LlvmError(format!("bitcast error: {}", e)))?.into_pointer_value();
                // Bind prefix elements
                for (j, inner_pat) in inner_pats.iter().enumerate() {
                    if let Pattern::Variable(name) = inner_pat {
                        let idx = i64_ty.const_int(j as u64, false);
                        // SAFETY: build_gep requires valid pointer and index types; the pointer is derived from a valid LLVM-typed allocation and indices are correctly-typed i64 values.
                        let elem_ptr = unsafe {
                            self.builder.build_gep(i64_ty, data_ptr, &[idx], &format!("slc_{}", j))
                        }.map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
                        let val = self.builder.build_load(BasicTypeEnum::IntType(i64_ty), elem_ptr, &format!("slcv_{}", j))
                            .map_err(|e| CompileError::LlvmError(format!("load error: {}", e)))?;
                        let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        self.builder.build_store(alloca, val)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                    }
                }
                // Bind rest as remaining list (simplified: bind as empty list)
                if let Some(rest_pat) = rest.as_ref() {
                    if let Pattern::Variable(name) = rest_pat.as_ref() {
                        let i64_ty = self.context.i64_type();
                        let empty_list: BasicValueEnum = i64_ty.const_int(0, false).into();
                        let alloca = self.builder.build_alloca(BasicTypeEnum::IntType(i64_ty), name)
                            .map_err(|e| CompileError::LlvmError(format!("alloca error: {}", e)))?;
                        self.builder.build_store(alloca, empty_list)
                            .map_err(|e| CompileError::LlvmError(format!("store error: {}", e)))?;
                        local_vars.insert(name.clone(), (alloca, BasicTypeEnum::IntType(i64_ty)));
                    }
                }
            }
            Pattern::Wildcard | Pattern::Literal(_) => {
                // Wildcard and literal patterns: no variable binding needed
            }
        }
        Ok(local_vars)
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
                let struct_ty = *self.tuple_type_stack.last()
                    .ok_or_else(|| CompileError::LlvmError("tuple_type_stack empty for tuple pattern".to_string()))?;
                let loaded = self.builder.build_load(struct_ty, pv, "tuple_pat_loaded")
                    .map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?;
                let sv = match loaded {
                    BasicValueEnum::StructValue(sv) => sv,
                    _ => return Err("tuple pattern: expected struct from pointer".into()),
                };
                let alloca = self.builder.build_alloca(struct_ty, "tuple_alloca")
                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                self.builder.build_store(alloca, sv)
                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
                (alloca, struct_ty)
            }
            BasicValueEnum::StructValue(sv) => {
                let struct_ty = sv.get_type();
                let alloca = self.builder.build_alloca(struct_ty, "tuple_alloca")
                    .map_err(|e| CompileError::LlvmError(format!("alloca: {}", e)))?;
                self.builder.build_store(alloca, sv)
                    .map_err(|e| CompileError::LlvmError(format!("store: {}", e)))?;
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
                let elem_ptr = self.builder.build_struct_gep(
                    struct_ty_enum, tuple_ptr, j as u32,
                    &format!("tup_el{}", j),
                ).map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                let elem_ty = struct_ty.get_field_type_at_index(j as u32)
                    .unwrap_or(BasicTypeEnum::IntType(i64_ty));
                let elem_val = self.builder.build_load(
                    elem_ty, elem_ptr, &format!("tup_v{}", j),
                ).map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                    .into_int_value();
                let eq = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ, elem_val, expected, &format!("tup_cmp{}", j),
                ).map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                agg = Some(match agg {
                    Some(prev) => self.builder.build_and(prev, eq, "tup_and")
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
        let list_ty = self.context.struct_type(&[
            BasicTypeEnum::IntType(i64_ty),
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
        ], false);
        let data_gep = self.builder.build_struct_gep(list_ty, scrutinee_ptr, 1, "list_data")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        let data_i8 = self.builder.build_load(
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            data_gep, "data",
        ).map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?.into_pointer_value();
        let data_ptr = self.builder.build_bit_cast(data_i8,
            i64_ty.ptr_type(inkwell::AddressSpace::default()), "data_i64")
            .map_err(|e| CompileError::LlvmError(format!("bitcast: {}", e)))?.into_pointer_value();
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
                let elem_ptr = unsafe {
                    self.builder.build_gep(i64_ty, data_ptr, &[idx], &format!("arr_el{}", j))
                }.map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                let elem_val = self.builder.build_load(
                    BasicTypeEnum::IntType(i64_ty), elem_ptr, &format!("arr_v{}", j),
                ).map_err(|e| CompileError::LlvmError(format!("load: {}", e)))?
                    .into_int_value();
                let eq = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ, elem_val, expected, &format!("arr_cmp{}", j),
                ).map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                agg = Some(match agg {
                    Some(prev) => self.builder.build_and(prev, eq, "arr_and")
                        .map_err(|e| CompileError::LlvmError(format!("and: {}", e)))?,
                    None => eq,
                });
            }
        }
        Ok(agg)
    }

    /// Generate element-wise comparison for a slice pattern.
    /// Returns `Some(i1)` if any element requires comparison, `None` for wildcard-only patterns.
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
        // Only integer/enum matches need a tag value. Tuple/array/slice matches
        // work directly on the scrutinee value, so avoid extracting a tag from
        // a pointer that may actually point to a tuple/list struct.
        let needs_tag = arms.iter().any(|arm| {
            matches!(arm.pat, Pattern::Constructor(_, _) | Pattern::Literal(_))
        });
        let scrutinee_iv: Option<inkwell::values::IntValue<'ctx>> = match scrutinee_val {
            BasicValueEnum::IntValue(iv) => Some(iv),
            BasicValueEnum::StructValue(sv) => {
                let tag = self.builder.build_extract_value(sv, 0, "enum_tag")
                    .map_err(|e| CompileError::LlvmError(format!("extract enum tag: {}", e)))?
                    .into_int_value();
                Some(self.builder.build_int_cast(tag, self.context.i64_type(), "tag_ext")
                    .map_err(|e| CompileError::LlvmError(format!("extend tag: {}", e)))?)
            }
            BasicValueEnum::PointerValue(pv) if needs_tag => {
                // Enum pointers use the common {i32 tag, i64 payload} layout.
                let enum_ty = self.context.struct_type(&[
                    BasicTypeEnum::IntType(self.context.i32_type()),
                    BasicTypeEnum::IntType(self.context.i64_type()),
                ], false);
                let tag_gep = self.builder.build_struct_gep(
                    BasicTypeEnum::StructType(enum_ty), pv, 0, "tag_gep",
                ).map_err(|e| CompileError::LlvmError(format!("tag gep: {}", e)))?;
                let tag = self.builder.build_load(
                    BasicTypeEnum::IntType(self.context.i32_type()), tag_gep, "tag_load",
                ).map_err(|e| CompileError::LlvmError(format!("tag load: {}", e)))?
                    .into_int_value();
                Some(self.builder.build_int_cast(tag, self.context.i64_type(), "tag_ext")
                    .map_err(|e| CompileError::LlvmError(format!("extend tag: {}", e)))?)
            }
            _ => None,
        };

        let function = self.current_function().ok_or_else(|| "codegen: no current function for match".to_string())?;
        let merge_bb = self.context.append_basic_block(function, "matchcont");
        let mut else_bb = self.context.append_basic_block(function, "matchelse");

        // Branch from current block to the dispatch (matchelse)
        self.builder.build_unconditional_branch(else_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
        self.builder.position_at_end(else_bb);

        let mut incoming_vals = Vec::new();
        let mut incoming_bbs = Vec::new();

        // Build if-else chain for each arm
        for (i, arm) in arms.iter().enumerate() {
            let arm_bb = self.context.append_basic_block(function, &format!("arm{}", i));

            match &arm.pat {
                Pattern::Wildcard | Pattern::Variable(_) => {
                    self.builder.position_at_end(else_bb);
                    // If the variable name is actually an enum variant, treat it as a
                    // unit constructor pattern and compare the tag.
                    let is_variant = if let Pattern::Variable(name) = &arm.pat {
                        self.find_variant_ordinal(name).is_ok()
                    } else {
                        false
                    };
                    if is_variant {
                        let scrutinee_iv = scrutinee_iv.ok_or_else(|| CompileError::LlvmError(
                            "constructor match arm requires an enum scrutinee".to_string()))?;
                        let ordinal = self.find_variant_ordinal(
                            if let Pattern::Variable(name) = &arm.pat { name } else { "" }
                        ).map_err(|e| CompileError::LlvmError(format!("match arm variant lookup: {}", e)))?;
                        let tag_val = self.context.i64_type().const_int(ordinal, false);
                        let cmp = self.builder.build_int_compare(
                            inkwell::IntPredicate::EQ,
                            scrutinee_iv,
                            tag_val,
                            "cmp",
                        ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                        let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                        self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        else_bb = next_bb;
                    } else {
                        // Always matches - jump to arm body
                        self.builder.build_unconditional_branch(arm_bb)
                            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        // Create a fresh else_bb so the after-loop code doesn't
                        // double-terminate the block we just wrote to.
                        else_bb = self.context.append_basic_block(function, &format!("wccont{}", i));
                    }
                }
                Pattern::Literal(lit) => {
                    self.builder.position_at_end(else_bb);
                    let scrutinee_iv = scrutinee_iv.ok_or_else(|| CompileError::LlvmError(
                        "literal match arm requires an integer or enum scrutinee".to_string()))?;
                    let lit_val = match lit {
                        Lit::Int(n) => self.context.i64_type().const_int(*n as u64, true),
                        Lit::Bool(b) => self.context.bool_type().const_int(*b as u64, false),
                        Lit::Unit => self.context.i64_type().const_int(0, false),
                        _ => return Err("unsupported match literal type".into()),
                    };
                    let cmp = self.builder.build_int_compare(
                        inkwell::IntPredicate::EQ,
                        scrutinee_iv,
                        lit_val,
                        "cmp",
                    ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                    // Always create an intermediate next block so the else chain
                    // never points directly at merge_bb.  This keeps the phi's
                    // predecessor set clean and avoids corrupting merge_bb.
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    else_bb = next_bb;
                }
                Pattern::Constructor(name, _) => {
                    // Constructor pattern: compare tag using ordinal index
                    self.builder.position_at_end(else_bb);
                    let scrutinee_iv = scrutinee_iv.ok_or_else(|| CompileError::LlvmError(
                        "constructor match arm requires an enum scrutinee".to_string()))?;
                    // Look up the variant ordinal index from type definitions
                    let ordinal = self.find_variant_ordinal(name)
                        .map_err(|e| CompileError::LlvmError(format!("match arm variant lookup: {}", e)))?;
                    let tag_val = self.context.i64_type().const_int(ordinal, false);
                    let cmp = self.builder.build_int_compare(
                        inkwell::IntPredicate::EQ,
                        scrutinee_iv,
                        tag_val,
                        "cmp",
                    ).map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                    else_bb = next_bb;
                }
                Pattern::Tuple(inner_pats) => {
                    self.builder.position_at_end(else_bb);
                    let match_cmp = self.compile_tuple_pattern(scrutinee_val, inner_pats)?;
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    match match_cmp {
                        Some(cmp) => {
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                        None => {
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                    }
                    else_bb = next_bb;
                }
                Pattern::Array(inner_pats) => {
                    self.builder.position_at_end(else_bb);
                    let match_cmp = self.compile_array_pattern(scrutinee_val, inner_pats)?;
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    match match_cmp {
                        Some(cmp) => {
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                        None => {
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                    }
                    else_bb = next_bb;
                }
                Pattern::Slice(inner_pats, rest) => {
                    self.builder.position_at_end(else_bb);
                    let match_cmp = self.compile_slice_pattern(scrutinee_val, inner_pats, rest)?;
                    let next_bb = self.context.append_basic_block(function, &format!("next{}", i));
                    match match_cmp {
                        Some(cmp) => {
                            self.builder.build_conditional_branch(cmp, arm_bb, next_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                        None => {
                            self.builder.build_unconditional_branch(arm_bb)
                                .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                        }
                    }
                    else_bb = next_bb;
                }
            }

            // Arm body — bind pattern variables
            self.builder.position_at_end(arm_bb);
            let local_vars = self.bind_pattern_variables(arm, scrutinee_val, scrutinee_iv, vars)?;
            match &arm.guard {
                Some(guard) => {
                    let guard_val = self.compile_expr(guard, &local_vars)?;
                    let guard_bool = match guard_val {
                        BasicValueEnum::IntValue(iv) => {
                            let zero = iv.get_type().const_int(0, false);
                            self.builder.build_int_compare(
                                inkwell::IntPredicate::NE,
                                iv,
                                zero,
                                "guard_cmp",
                            ).map_err(|e| CompileError::LlvmError(format!("guard cmp: {}", e)))?
                        }
                        BasicValueEnum::PointerValue(pv) => {
                            // Not-null means truthy (non-null pointers are valid values)
                            let is_null = self.builder.build_is_null(pv, "guard_null")
                                .map_err(|e| CompileError::LlvmError(format!("guard null: {}", e)))?;
                            let zero = self.context.bool_type().const_int(0, false);
                            self.builder.build_int_compare(
                                inkwell::IntPredicate::EQ, is_null, zero, "guard_notnull",
                            ).map_err(|e| CompileError::LlvmError(format!("guard notnull: {}", e)))?
                        }
                        _ => return Err("match guard must be boolean or pointer".into()),
                    };
                    let arm_body_bb = self.context.append_basic_block(function, &format!("arm_body{}", i));
                    self.builder.build_conditional_branch(guard_bool, arm_body_bb, else_bb)
                        .map_err(|e| CompileError::LlvmError(format!("guard branch: {}", e)))?;
                    self.builder.position_at_end(arm_body_bb);
                    let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                    incoming_vals.push(arm_val);
                    incoming_bbs.push(arm_body_bb);
                    self.builder.build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                }
                None => {
                    let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                    incoming_vals.push(arm_val);
                    incoming_bbs.push(arm_bb);
                    self.builder.build_unconditional_branch(merge_bb)
                        .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;
                }
            }
        }

        // Unreachable else block (should not be reached if match is exhaustive).
        // else_bb is a fresh next_N block (never merge_bb) thanks to the
        // unconditional intermediate-block creation above.
        self.builder.position_at_end(else_bb);
        self.builder.build_unconditional_branch(merge_bb)
            .map_err(|e| CompileError::LlvmError(format!("branch error: {}", e)))?;

        // Merge block - use phi to select the right value
        self.builder.position_at_end(merge_bb);
        if incoming_vals.is_empty() {
            return Err("empty match expression".into());
        }
        let ty = incoming_vals[0].get_type();
        let phi = self.builder.build_phi(ty, "match.result")
            .map_err(|e| CompileError::LlvmError(format!("phi error: {}", e)))?;
        let mut phi_incoming: Vec<_> = incoming_vals.iter().zip(incoming_bbs.iter())
            .map(|(v, bb)| (v as &dyn inkwell::values::BasicValue, *bb))
            .collect();
        // Add the unreachable else block with a dummy value so every
        // predecessor of merge_bb has a phi entry.
        let dummy_val = self.context.i64_type().const_int(0, false);
        phi_incoming.push((&dummy_val as &dyn inkwell::values::BasicValue, else_bb));
        phi.add_incoming(&phi_incoming);
        Ok(phi.as_basic_value())
    }

}
