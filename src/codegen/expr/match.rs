use crate::ast::*;
use crate::codegen::CallSiteValueExt;
use crate::codegen::{CodeGenerator, VarEntry};
use crate::error::CompileError;

use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};
use std::collections::HashMap;

/// Immutable context shared between match dispatch and body compilation.
struct MatchArmEnv<'ctx> {
    scrutinee_val: BasicValueEnum<'ctx>,
    scrutinee_iv: Option<IntValue<'ctx>>,
    merge_bb: BasicBlock<'ctx>,
    else_bb: BasicBlock<'ctx>,
    /// The Mimi AST type of the scrutinee expression (if inferable),
    /// used to recover struct-typed variables from ptrtoint-encoded i64
    /// payloads in pattern matching (e.g., `Err((src, e))` where src is
    /// a flow state stored as a heap pointer).
    scrutinee_type: Option<crate::ast::Type>,
}

impl<'ctx> CodeGenerator<'ctx> {
    pub(in crate::codegen) fn bind_pattern_variables(
        &mut self,
        arm: &MatchArm,
        scrutinee_val: BasicValueEnum<'ctx>,
        scrutinee_iv: Option<inkwell::values::IntValue<'ctx>>,
        vars: &HashMap<String, VarEntry<'ctx>>,
        scrutinee_type: Option<&crate::ast::Type>,
    ) -> Result<HashMap<String, VarEntry<'ctx>>, CompileError> {
        let mut local_vars = vars.clone();
        // Bind variables from pattern
        match &arm.pat.kind {
            PatternKind::Variable(name) => {
                // Uppercase identifiers that name enum variants are treated as
                // unit constructor patterns, not variable bindings.
                if self
                    .find_variant_ordinal_scoped(name, scrutinee_type)
                    .is_ok()
                {
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
            PatternKind::Constructor(name, inner_patterns) => {
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
                // Q1 (rc-quality-gate-0.34.25a): built-in Result<T, string>
                // stores ptrtoint(heap_{ptr,len}) in the i64 error slot.
                // decode_payload_struct reconstructs the string struct when it
                // knows the expected type — but both call sites below passed
                // None, so the raw heap-pointer i64 leaked into the bound
                // variable (garbage display; the VM prints the string — L1
                // divergence). Derive the expected type from the scrutinee's
                // Result<T, E> AST type. Deliberately restricted to string
                // errors: Result<T, (Source, E)> rejected tuples have their
                // own hard-coded {i64,i64} reconstruction below, and other
                // shapes are unverified.
                let err_expected_ty: Option<(crate::ast::Type, BasicTypeEnum<'ctx>)> =
                    if payload_idx == 2 && variant_owner.is_none() {
                        scrutinee_type.and_then(|st| {
                            let err_ty: Option<&crate::ast::Type> = match st.unlocated() {
                                crate::ast::Type::Result(_, err) => Some(err.as_ref()),
                                // AST surface form: Result<T, E> parses as
                                // Name("Result", [T, E]) in legacy paths.
                                crate::ast::Type::Name(n, args)
                                    if n == "Result" && args.len() == 2 =>
                                {
                                    Some(&args[1])
                                }
                                _ => None,
                            };
                            err_ty
                                .filter(|t| crate::core::helpers::is_string(t))
                                .and_then(|t| self.llvm_type_for(t).map(|llvm| (t.clone(), llvm)))
                        })
                    } else {
                        None
                    };
                let (payload, payload_ty) = match scrutinee_val {
                    BasicValueEnum::StructValue(sv) => {
                        let payload_val = self
                            .builder
                            .build_extract_value(sv, payload_idx, "payload")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("extract payload: {}", e))
                            })?;
                        // Check if the variant's payload is a struct type (ptrtoint encoded)
                        let (decoded, ty) = self.decode_payload_struct(
                            name,
                            payload_val,
                            err_expected_ty.as_ref().map(|(_, t)| *t),
                        )?;
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
                        let (decoded, ty) = self.decode_payload_struct(
                            name,
                            payload_val,
                            err_expected_ty.as_ref().map(|(_, t)| *t),
                        )?;
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
                        if let PatternKind::Variable(pname) = &inner_pat.kind {
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
                        match &inner_pat.kind {
                            PatternKind::Variable(bind_name) => {
                                // v0.34.16 (ADR-002): Record-style constructor
                                // fields (`B { message }`) name the field —
                                // extract it from the decoded payload struct
                                // instead of binding the whole payload. Tuple
                                // fields (`Ok(x)` → `_0`) bind the payload
                                // itself (existing behavior).
                                let is_named_field = !bind_name.starts_with('_');
                                if is_named_field {
                                    if let BasicValueEnum::StructValue(sv) = payload {
                                        let payload_ptr =
                                            self.build_alloca(payload_ty, "named_field_alloca")?;
                                        self.build_store(payload_ptr, sv)?;
                                        // Full field defs (name + type) from the
                                        // owning Record type definition. We need
                                        // both the index (declaration order) AND
                                        // the field's AST type so downstream field
                                        // access (`bind_name.subfield`) resolves —
                                        // v0.34.18b fixes E0707 on match-bound
                                        // record fields (e.g. `Fault { trace }` →
                                        // `trace.last_state_name`).
                                        let record_fields: Option<Vec<crate::ast::Field>> =
                                            variant_owner.as_ref().and_then(|(owner, _)| {
                                                self.type_defs.get(owner).and_then(|td| {
                                                    if let TypeDefKind::Enum(variants) = &td.kind {
                                                        variants
                                                            .iter()
                                                            .find(|v| v.name == *name)
                                                            .and_then(|v| match &v.payload {
                                                                Some(VariantPayload::Tuple(
                                                                    types,
                                                                )) if types.len() == 1 => self
                                                                    .record_field_defs_of(
                                                                        &types[0],
                                                                    ),
                                                                _ => None,
                                                            })
                                                    } else {
                                                        None
                                                    }
                                                })
                                            });
                                        let field_info =
                                            record_fields.as_ref().and_then(|fields| {
                                                fields
                                                    .iter()
                                                    .position(|f| f.name == *bind_name)
                                                    .map(|i| (i as u32, fields[i].ty.clone()))
                                            });
                                        if let Some((idx, field_ast_ty)) = field_info {
                                            let elem_ty = match payload_ty {
                                                BasicTypeEnum::StructType(st) => st
                                                    .get_field_type_at_index(idx)
                                                    .unwrap_or(BasicTypeEnum::IntType(
                                                        self.context.i64_type(),
                                                    )),
                                                _ => {
                                                    BasicTypeEnum::IntType(self.context.i64_type())
                                                }
                                            };
                                            let gep = self
                                                .gep()
                                                .build_struct_gep(
                                                    payload_ty,
                                                    payload_ptr,
                                                    idx,
                                                    bind_name,
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "named field gep: {e}"
                                                    ))
                                                })?;
                                            let val = self.build_load(
                                                elem_ty,
                                                gep,
                                                &format!("field_{bind_name}"),
                                            )?;
                                            self.bind_pattern_var(
                                                &mut local_vars,
                                                bind_name,
                                                val,
                                                elem_ty,
                                            )?;
                                            // Register the field's AST type so
                                            // `bind_name.subfield` field access can
                                            // resolve the record type name.
                                            self.var_types.insert(
                                                bind_name.to_string(),
                                                field_ast_ty.clone(),
                                            );
                                            if let Some(full) =
                                                self.get_full_type_name(&field_ast_ty)
                                            {
                                                self.var_type_names
                                                    .insert(bind_name.to_string(), full);
                                            }
                                            self.register_list_elem_type(bind_name, &field_ast_ty);
                                            continue;
                                        }
                                    }
                                }
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
                                } else if let Some((ref err_ast, _)) = err_expected_ty {
                                    // Q1: built-in Err string payload — register
                                    // the scrutinee's error AST type (string) for
                                    // the bound variable so println/field access
                                    // treat it as a string, matching the VM.
                                    self.var_types.insert(bind_name.clone(), err_ast.clone());
                                    if let Some(full) = self.get_full_type_name(err_ast) {
                                        self.var_type_names.insert(bind_name.clone(), full);
                                    }
                                } else {
                                    // Built-in constructor (Ok/Err/Some/None) from
                                    // Result/Option. The AST type is not in type_defs,
                                    // so payload_ast is None. Look up the type by
                                    // matching the payload's LLVM type against
                                    // registered type_llvm entries.
                                    let payload_type_name =
                                        self.find_type_name_by_llvm_type(payload_ty);
                                    if let Some(ref ty_name) = payload_type_name {
                                        self.var_type_names
                                            .insert(bind_name.clone(), ty_name.clone());
                                    }
                                }
                            }
                            _ => {
                                // Non-variable pattern (e.g. Tuple, Constructor).
                                // Recursively bind inner variables from the payload.
                                //
                                // For built-in Err with i64 (ptrtoint) payload,
                                // the heap-allocated struct is always {i64, i64}
                                // (source and error, both ptrtoint-encoded by
                                // compile_try_rejected). Decode before recursing
                                // so tuple/constructor inner patterns see a
                                // StructValue instead of a raw i64.
                                let recurse_payload = if name == "Err"
                                    && variant_owner.is_none()
                                    && matches!(payload, BasicValueEnum::IntValue(_))
                                {
                                    let i64_ty = self.context.i64_type();
                                    let tuple_llvm_ty = self.context.struct_type(
                                        &[
                                            BasicTypeEnum::IntType(i64_ty),
                                            BasicTypeEnum::IntType(i64_ty),
                                        ],
                                        false,
                                    );
                                    let ptr = self
                                        .builder
                                        .build_int_to_ptr(
                                            payload.into_int_value(),
                                            self.context.ptr_type(inkwell::AddressSpace::default()),
                                            "err_tuple_ptr",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!(
                                                "inttoptr err tuple: {e}"
                                            ))
                                        })?;
                                    self.builder
                                        .build_load(
                                            BasicTypeEnum::StructType(tuple_llvm_ty),
                                            ptr,
                                            "err_tuple_loaded",
                                        )
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("load err tuple: {e}"))
                                        })?
                                } else {
                                    payload
                                };
                                // For Tuple patterns inside built-in constructors
                                // with ptrtoint-encoded i64 payloads, extract each
                                // field individually and convert i64→struct via
                                // inttoptr+load, so Variable bindings get the
                                // correct LLVM struct type instead of a raw i64.
                                if let PatternKind::Tuple(sub_pats) = &inner_pat.kind {
                                    let decoded_struct = recurse_payload.into_struct_value();
                                    // Determine the expected Mimi types for each
                                    // tuple field from the scrutinee's error type.
                                    // Result<T, (Source, E)> → error tuple field types.
                                    let err_field_mimi_types: Vec<crate::ast::Type> =
                                        scrutinee_type
                                            .and_then(|st| match st {
                                                crate::ast::Type::Result(_, err_tuple) => {
                                                    match err_tuple.as_ref() {
                                                        crate::ast::Type::Tuple(elems) => {
                                                            Some(elems.clone())
                                                        }
                                                        _ => None,
                                                    }
                                                }
                                                _ => None,
                                            })
                                            .unwrap_or_default();
                                    for (i, sub_pat) in sub_pats.iter().enumerate() {
                                        // Extract i-th field (i64 ptrtoint)
                                        let field_i64 = self
                                            .builder
                                            .build_extract_value(
                                                decoded_struct,
                                                i as u32,
                                                &format!("err_tuple_field_{i}"),
                                            )
                                            .map_err(|e| {
                                                CompileError::LlvmError(format!(
                                                    "extract err tuple field {i}: {e}"
                                                ))
                                            })?
                                            .into_int_value();
                                        // If we know the expected Mimi type, inttoptr
                                        // and load as the correct LLVM struct type.
                                        let field_val: BasicValueEnum<'ctx> = if let Some(
                                            field_mimi_ty,
                                        ) =
                                            err_field_mimi_types.get(i)
                                        {
                                            match self.llvm_type_for(field_mimi_ty) {
                                                Some(target_llvm) => {
                                                    let ptr = self
                                                        .builder
                                                        .build_int_to_ptr(
                                                            field_i64,
                                                            self.context.ptr_type(
                                                                inkwell::AddressSpace::default(),
                                                            ),
                                                            &format!("field_{i}_ptr"),
                                                        )
                                                        .map_err(|e| {
                                                            CompileError::LlvmError(format!(
                                                                "inttoptr field {i}: {e}"
                                                            ))
                                                        })?;
                                                    self.builder
                                                        .build_load(
                                                            target_llvm,
                                                            ptr,
                                                            &format!("field_{i}_loaded"),
                                                        )
                                                        .map_err(|e| {
                                                            CompileError::LlvmError(format!(
                                                                "load field {i}: {e}"
                                                            ))
                                                        })?
                                                }
                                                None => {
                                                    // Fallback: keep as i64
                                                    field_i64.into()
                                                }
                                            }
                                        } else {
                                            field_i64.into()
                                        };
                                        self.compile_pattern_bind(
                                            sub_pat,
                                            field_val,
                                            &mut local_vars,
                                        )?;
                                        // Register var_type_names for inner
                                        // variables so infer_object_type can
                                        // resolve the type name for field access.
                                        if let PatternKind::Variable(vname) = &sub_pat.kind {
                                            if let Some(field_mimi_ty) = err_field_mimi_types.get(i)
                                            {
                                                let ty_name =
                                                    Self::mimi_type_to_type_name(field_mimi_ty);
                                                if let Some(tn) = ty_name {
                                                    self.var_type_names.insert(vname.clone(), tn);
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    // Non-Tuple inner pattern: fall through to
                                    // compile_pattern_bind with the decoded payload.
                                    self.compile_pattern_bind(
                                        inner_pat,
                                        recurse_payload,
                                        &mut local_vars,
                                    )?;
                                }
                            }
                        }
                    }
                }
            }
            PatternKind::Tuple(inner_pats) => {
                // For tuple patterns, bind inner variables by loading from struct.
                // Prefer the actual struct type from the scrutinee value when available;
                // for PointerValue scrutinees resolve_pointer_tuple_type prefers the
                // scrutinee's AST tuple type, then tuple_type_stack (possibly empty).
                let (struct_ty, scrutinee_ptr) = match scrutinee_val {
                    BasicValueEnum::StructValue(sv) => {
                        let actual_ty = sv.get_type();
                        let alloca = self.build_alloca(actual_ty, "tuple_alloca")?;
                        self.build_store(alloca, sv)?;
                        (actual_ty, alloca)
                    }
                    BasicValueEnum::PointerValue(pv) => {
                        let stack_ty = self.resolve_pointer_tuple_type(scrutinee_type)?;
                        (stack_ty, pv)
                    }
                    _ => return Ok(local_vars),
                };
                let struct_ty_enum = BasicTypeEnum::StructType(struct_ty);
                for (j, inner_pat) in inner_pats.iter().enumerate() {
                    if let PatternKind::Variable(name) = &inner_pat.kind {
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
            PatternKind::Array(inner_pats) => {
                // For array patterns, bind inner variables by loading from list data.
                // Dispatch only reaches this body for list subjects, so a None
                // coercion (non-list) is unreachable — skip binding fail-closed.
                let scrutinee_ptr = match self.coerce_list_scrutinee_ptr(scrutinee_val, false)? {
                    Some(ptr) => ptr,
                    None => return Ok(local_vars),
                };
                let data_ptr = self.load_list_data_ptr(scrutinee_ptr)?;
                self.bind_list_prefix(data_ptr, inner_pats, &mut local_vars)?;
            }
            PatternKind::Slice(inner_pats, rest) => {
                // For slice patterns, bind prefix variables and rest as list
                // (dispatch guarantees the subject is a list — see the Array arm).
                let scrutinee_ptr = match self.coerce_list_scrutinee_ptr(scrutinee_val, false)? {
                    Some(ptr) => ptr,
                    None => return Ok(local_vars),
                };
                let data_ptr = self.load_list_data_ptr(scrutinee_ptr)?;
                self.bind_list_prefix(data_ptr, inner_pats, &mut local_vars)?;

                // AUDIT FIX (full-audit-2026-08-05 §7, match.rs:653-674): bind
                // `rest` to the ACTUAL remainder list subject[pats.len()..].
                // The old code bound a hardcoded empty i64 0 ("simplified"),
                // so `len(rest)` and element access diverged from the VM —
                // which binds `__slice(subject, pats.len(), len(subject))`
                // (interp/bytecode/compiler.rs:4021-4056). Mirrors the memcpy
                // remainder construction of the irrefutable `let [..rest]`
                // path (codegen/func/pattern.rs). Dispatch guarantees
                // len >= pats.len() here, so the subtraction cannot wrap.
                if let Some(rest_pat) = rest.as_ref() {
                    if let PatternKind::Variable(name) = &rest_pat.kind {
                        let i64_ty = self.context.i64_type();
                        let total_len = self.load_list_len(scrutinee_ptr)?;
                        let prefix = i64_ty.const_int(inner_pats.len() as u64, false);
                        let rest_len = self
                            .builder
                            .build_int_sub(total_len, prefix, "rest_len")
                            .map_err(|e| CompileError::LlvmError(format!("sub: {}", e)))?;
                        let src = self
                            .gep()
                            .build_in_bounds_gep(
                                BasicTypeEnum::IntType(i64_ty),
                                data_ptr,
                                &[prefix],
                                "rest_src",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
                        let bytes = self
                            .builder
                            .build_int_mul(rest_len, i64_ty.const_int(8, false), "rest_bytes")
                            .map_err(|e| CompileError::LlvmError(format!("mul: {}", e)))?;
                        // Empty remainder: skip malloc(0) and bind a null-data
                        // empty list (same shape as func/pattern.rs).
                        let zero = i64_ty.const_int(0, false);
                        let is_empty = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::EQ,
                                rest_len,
                                zero,
                                "rest_empty",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("cmp: {}", e)))?;
                        let function = self.current_function().ok_or_else(|| {
                            CompileError::LlvmError("no function for slice rest".to_string())
                        })?;
                        let empty_bb = self.context.append_basic_block(function, "rest_empty_bb");
                        let copy_bb = self.context.append_basic_block(function, "rest_copy_bb");
                        let rest_merge_bb =
                            self.context.append_basic_block(function, "rest_merge_bb");
                        self.build_cond_br(is_empty, empty_bb, copy_bb)?;

                        self.builder.position_at_end(empty_bb);
                        let null_data = self
                            .context
                            .ptr_type(inkwell::AddressSpace::default())
                            .const_null();
                        let empty_list = self.build_list_struct(zero, null_data)?;
                        self.build_br(rest_merge_bb)?;
                        let empty_end = self.builder.get_insert_block().unwrap_or(empty_bb);

                        self.builder.position_at_end(copy_bb);
                        let dest = self.malloc_or_abort(bytes, "rest_data")?;
                        // SAFETY: `src` covers elements [prefix, total_len) of a
                        // valid list data allocation (dispatch established
                        // total_len >= prefix), `dest` is a fresh `bytes`-sized
                        // allocation from malloc_or_abort, and the regions are
                        // disjoint.
                        let memcpy_fn = self.get_runtime_fn("memcpy")?;
                        self.builder
                            .build_call(
                                memcpy_fn,
                                &[
                                    BasicMetadataValueEnum::PointerValue(dest),
                                    BasicMetadataValueEnum::PointerValue(src),
                                    BasicMetadataValueEnum::IntValue(bytes),
                                ],
                                "rest_memcpy",
                            )
                            .map_err(|e| CompileError::LlvmError(format!("memcpy: {}", e)))?;
                        // build_list_struct registers the data slot for
                        // scope-exit free; do not also register_heap_alloc
                        // (double free).
                        let copy_list = self.build_list_struct(rest_len, dest)?;
                        self.build_br(rest_merge_bb)?;
                        let copy_end = self.builder.get_insert_block().unwrap_or(copy_bb);

                        self.builder.position_at_end(rest_merge_bb);
                        let phi_ty = empty_list.get_type();
                        let phi = self
                            .builder
                            .build_phi(phi_ty, "rest_list_phi")
                            .map_err(|e| CompileError::LlvmError(format!("phi: {}", e)))?;
                        phi.add_incoming(&[(&empty_list, empty_end), (&copy_list, copy_end)]);
                        let rest_val = phi.as_basic_value();
                        self.bind_pattern_var(
                            &mut local_vars,
                            name,
                            rest_val,
                            rest_val.get_type(),
                        )?;
                    }
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) => {
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

    /// Coerce a list-pattern scrutinee to a `{i64 len, i8* data}` struct
    /// pointer, or `None` if the value is not a list.
    ///
    /// Lists are normally PointerValue; a by-value list StructValue occurs for
    /// nested subjects (e.g. `match xs[0] { .. }` on List<List<T>>) and is
    /// materialized into a fresh alloca. Only a genuine list shape
    /// `{i64, ptr}` is coerced — every other shape (enum `{i32,i64}`, tuple,
    /// string `{ptr,i64}`) must NOT match a list pattern: the VM tests
    /// `TypeOf == "list"` first (interp/bytecode/compiler.rs:3832-3848), and
    /// GEP-ing any other shape as a list struct would be UB.
    fn coerce_list_scrutinee_ptr(
        &self,
        scrutinee: BasicValueEnum<'ctx>,
        is_string_scrutinee: bool,
    ) -> Result<Option<PointerValue<'ctx>>, CompileError> {
        Ok(match scrutinee {
            BasicValueEnum::PointerValue(pv) if !is_string_scrutinee => Some(pv),
            BasicValueEnum::StructValue(sv) if !is_string_scrutinee => {
                let fields = sv.get_type().get_field_types();
                let is_list_shape = matches!(
                    fields.as_slice(),
                    [BasicTypeEnum::IntType(t), BasicTypeEnum::PointerType(_)]
                        if t.get_bit_width() == 64
                );
                if !is_list_shape {
                    return Ok(None);
                }
                let alloca =
                    self.build_alloca(BasicTypeEnum::StructType(sv.get_type()), "list_scrutinee")?;
                self.build_store(alloca, sv)?;
                Some(alloca)
            }
            _ => None,
        })
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
            if let PatternKind::Variable(name) = &inner_pat.kind {
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

    /// Resolve the struct layout for a pointer-form tuple scrutinee.
    ///
    /// AUDIT FIX (full-audit-2026-08-05 §7, record.rs:284 + match.rs:615/769):
    /// `tuple_type_stack` is now pushed/popped symmetrically in
    /// `compile_tuple_expr`, so it may legitimately be EMPTY here — and even
    /// when non-empty its top may be unrelated to this scrutinee (the old
    /// never-popped pushes were exactly the "stale layout" hazard). Prefer
    /// the scrutinee's AST tuple type when known (authoritative: the checker
    /// assigned it), fall back to the stack, and fail closed with a clear
    /// error instead of guessing a layout.
    fn resolve_pointer_tuple_type(
        &self,
        scrutinee_type: Option<&crate::ast::Type>,
    ) -> Result<inkwell::types::StructType<'ctx>, CompileError> {
        if let Some(ty) = scrutinee_type {
            if let crate::ast::Type::Tuple(elems) = ty.unlocated() {
                let i64_default = BasicTypeEnum::IntType(self.context.i64_type());
                let field_tys: Vec<BasicTypeEnum<'ctx>> = elems
                    .iter()
                    .map(|t| {
                        crate::codegen::types::mimi_type_to_llvm(self.context, t)
                            .unwrap_or(i64_default)
                    })
                    .collect();
                return Ok(self.context.struct_type(&field_tys, false));
            }
        }
        self.tuple_type_stack.last().copied().ok_or_else(|| {
            CompileError::LlvmError(
                "tuple pattern on pointer scrutinee: no tuple type known and \
                 tuple_type_stack is empty"
                    .to_string(),
            )
        })
    }

    /// Generate element-wise comparison for a tuple pattern.
    /// Returns `Some(i1)` if any element requires comparison, `None` for wildcard-only patterns.
    fn compile_tuple_pattern(
        &self,
        scrutinee: BasicValueEnum<'ctx>,
        inner_pats: &[Pattern],
        scrutinee_type: Option<&crate::ast::Type>,
    ) -> Result<Option<inkwell::values::IntValue<'ctx>>, CompileError> {
        let i64_ty = self.context.i64_type();
        // Normalize to a struct value so we can use the real struct type for GEPs.
        let (tuple_ptr, struct_ty) = match scrutinee {
            BasicValueEnum::PointerValue(pv) => {
                let struct_ty = self.resolve_pointer_tuple_type(scrutinee_type)?;
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
            let lit_val = match &pat.kind {
                PatternKind::Literal(lit) => match lit {
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
    ///
    /// AUDIT FIX (full-audit-2026-08-05 §7, match.rs:835-917/1396-1411):
    /// exact-length list pattern — delegates to `compile_list_pattern_test`
    /// with `exact_len = true` (VM reference: bytecode `EqInt len == pats.len()`,
    /// interp/bytecode/compiler.rs:3830-3879).
    fn compile_array_pattern(
        &self,
        scrutinee: BasicValueEnum<'ctx>,
        inner_pats: &[Pattern],
        is_string_scrutinee: bool,
    ) -> Result<Option<inkwell::values::IntValue<'ctx>>, CompileError> {
        self.compile_list_pattern_test(scrutinee, inner_pats, true, is_string_scrutinee)
    }

    /// Generate the dispatch test for list patterns (array + slice).
    ///
    /// AUDIT FIX (full-audit-2026-08-05 §7, match.rs:835-917/1396-1411):
    /// the previous implementation compared only prefix elements with NO
    /// length check and NO subject-kind test: `match [1, 2, 3] { [1, 2] => .. }`
    /// over-matched, an empty `[]` pattern returned `None` and the dispatcher
    /// took an unconditional br (matching EVERYTHING, including non-lists),
    /// and element loads read out of bounds when the subject was shorter than
    /// the pattern. The bytecode VM is the reference semantics:
    /// array patterns require `TypeOf == "list"` AND `len == pats.len()`
    /// (compiler.rs:3830-3879), slice patterns `len >= pats.len()`
    /// (compiler.rs:3928-3977), and element tests run only after the
    /// length check passes (VM `JmpIfNot` skip around element access).
    ///
    /// Emitted shape: `len_test` (EQ for array / SGE for slice), then — if
    /// any element carries a literal — a guarded diamond:
    ///
    /// ```text
    ///          len_test?
    ///          /      \
    ///   elems_bb    nomatch_bb
    ///   (loads+cmp)     |
    ///          \      /
    ///          phi(i1)
    /// ```
    ///
    /// Element loads live only in `elems_bb`, which is entered with
    /// `len >= pats.len()` established, so no OOB read is possible.
    /// Non-list subjects never match: a non-pointer value is not a list, and
    /// a statically-known string scrutinee is a pointer but not a list struct
    /// (VM: `TypeOf != "list"` → fallthrough; also avoids GEP-ing a string
    /// pointer as `{i64 len, i8* data}`). In both cases a constant-false
    /// test is returned so the arm falls through instead of matching.
    fn compile_list_pattern_test(
        &self,
        scrutinee: BasicValueEnum<'ctx>,
        inner_pats: &[Pattern],
        exact_len: bool,
        is_string_scrutinee: bool,
    ) -> Result<Option<inkwell::values::IntValue<'ctx>>, CompileError> {
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        // Not a list (or statically known string) → arm never matches.
        let scrutinee_ptr = match self.coerce_list_scrutinee_ptr(scrutinee, is_string_scrutinee)? {
            Some(ptr) => ptr,
            None => return Ok(Some(bool_ty.const_int(0, false))),
        };
        let len = self.load_list_len(scrutinee_ptr)?;
        let expected_len = i64_ty.const_int(inner_pats.len() as u64, false);
        let len_predicate = if exact_len {
            inkwell::IntPredicate::EQ
        } else {
            inkwell::IntPredicate::SGE
        };
        let len_ok = self
            .builder
            .build_int_compare(len_predicate, len, expected_len, "list_len_test")
            .map_err(|e| CompileError::LlvmError(format!("list len cmp: {}", e)))?;

        // If no element carries a literal, the length test IS the complete
        // pattern test (variables/wildcards add no comparisons — VM emits no
        // element tests for them either).
        let needs_element_test = inner_pats
            .iter()
            .any(|p| matches!(&p.kind, PatternKind::Literal(_)));
        if !needs_element_test {
            return Ok(Some(len_ok));
        }

        // Guarded element comparisons (see doc comment).
        let function = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .ok_or_else(|| {
                CompileError::LlvmError("list pattern test has no function".to_string())
            })?;
        let elems_bb = self.context.append_basic_block(function, "list_elems");
        let nomatch_bb = self.context.append_basic_block(function, "list_nomatch");
        let merge_bb = self.context.append_basic_block(function, "list_merge");
        self.build_cond_br(len_ok, elems_bb, nomatch_bb)?;

        self.builder.position_at_end(elems_bb);
        // SAFETY: element loads below execute only when `len_ok` held at
        // runtime (len >= inner_pats.len()), so every index is in bounds.
        let data_ptr = self.load_list_data_ptr(scrutinee_ptr)?;
        let mut agg: Option<inkwell::values::IntValue<'ctx>> = None;
        for (j, pat) in inner_pats.iter().enumerate() {
            let lit_val = match &pat.kind {
                PatternKind::Literal(lit) => match lit {
                    Lit::Int(n) => Some(i64_ty.const_int(*n as u64, true)),
                    Lit::Bool(b) => Some(i64_ty.const_int(*b as u64, false)),
                    Lit::Unit => Some(i64_ty.const_int(0, false)),
                    _ => return Err("unsupported array element literal type".into()),
                },
                _ => None,
            };
            if let Some(expected) = lit_val {
                let idx = i64_ty.const_int(j as u64, false);
                let elem_ptr = self
                    .gep()
                    .build_gep(i64_ty, data_ptr, &[idx], &format!("arr_el{}", j))
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
        let elems_match = agg.unwrap_or_else(|| bool_ty.const_int(1, false));
        self.build_br(merge_bb)?;
        let elems_end = self.builder.get_insert_block().unwrap_or(elems_bb);

        self.builder.position_at_end(nomatch_bb);
        self.build_br(merge_bb)?;

        self.builder.position_at_end(merge_bb);
        let nomatch_val = bool_ty.const_int(0, false);
        let phi = self
            .builder
            .build_phi(bool_ty, "list_pat_test")
            .map_err(|e| CompileError::LlvmError(format!("list pattern phi: {}", e)))?;
        phi.add_incoming(&[
            (&elems_match as &dyn inkwell::values::BasicValue, elems_end),
            (&nomatch_val, nomatch_bb),
        ]);
        Ok(Some(phi.as_basic_value().into_int_value()))
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
        is_string_scrutinee: bool,
    ) -> Result<Option<inkwell::values::IntValue<'ctx>>, CompileError> {
        // AUDIT FIX (full-audit-2026-08-05 §7, match.rs:1045-1053): a slice
        // pattern `[p1, .., ..rest]` requires `len >= pats.len()` (VM `GeInt`,
        // interp/bytecode/compiler.rs:3951-3977), not the array pattern's
        // exact length — the old delegation to `compile_array_pattern`
        // (which had no length test at all) both rejected valid matches and
        // over-matched shorter ones. `rest` affects bindings only (see
        // bind_pattern_variables), not the dispatch test.
        self.compile_list_pattern_test(scrutinee, inner_pats, false, is_string_scrutinee)
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
            arms.iter().any(|arm| {
                matches!(
                    &arm.pat.kind,
                    PatternKind::Constructor(_, _) | PatternKind::Literal(_)
                )
            })
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

        // v0.34.18a: compute the scrutinee type once so arm dispatch can scope
        // variant-ordinal resolution to the scrutinee's enum (disambiguates the
        // shared `Fault` variant across per-flow __MultiTarget unions).
        let scrutinee_type = self.expr_type_of(scrutinee, vars);

        // Build if-else chain for each arm
        for (i, arm) in arms.iter().enumerate() {
            let (arm_bb, next_else_bb) = self.compile_match_arm_dispatch(
                i,
                arm,
                scrutinee_val,
                scrutinee_iv,
                else_bb,
                scrutinee_type.as_ref(),
                is_string_scrutinee,
            )?;
            // Guard failure must continue to the next arm's dispatch block, so
            // update else_bb before compiling the arm body.
            else_bb = next_else_bb;
            let env = MatchArmEnv {
                scrutinee_val,
                scrutinee_iv,
                merge_bb,
                else_bb,
                scrutinee_type: scrutinee_type.clone(),
            };
            let (arm_val, body_bb) = self.compile_match_arm_body(i, arm, arm_bb, vars, &env)?;
            incoming_vals.push(arm_val);
            incoming_bbs.push(body_bb);
        }

        // Unreachable else block. In a fallible multi-target transition the
        // panic is absorbed into the Fault variant (parity with the bytecode
        // VM, which reports a non-exhaustive match as panic:E0805); everywhere
        // else call mimi_match_panic (runtime trap) before build_unreachable()
        // so that if a non-exhaustive match is reached at runtime, the program
        // prints a diagnostic and aborts instead of UB.
        self.builder.position_at_end(else_bb);
        if self.in_fallible_multi_target() {
            // H1 (audit-codegen): the trap site is lexically inside the
            // transition body — absorbing keeps dual-backend parity (interp
            // E0805). Callers (helper/泛型) compiled as standalone functions
            // have the flag cleared (H2) and fall through to the abort path.
            // build_return terminates else_bb; the phi below only merges arm
            // blocks, so nothing flows in from here.
            self.emit_panic_fault_return("E0805")?;
        } else {
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
        }

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
        scrutinee_type: Option<&crate::ast::Type>,
        is_string_scrutinee: bool,
    ) -> Result<(BasicBlock<'ctx>, BasicBlock<'ctx>), CompileError> {
        let function = else_bb.get_parent().ok_or_else(|| {
            CompileError::LlvmError("match arm dispatch has no parent function".to_string())
        })?;
        let arm_bb = self
            .context
            .append_basic_block(function, &format!("arm{}", arm_idx));
        self.builder.position_at_end(else_bb);

        match &arm.pat.kind {
            PatternKind::Wildcard | PatternKind::Variable(_) => {
                // If the variable name is actually an enum variant, treat it as a
                // unit constructor pattern and compare the tag.
                let is_variant = if let PatternKind::Variable(name) = &arm.pat.kind {
                    self.find_variant_ordinal_scoped(name, scrutinee_type)
                        .is_ok()
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
                        .find_variant_ordinal_scoped(
                            if let PatternKind::Variable(name) = &arm.pat.kind {
                                name
                            } else {
                                ""
                            },
                            scrutinee_type,
                        )
                        .map_err(|e| {
                            CompileError::LlvmError(format!("match arm variant lookup: {}", e))
                        })?;
                    // K-2 family: tag constant at the scrutinee's own width
                    // (icmp operands must match).
                    let tag_val = scrutinee_iv.get_type().const_int(ordinal, false);
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
            PatternKind::Literal(lit) => {
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
                        // K-2 (full-audit 2026-08-05 §3.6): materialize the
                        // arm constant at the scrutinee's OWN width. The old
                        // `bw < 64 → i32 / else → i64` split produced an i32
                        // constant for an i1 (bool) scrutinee, and the icmp
                        // below (i1 vs i32) is invalid IR — the legacy channel
                        // has no per-function verify fallback, so it ICEs.
                        // Building from scrutinee_iv.get_type() is correct for
                        // i1, i32 and i64 scrutinees alike.
                        Lit::Int(n) => scrutinee_iv.get_type().const_int(*n as u64, true),
                        Lit::Bool(b) => scrutinee_iv.get_type().const_int(*b as u64, false),
                        Lit::Unit => scrutinee_iv.get_type().const_int(0, false),
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
            PatternKind::Constructor(name, sub_patterns) => {
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
                // Look up the variant ordinal index from type definitions,
                // scoped to the scrutinee's enum (v0.34.18a: disambiguates the
                // shared `Fault` variant across per-flow __MultiTarget unions).
                let ordinal = self
                    .find_variant_ordinal_scoped(name, scrutinee_type)
                    .map_err(|e| {
                        CompileError::LlvmError(format!("match arm variant lookup: {}", e))
                    })?;
                // K-2 family: tag constant at the scrutinee's own width
                // (icmp operands must match).
                let tag_val = scrutinee_iv.get_type().const_int(ordinal, false);
                let mut arm_cond = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, scrutinee_iv, tag_val, "cmp")
                    .map_err(|e| CompileError::LlvmError(format!("cmp error: {}", e)))?;
                // 0.35.7-fix: literal sub-patterns (e.g. `B(true) => ...`) must
                // be part of the ARM CONDITION, not deferred to pattern binding.
                // The old flow entered the arm on tag match alone, then the
                // binder's PatternKind::Literal branch asserted — so a
                // `Bool(false)` value under a `Bool(true)` arm aborted the whole
                // program instead of falling through to the next arm. Compare
                // each literal field against the extracted payload and AND it
                // into the branch condition.
                let sv = match scrutinee_val {
                    BasicValueEnum::StructValue(sv) => sv,
                    BasicValueEnum::PointerValue(pv) => {
                        let sty = self
                            .llvm_type_for(scrutinee_type.unwrap_or(&crate::ast::Type::Infer))
                            .or_else(|| {
                                Some(BasicTypeEnum::StructType(self.context.struct_type(
                                    &[
                                        BasicTypeEnum::IntType(self.context.i32_type()),
                                        BasicTypeEnum::IntType(self.context.i64_type()),
                                    ],
                                    false,
                                )))
                            })
                            .ok_or_else(|| {
                                CompileError::LlvmError(
                                    "literal-pattern arm: unknown scrutinee struct type".into(),
                                )
                            })?;
                        self.build_load(sty, pv, "pat_scrutinee")?
                            .into_struct_value()
                    }
                    _ => {
                        return Err(CompileError::LlvmError(
                            "literal-pattern arm requires a struct scrutinee".into(),
                        ))
                    }
                };
                for (i, (_, sub_pat)) in sub_patterns.iter().enumerate() {
                    if let PatternKind::Literal(lit) = &sub_pat.kind {
                        let payload = self
                            .builder
                            .build_extract_value(sv, (i + 1) as u32, "pat_payload")
                            .map_err(|e| {
                                CompileError::LlvmError(format!("extract payload: {}", e))
                            })?;
                        let lit_val = self.compile_literal_expr(lit, &HashMap::new())?;
                        let payload_cmp = match (payload, lit_val) {
                            (BasicValueEnum::IntValue(p), BasicValueEnum::IntValue(l)) => {
                                // Normalize widths: truncate/extend the payload
                                // to the literal's width at match time.
                                let p_norm = if p.get_type().get_bit_width()
                                    > l.get_type().get_bit_width()
                                {
                                    self.builder
                                        .build_int_truncate(p, l.get_type(), "pat_payload_trunc")
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("payload trunc: {}", e))
                                        })?
                                } else if p.get_type().get_bit_width()
                                    < l.get_type().get_bit_width()
                                {
                                    self.builder
                                        .build_int_z_extend(p, l.get_type(), "pat_payload_zext")
                                        .map_err(|e| {
                                            CompileError::LlvmError(format!("payload zext: {}", e))
                                        })?
                                } else {
                                    p
                                };
                                self.builder
                                    .build_int_compare(
                                        inkwell::IntPredicate::EQ,
                                        p_norm,
                                        l,
                                        "pat_payload_eq",
                                    )
                                    .map_err(|e| {
                                        CompileError::LlvmError(format!("payload cmp: {}", e))
                                    })?
                            }
                            _ => {
                                return Err(CompileError::LlvmError(
                                    "literal sub-pattern: non-integer payload".into(),
                                ))
                            }
                        };
                        arm_cond = self
                            .builder
                            .build_and(arm_cond, payload_cmp, "pat_cond_and")
                            .map_err(|e| CompileError::LlvmError(format!("and: {}", e)))?;
                    }
                }
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                self.build_cond_br(arm_cond, arm_bb, next_bb)?;
                Ok((arm_bb, next_bb))
            }
            PatternKind::Tuple(inner_pats) => {
                let match_cmp =
                    self.compile_tuple_pattern(scrutinee_val, inner_pats, scrutinee_type)?;
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                match match_cmp {
                    Some(cmp) => self.build_cond_br(cmp, arm_bb, next_bb)?,
                    None => self.build_br(arm_bb)?,
                }
                Ok((arm_bb, next_bb))
            }
            PatternKind::Array(inner_pats) => {
                let match_cmp =
                    self.compile_array_pattern(scrutinee_val, inner_pats, is_string_scrutinee)?;
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                // AUDIT FIX (full-audit-2026-08-05 §7): compile_list_pattern_test
                // ALWAYS returns Some(test) — the old `None => build_br(arm_bb)`
                // unconditional arm (empty/wildcard-only patterns) matched every
                // subject including non-lists. Kept defensively: a `None` here
                // would reintroduce the over-match, so fail closed instead.
                match match_cmp {
                    Some(cmp) => self.build_cond_br(cmp, arm_bb, next_bb)?,
                    None => {
                        return Err(CompileError::LlvmError(
                            "array pattern dispatch: missing pattern test".to_string(),
                        ))
                    }
                }
                Ok((arm_bb, next_bb))
            }
            PatternKind::Slice(inner_pats, rest) => {
                let match_cmp = self.compile_slice_pattern(
                    scrutinee_val,
                    inner_pats,
                    rest,
                    is_string_scrutinee,
                )?;
                let next_bb = self
                    .context
                    .append_basic_block(function, &format!("next{}", arm_idx));
                // Same fail-closed guard as Array above.
                match match_cmp {
                    Some(cmp) => self.build_cond_br(cmp, arm_bb, next_bb)?,
                    None => {
                        return Err(CompileError::LlvmError(
                            "slice pattern dispatch: missing pattern test".to_string(),
                        ))
                    }
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

        let local_vars = self.bind_pattern_variables(
            arm,
            env.scrutinee_val,
            env.scrutinee_iv,
            vars,
            env.scrutinee_type.as_ref(),
        )?;
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
                let prior_count = self
                    .heap_allocs
                    .borrow()
                    .last()
                    .map(|s| s.len())
                    .unwrap_or(0);
                let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                // MATCH-STRFIX: if the arm body produces a heap-allocated
                // string (fstring, concat, etc.), detach the heap entries so
                // free_heap_allocs doesn't free data the caller still needs.
                self.claim_match_arm_string(&arm.body, &arm_val, prior_count);
                let guarded_body_bb = self.builder.get_insert_block().ok_or_else(|| {
                    CompileError::LlvmError("no insert block after guard arm body".to_string())
                })?;
                self.build_br(env.merge_bb)?;
                Ok((arm_val, guarded_body_bb))
            }
            None => {
                let prior_count = self
                    .heap_allocs
                    .borrow()
                    .last()
                    .map(|s| s.len())
                    .unwrap_or(0);
                let arm_val = self.compile_expr(&arm.body, &local_vars)?;
                // MATCH-STRFIX: same as above for unguarded arms.
                self.claim_match_arm_string(&arm.body, &arm_val, prior_count);
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
            // CG-H4: refuse silent zero substitution for type-mismatched match arms.
            return Err(CompileError::TypeMismatch(format!(
                "match arm values have incompatible types (cannot unify {:?} with {:?})",
                v.get_type(),
                ty
            )));
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

    /// If a match arm body produces a heap-allocated string (fstring, string
    /// concat, etc.), pop the heap entries so that `free_heap_allocs` at scope
    /// exit does not free data the caller will receive through the match
    /// result / phi node.
    ///
    /// Uses a prior-count approach: `prior_count` is the number of heap entries
    /// in the current scope before the arm body was compiled. We truncate back
    /// to that count, removing only the entries added by this arm body. This is
    /// exact regardless of how many entries the arm body's expression produced
    /// (varies: N+1 Ptr entries for fstring with N interpolations, 1 for string
    /// concat, etc.) and independent of entry type (Ptr vs Slot).
    fn claim_match_arm_string(&self, body: &Expr, _val: &BasicValueEnum<'ctx>, prior_count: usize) {
        if self.is_string_producing_expr(body) {
            let mut guard = self.heap_allocs.borrow_mut();
            if let Some(scope) = guard.last_mut() {
                let added = scope.len().saturating_sub(prior_count);
                if added > 0 {
                    scope.truncate(prior_count);
                }
            }
        }
    }

    /// Check if an expression produces a heap-allocated string, looking
    /// through Block and If wrappers.
    ///
    /// `Expr::If` branches use `compile_block_last_val` (not `compile_block`),
    /// so fstring/interpolation heap entries go into the enclosing scope —
    /// not a nested scope. If we miss this, `free_heap_allocs` frees the
    /// string buffer while the if-expr phi still references it, causing
    /// dangling-pointer access at runtime.
    fn is_string_producing_expr(&self, expr: &Expr) -> bool {
        match expr.unlocated() {
            Expr::Literal(Lit::FString(_)) => true,
            Expr::Binary(BinOp::Add, _, _) => true,
            Expr::Call(callee, _) => {
                matches!(
                    callee.unlocated(),
                    Expr::Ident(name) if matches!(
                        name.as_str(),
                        "str_concat" | "str_repeat" | "str_slice"
                            | "str_trim" | "str_join" | "str_from"
                            | "to_string" | "format"
                    )
                )
            }
            // Look through block wrappers: { f"..." } or { let x = ...; f"..." }
            Expr::Block(stmts) => self.block_last_is_string_producer(stmts),
            // Recurse into if-expr: both branches are Blocks compiled via
            // compile_block_last_val (no heap scope push), so string-producing
            // tail expressions in either branch leak heap entries into the
            // enclosing scope and must be claimed.
            Expr::If {
                cond: _,
                then_,
                else_,
            } => {
                self.block_last_is_string_producer(then_)
                    || else_
                        .as_ref()
                        .is_some_and(|blk| self.block_last_is_string_producer(blk))
            }
            _ => false,
        }
    }

    /// Check if the last statement of a Block is a string-producing expression.
    fn block_last_is_string_producer(&self, block: &Block) -> bool {
        if let Some(last) = block.last() {
            match last.unlocated() {
                Stmt::Expr(e) => self.is_string_producing_expr(e),
                _ => false,
            }
        } else {
            false
        }
    }
}
