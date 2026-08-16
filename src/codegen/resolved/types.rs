//! LLVM type lowering for checker-owned canonical types.
//!
//! This bridge deliberately accepts only [`ResolvedTypeId`] identities from a
//! [`ResolvedTypeTable`].  It must not recover a surface spelling, inspect the
//! legacy AST, or silently erase an unknown shape to `i64`.

use std::collections::BTreeSet;

use inkwell::context::Context;
use inkwell::types::BasicTypeEnum;
use inkwell::AddressSpace;

use crate::core::{
    FunctionTypeAbi, PrimitiveType, ResolvedType, ResolvedTypeId, ResolvedTypeTable,
};
use crate::error::CompileError;

/// Lower a checker-owned canonical type identity to its native LLVM layout.
///
/// The first migration slice intentionally supports only closed structural
/// types whose layout is independent of a declaration catalog. Nominal and
/// all other declaration-dependent/resource types fail closed with
/// [`CompileError::Unsupported`].
pub(in crate::codegen) fn llvm_type_for_resolved<'ctx>(
    context: &'ctx Context,
    types: &ResolvedTypeTable,
    id: &ResolvedTypeId,
) -> Result<BasicTypeEnum<'ctx>, CompileError> {
    let mut active = BTreeSet::new();
    let mut no_hook = |_: &ResolvedTypeId| None;
    lower_resolved_type(context, types, id, &mut active, &mut no_hook)
}

/// 0.36.35: nominal-resolution hook — lets a caller (the resolved emitter with
/// generator/type_defs access) supply the LLVM layout for nominal identities
/// the pure table cannot derive (Flow-state record layouts). Consulted inside
/// Nominal lowering BEFORE the builtin-name match, so nested container payloads
/// (Result<state, E>, Option<state>) get the same layout as top-level values.
pub(in crate::codegen) fn llvm_type_for_resolved_with<'ctx>(
    context: &'ctx Context,
    types: &ResolvedTypeTable,
    id: &ResolvedTypeId,
    nominal_hook: &mut dyn FnMut(&ResolvedTypeId) -> Option<BasicTypeEnum<'ctx>>,
) -> Result<BasicTypeEnum<'ctx>, CompileError> {
    let mut active = BTreeSet::new();
    lower_resolved_type(context, types, id, &mut active, nominal_hook)
}

fn lower_resolved_type<'ctx>(
    context: &'ctx Context,
    types: &ResolvedTypeTable,
    id: &ResolvedTypeId,
    active: &mut BTreeSet<ResolvedTypeId>,
    nominal_hook: &mut dyn FnMut(&ResolvedTypeId) -> Option<BasicTypeEnum<'ctx>>,
) -> Result<BasicTypeEnum<'ctx>, CompileError> {
    let resolved = types.get(id).ok_or_else(|| {
        CompileError::Unsupported(format!(
            "canonical LLVM type '{}' is absent from ResolvedTypeTable",
            id.as_str()
        ))
    })?;

    if !active.insert(id.clone()) {
        return Err(CompileError::Unsupported(format!(
            "canonical LLVM type '{}' contains a recursive structural cycle",
            id.as_str()
        )));
    }

    let lowered = match resolved {
        ResolvedType::Primitive(primitive) => Ok(lower_primitive(context, *primitive)),
        ResolvedType::Option(payload) => {
            let payload = lower_resolved_type(context, types, payload, active, nominal_hook)?;
            // Match legacy ABI: widen sub-64-bit integer payloads to i64.
            // This ensures Option<i32> uses {i1, i64} layout, matching
            // mimi_type_to_llvm's Type::Option lowering. Per-function dispatch
            // (cross-emitter) depends on this ABI compatibility.
            let payload = crate::codegen::types::widen_int_to_i64(context, payload);
            Ok(BasicTypeEnum::StructType(context.struct_type(
                &[BasicTypeEnum::IntType(context.bool_type()), payload],
                false,
            )))
        }
        ResolvedType::Result { ok, error: _error } => {
            let ok = lower_resolved_type(context, types, ok, active, nominal_hook)?;
            // Match legacy ABI: widen sub-64-bit integer ok-payload to i64,
            // and ALWAYS use i64 for the error slot regardless of E type.
            // Per-function dispatch (cross-emitter) depends on this compatibility.
            let ok = crate::codegen::types::widen_int_to_i64(context, ok);
            let err_llvm = BasicTypeEnum::IntType(context.i64_type());
            Ok(BasicTypeEnum::StructType(context.struct_type(
                &[BasicTypeEnum::IntType(context.bool_type()), ok, err_llvm],
                false,
            )))
        }
        ResolvedType::Tuple(elements) => {
            let elements = elements
                .iter()
                .map(|element| -> Result<BasicTypeEnum<'ctx>, CompileError> {
                    let lowered =
                        lower_resolved_type(context, types, element, active, nominal_hook)?;
                    // Match legacy ABI: widen sub-64-bit integer fields (except
                    // i1/bool) to i64 in tuple layout. This ensures per-function
                    // dispatch compatibility between resolved and legacy emitters
                    // (legacy mimi_type_to_llvm applies the same widening).
                    Ok(match lowered {
                        BasicTypeEnum::IntType(it)
                            if it.get_bit_width() > 1 && it.get_bit_width() < 64 =>
                        {
                            BasicTypeEnum::IntType(context.i64_type())
                        }
                        other => other,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(BasicTypeEnum::StructType(
                context.struct_type(&elements, false),
            ))
        }
        // Generic parameters are erased to an opaque i64 slot. This is only
        // safe for functions that treat the value opaquely (identity/choose/
        // pair/apply); non-erased use sites still fail closed.
        ResolvedType::GenericParameter(_) => Ok(BasicTypeEnum::IntType(context.i64_type())),
        ResolvedType::Reference { .. } => Ok(BasicTypeEnum::PointerType(
            context.ptr_type(AddressSpace::default()),
        )),
        ResolvedType::Ownership { target, .. } => {
            lower_resolved_type(context, types, target, active, nominal_hook)
        }
        ResolvedType::DynamicAny { .. } => {
            // C3 (audit 2026-08-03): Any (stdlib map/set value type) lowers to
            // an opaque i64 handle — the same ABI as Map/Set handles and the
            // runtime map value box (map_set packs values into i64/ptr slots).
            // This lets stdlib wrappers like `set(m, "k", 1)` and
            // `get(m, "k").1` flow through per-function dispatch.
            Ok(BasicTypeEnum::IntType(context.i64_type()))
        }
        ResolvedType::Function {
            abi,
            parameters,
            result,
        } => {
            // Function representation is opaque at the LLVM pointer level, but
            // recursively validate the full checker signature. This prevents an
            // unsupported nominal/generic type from being hidden inside a
            // seemingly lowerable closure or C function pointer.
            for parameter in parameters {
                let _ = lower_resolved_type(context, types, parameter, active, nominal_hook)?;
            }
            let _ = lower_resolved_type(context, types, result, active, nominal_hook)?;

            let pointer = BasicTypeEnum::PointerType(context.ptr_type(AddressSpace::default()));
            match abi {
                FunctionTypeAbi::Mimi => Ok(BasicTypeEnum::StructType(
                    context.struct_type(&[pointer, pointer], false),
                )),
                FunctionTypeAbi::C => Ok(pointer),
            }
        }
        // 0.32.2: Builtin collection types. List is {i64 len, ptr data}
        // matching the legacy emitter's list_struct_type(). Map and Set
        // share the same runtime representation (handle-based).
        ResolvedType::Nominal {
            item, arguments, ..
        } => {
            // 0.36.35: caller-supplied nominal hook wins (Flow-state record
            // layouts come from the legacy type_defs via the emitter).
            if let Some(hooked) = nominal_hook(id) {
                return Ok(hooked);
            }
            // 0.35.23 deep-eval: the container's LLVM layout is INDEPENDENT
            // of its element type ({i64 len, ptr} for List; opaque i64 handle
            // for Map/Set), so a user-record element (List<LogEntry>) must
            // not fail the container lowering — the old strict recursion
            // made llvm_type_for_resolved(List<LogEntry>) error, mimi-log's
            // main fell back to legacy, and the legacy emitter then hit its
            // List<record> for-loop gap (E0700 field access on an i64 slot).
            // The emitter's lower_type record fallback handles the element
            // layout at the actual use site.
            let _ = arguments
                .iter()
                .map(|arg| lower_resolved_type(context, types, arg, active, nominal_hook))
                .collect::<Vec<_>>();
            match item.as_str() {
                "builtin:type:List" => {
                    let i64_ty = BasicTypeEnum::IntType(context.i64_type());
                    let ptr_ty =
                        BasicTypeEnum::PointerType(context.ptr_type(AddressSpace::default()));
                    Ok(BasicTypeEnum::StructType(
                        context.struct_type(&[i64_ty, ptr_ty], false),
                    ))
                }
                "builtin:type:Map" | "builtin:type:Set" | "builtin:type:Record" => {
                    // Map/Set/Record are opaque handles (i64) at the LLVM level.
                    Ok(BasicTypeEnum::IntType(context.i64_type()))
                }
                // 0.36.32: SessionChan<T> endpoints are opaque i64 handles at
                // the LLVM level (mirroring Map/Set) — the typed/residual
                // surface is compile-time only (E0414/E0425/E0426).
                _v if item.as_str().ends_with("SessionChan") => {
                    Ok(BasicTypeEnum::IntType(context.i64_type()))
                }
                // Future<T> is an opaque i8* handle in both legacy and
                // resolved ABIs; keeping it i64 broke resolved `spawn`/`await`
                // main functions when binding the future to a local.
                _v if item.as_str() == "builtin:type:Future" => Ok(BasicTypeEnum::PointerType(
                    context.ptr_type(AddressSpace::default()),
                )),
                // Actor handles are pointers at the LLVM level (the legacy
                // actor runtime returns and consumes i8* endpoints).
                _v if item.as_str().starts_with("actor:") => Ok(BasicTypeEnum::PointerType(
                    context.ptr_type(AddressSpace::default()),
                )),
                // 0.36.7 (裁决 3/DoD #4): the structured Fault crash-context
                // records must lower in the resolved native slice, mirroring
                // the legacy emitter layouts in codegen/compile.rs
                // (register_trace_records): MemoryDump { fields: string,
                // count: i32 }; PanicPayload { error_type: string, file:
                // string, line: i32, stack: string }; SystemTrace
                // { last_state_name: string, unexpected_event: string,
                // snapshot: string, memory_dump: MemoryDump,
                // panic_payload: PanicPayload }. Per-function dispatch
                // (cross-emitter ABI) requires the exact same layouts.
                "builtin:type:SystemTrace"
                | "builtin:type:MemoryDump"
                | "builtin:type:PanicPayload"
                | "builtin:type:PeerFault" => {
                    let pointer =
                        BasicTypeEnum::PointerType(context.ptr_type(AddressSpace::default()));
                    let string_ty = BasicTypeEnum::StructType(context.struct_type(
                        &[pointer, BasicTypeEnum::IntType(context.i64_type())],
                        false,
                    ));
                    let i32_ty = BasicTypeEnum::IntType(context.i32_type());
                    match item.as_str() {
                        "builtin:type:MemoryDump" => Ok(BasicTypeEnum::StructType(
                            context.struct_type(&[string_ty, i32_ty], false),
                        )),
                        "builtin:type:PanicPayload" => Ok(BasicTypeEnum::StructType(
                            context.struct_type(&[string_ty, string_ty, i32_ty, string_ty], false),
                        )),
                        "builtin:type:PeerFault" => Ok(BasicTypeEnum::StructType(
                            context.struct_type(&[string_ty, string_ty], false),
                        )),
                        _ => {
                            let memory_dump_ty = BasicTypeEnum::StructType(
                                context.struct_type(&[string_ty, i32_ty], false),
                            );
                            let panic_payload_ty = BasicTypeEnum::StructType(
                                context
                                    .struct_type(&[string_ty, string_ty, i32_ty, string_ty], false),
                            );
                            Ok(BasicTypeEnum::StructType(context.struct_type(
                                &[
                                    string_ty,
                                    string_ty,
                                    string_ty,
                                    memory_dump_ty,
                                    panic_payload_ty,
                                ],
                                false,
                            )))
                        }
                    }
                }
                other => Err(CompileError::Unsupported(format!(
                    "nominal type '{other}' is not in the resolved native slice"
                ))),
            }
        }
        unsupported => Err(CompileError::Unsupported(format!(
            "canonical LLVM lowering does not support resolved type '{}' ({unsupported:?})",
            id.as_str()
        ))),
    };

    active.remove(id);
    lowered
}

fn lower_primitive<'ctx>(context: &'ctx Context, primitive: PrimitiveType) -> BasicTypeEnum<'ctx> {
    match primitive {
        PrimitiveType::I8 | PrimitiveType::U8 => BasicTypeEnum::IntType(context.i8_type()),
        PrimitiveType::I16 | PrimitiveType::U16 => BasicTypeEnum::IntType(context.i16_type()),
        PrimitiveType::I32 | PrimitiveType::U32 | PrimitiveType::Char => {
            BasicTypeEnum::IntType(context.i32_type())
        }
        PrimitiveType::I64 | PrimitiveType::U64 | PrimitiveType::Isize | PrimitiveType::Usize => {
            BasicTypeEnum::IntType(context.i64_type())
        }
        PrimitiveType::I128 | PrimitiveType::U128 => BasicTypeEnum::IntType(context.i128_type()),
        PrimitiveType::F32 => BasicTypeEnum::FloatType(context.f32_type()),
        PrimitiveType::F64 => BasicTypeEnum::FloatType(context.f64_type()),
        PrimitiveType::Bool => BasicTypeEnum::IntType(context.bool_type()),
        PrimitiveType::String => {
            // Native ABI: strings are {ptr, i64} structs matching the legacy
            // emitter's build_string_struct layout. This ensures ABI compatibility
            // for per-function dispatch (resolved + legacy in the same module).
            let pointer = BasicTypeEnum::PointerType(context.ptr_type(AddressSpace::default()));
            BasicTypeEnum::StructType(context.struct_type(
                &[pointer, BasicTypeEnum::IntType(context.i64_type())],
                false,
            ))
        }
        // Unit expressions use the established native sentinel representation
        // `i64 0`. This is an explicit ABI choice, not an unknown-type fallback.
        PrimitiveType::Unit => BasicTypeEnum::IntType(context.i64_type()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{NominalTypeId, ResolvedType};

    fn intern(types: &mut ResolvedTypeTable, ty: ResolvedType) -> ResolvedTypeId {
        types.intern_resolved(ty).expect("valid resolved type")
    }

    fn int_width(ty: BasicTypeEnum<'_>) -> u32 {
        match ty {
            BasicTypeEnum::IntType(integer) => integer.get_bit_width(),
            other => panic!("expected integer type, got {other:?}"),
        }
    }

    #[test]
    fn lowers_every_primitive_without_fallback() {
        let context = Context::create();
        let cases = [
            (PrimitiveType::I8, 8),
            (PrimitiveType::U8, 8),
            (PrimitiveType::I16, 16),
            (PrimitiveType::U16, 16),
            (PrimitiveType::I32, 32),
            (PrimitiveType::U32, 32),
            (PrimitiveType::I64, 64),
            (PrimitiveType::U64, 64),
            (PrimitiveType::I128, 128),
            (PrimitiveType::U128, 128),
            (PrimitiveType::Isize, 64),
            (PrimitiveType::Usize, 64),
            (PrimitiveType::Bool, 1),
            (PrimitiveType::Char, 32),
            (PrimitiveType::Unit, 64),
        ];

        for (primitive, width) in cases {
            let mut types = ResolvedTypeTable::new();
            let id = intern(&mut types, ResolvedType::Primitive(primitive));
            let lowered = llvm_type_for_resolved(&context, &types, &id).expect("primitive");
            assert_eq!(int_width(lowered), width, "{primitive:?}");
        }

        for (primitive, width) in [(PrimitiveType::F32, 32), (PrimitiveType::F64, 64)] {
            let mut types = ResolvedTypeTable::new();
            let id = intern(&mut types, ResolvedType::Primitive(primitive));
            let lowered = llvm_type_for_resolved(&context, &types, &id).expect("float");
            match lowered {
                BasicTypeEnum::FloatType(float) => assert_eq!(float.get_bit_width(), width),
                other => panic!("expected float type, got {other:?}"),
            }
        }

        let mut types = ResolvedTypeTable::new();
        let string = intern(&mut types, ResolvedType::Primitive(PrimitiveType::String));
        let lowered = llvm_type_for_resolved(&context, &types, &string).expect("string");
        let BasicTypeEnum::StructType(string_struct) = lowered else {
            panic!("string must be a {{ptr, i64}} struct, got {lowered:?}")
        };
        let fields = string_struct.get_field_types();
        assert_eq!(fields.len(), 2);
        assert!(matches!(fields[0], BasicTypeEnum::PointerType(_)));
        assert_eq!(int_width(fields[1]), 64);
    }

    #[test]
    fn recursively_lowers_option_result_and_tuple() {
        let context = Context::create();
        let mut types = ResolvedTypeTable::new();
        let i32_id = intern(&mut types, ResolvedType::Primitive(PrimitiveType::I32));
        let string_id = intern(&mut types, ResolvedType::Primitive(PrimitiveType::String));
        let tuple_id = intern(
            &mut types,
            ResolvedType::Tuple(vec![i32_id.clone(), string_id]),
        );
        let result_id = intern(
            &mut types,
            ResolvedType::Result {
                ok: tuple_id,
                error: i32_id,
            },
        );
        let option_id = intern(&mut types, ResolvedType::Option(result_id));

        let lowered = llvm_type_for_resolved(&context, &types, &option_id).expect("nested type");
        let BasicTypeEnum::StructType(option) = lowered else {
            panic!("option must be a struct")
        };
        let option_fields = option.get_field_types();
        assert_eq!(int_width(option_fields[0]), 1);
        let BasicTypeEnum::StructType(result) = option_fields[1] else {
            panic!("option payload must be Result")
        };
        let result_fields = result.get_field_types();
        assert_eq!(int_width(result_fields[0]), 1);
        // Error slot widened to i64 for legacy ABI compatibility
        // (mimi_type_to_llvm uses i64 for all non-bool int error payloads).
        assert_eq!(int_width(result_fields[2]), 64);
        let BasicTypeEnum::StructType(tuple) = result_fields[1] else {
            panic!("result payload must be tuple")
        };
        // Tuple fields widen sub-64-bit ints to i64 (legacy ABI compatibility).
        assert_eq!(int_width(tuple.get_field_types()[0]), 64);
    }

    #[test]
    fn distinguishes_mimi_closures_from_c_function_pointers() {
        let context = Context::create();
        let mut types = ResolvedTypeTable::new();
        let i32_id = intern(&mut types, ResolvedType::Primitive(PrimitiveType::I32));
        let mimi_id = intern(
            &mut types,
            ResolvedType::Function {
                abi: FunctionTypeAbi::Mimi,
                parameters: vec![i32_id.clone()],
                result: i32_id.clone(),
            },
        );
        let c_id = intern(
            &mut types,
            ResolvedType::Function {
                abi: FunctionTypeAbi::C,
                parameters: vec![i32_id.clone()],
                result: i32_id,
            },
        );

        let mimi = llvm_type_for_resolved(&context, &types, &mimi_id).expect("Mimi function");
        let BasicTypeEnum::StructType(closure) = mimi else {
            panic!("Mimi function must be a closure struct")
        };
        assert_eq!(closure.count_fields(), 2);
        assert!(closure
            .get_field_types()
            .iter()
            .all(|field| matches!(field, BasicTypeEnum::PointerType(_))));

        assert!(matches!(
            llvm_type_for_resolved(&context, &types, &c_id).expect("C function"),
            BasicTypeEnum::PointerType(_)
        ));
    }

    #[test]
    fn nominal_types_fail_closed_even_when_nested() {
        let context = Context::create();
        let mut types = ResolvedTypeTable::new();
        let nominal = intern(
            &mut types,
            ResolvedType::Nominal {
                item: NominalTypeId::new("type:User").expect("nominal identity"),
                arguments: Vec::new(),
                is_linear: false,
            },
        );
        let option = intern(&mut types, ResolvedType::Option(nominal.clone()));

        for id in [&nominal, &option] {
            let error = llvm_type_for_resolved(&context, &types, id)
                .expect_err("user-defined nominal type must not be erased");
            assert!(matches!(error, CompileError::Unsupported(_)));
            assert!(
                error.to_string().contains("nominal type"),
                "unexpected error: {error}"
            );
        }
    }
}
