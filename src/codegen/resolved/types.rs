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
    lower_resolved_type(context, types, id, &mut active)
}

fn lower_resolved_type<'ctx>(
    context: &'ctx Context,
    types: &ResolvedTypeTable,
    id: &ResolvedTypeId,
    active: &mut BTreeSet<ResolvedTypeId>,
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
            let payload = lower_resolved_type(context, types, payload, active)?;
            Ok(BasicTypeEnum::StructType(context.struct_type(
                &[BasicTypeEnum::IntType(context.bool_type()), payload],
                false,
            )))
        }
        ResolvedType::Result { ok, error } => {
            let ok = lower_resolved_type(context, types, ok, active)?;
            let error = lower_resolved_type(context, types, error, active)?;
            Ok(BasicTypeEnum::StructType(context.struct_type(
                &[BasicTypeEnum::IntType(context.bool_type()), ok, error],
                false,
            )))
        }
        ResolvedType::Tuple(elements) => {
            let elements = elements
                .iter()
                .map(|element| lower_resolved_type(context, types, element, active))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(BasicTypeEnum::StructType(
                context.struct_type(&elements, false),
            ))
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
                let _ = lower_resolved_type(context, types, parameter, active)?;
            }
            let _ = lower_resolved_type(context, types, result, active)?;

            let pointer = BasicTypeEnum::PointerType(context.ptr_type(AddressSpace::default()));
            match abi {
                FunctionTypeAbi::Mimi => Ok(BasicTypeEnum::StructType(
                    context.struct_type(&[pointer, pointer], false),
                )),
                FunctionTypeAbi::C => Ok(pointer),
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
            // Native ABI: strings are opaque C-style pointers (null-terminated).
            // The runtime (puts, printf, mimi_string_*) consumes raw ptr.
            BasicTypeEnum::PointerType(context.ptr_type(AddressSpace::default()))
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
        assert!(
            matches!(lowered, BasicTypeEnum::PointerType(_)),
            "string must be an opaque pointer, got {lowered:?}"
        );
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
        assert_eq!(int_width(result_fields[2]), 32);
        let BasicTypeEnum::StructType(tuple) = result_fields[1] else {
            panic!("result payload must be tuple")
        };
        assert_eq!(int_width(tuple.get_field_types()[0]), 32);
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
                .expect_err("nominal type must not be erased");
            assert!(matches!(error, CompileError::Unsupported(_)));
            assert!(error.to_string().contains("Nominal"));
        }
    }
}
