use crate::ast::{Field, Type};
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, StructType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};
use inkwell::AddressSpace;

/// Widen integer BasicTypeEnums to i64. Non-integer types pass through.
/// This ensures Result<i32,E> and Option<i32> use i64 for the payload slot,
/// matching the Ok/Some constructors which receive i64 literal values.
fn widen_int_to_i64<'ctx>(
    ctx: &'ctx Context,
    ty: BasicTypeEnum<'ctx>,
) -> BasicTypeEnum<'ctx> {
    match ty {
        BasicTypeEnum::IntType(it) => {
            if it.get_bit_width() == 64 {
                ty
            } else {
                BasicTypeEnum::IntType(ctx.i64_type())
            }
        }
        _ => ty,
    }
}

pub fn mimi_type_to_llvm<'ctx>(ctx: &'ctx Context, ty: &Type) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Type::Name(name, args) => match name.as_str() {
            "i32" => Some(BasicTypeEnum::IntType(ctx.i32_type())),
            "i64" => Some(BasicTypeEnum::IntType(ctx.i64_type())),
            "f64" => Some(BasicTypeEnum::FloatType(ctx.f64_type())),
            "bool" => Some(BasicTypeEnum::IntType(ctx.bool_type())),
            "string" => {
                let i8_ptr = ctx.ptr_type(AddressSpace::default());
                let i64 = ctx.i64_type();
                let fields = [
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::IntType(i64),
                ];
                Some(BasicTypeEnum::StructType(ctx.struct_type(&fields, false)))
            }
            "Result" if args.len() == 2 => {
                // Canonicalize to Type::Result so layout logic is single-source.
                mimi_type_to_llvm(
                    ctx,
                    &Type::Result(Box::new(args[0].clone()), Box::new(args[1].clone())),
                )
            }
            "Option" if args.len() == 1 => {
                // Canonicalize to Type::Option so layout logic is single-source.
                mimi_type_to_llvm(ctx, &Type::Option(Box::new(args[0].clone())))
            }
            "List" => {
                let i8_ptr = ctx.ptr_type(AddressSpace::default());
                let i64 = ctx.i64_type();
                Some(BasicTypeEnum::StructType(ctx.struct_type(
                    &[
                        BasicTypeEnum::IntType(i64),
                        BasicTypeEnum::PointerType(i8_ptr),
                    ],
                    false,
                )))
            }
            "unit" | "nothing" => None,
            _ => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        },
        Type::Ref(_, inner) | Type::RefMut(_, inner) => {
            let inner_llvm = mimi_type_to_llvm(ctx, inner)?;
            let ptr = match inner_llvm {
                BasicTypeEnum::IntType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::FloatType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::PointerType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::StructType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::ArrayType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                _ => BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
            };
            Some(ptr)
        }
        Type::Tuple(elems) => {
            let mut llvm_elems = Vec::new();
            for e in elems {
                llvm_elems.push(mimi_type_to_llvm(ctx, e)?);
            }
            Some(BasicTypeEnum::StructType(
                ctx.struct_type(&llvm_elems, false),
            ))
        }
        Type::Shared(_)
        | Type::LocalShared(_)
        | Type::Weak(_)
        | Type::WeakLocal(_)
        | Type::CShared(_)
        | Type::CBorrow(_)
        | Type::CBorrowMut(_)
        | Type::RawPtr(_)
        | Type::RawPtrMut(_) => Some(BasicTypeEnum::PointerType(
            ctx.ptr_type(AddressSpace::default()),
        )),
        Type::RawString => Some(BasicTypeEnum::PointerType(
            ctx.ptr_type(AddressSpace::default()),
        )),
        Type::Infer => None,
        Type::ExternFunc(_, _) => {
            // Function pointer - represented as void* in LLVM
            Some(BasicTypeEnum::PointerType(
                ctx.ptr_type(AddressSpace::default()),
            ))
        }
        Type::CBuffer(_) => {
            // CBuffer - represented as void* in LLVM
            Some(BasicTypeEnum::PointerType(
                ctx.ptr_type(AddressSpace::default()),
            ))
        }
        Type::Cap(_) => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        Type::Newtype(_, inner) => mimi_type_to_llvm(ctx, inner),
        Type::Allocator => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        Type::Array(inner, size) => {
            let elem = mimi_type_to_llvm(ctx, inner)?;
            match elem {
                BasicTypeEnum::IntType(t) => {
                    Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32)))
                }
                BasicTypeEnum::FloatType(t) => {
                    Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32)))
                }
                BasicTypeEnum::PointerType(t) => {
                    Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32)))
                }
                BasicTypeEnum::StructType(t) => {
                    Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32)))
                }
                BasicTypeEnum::ArrayType(t) => {
                    Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32)))
                }
                _ => Some(BasicTypeEnum::ArrayType(
                    ctx.i64_type().array_type(*size as u32),
                )),
            }
        }
        Type::Option(inner) => {
            // Option<T> represented as {i1, payload} — discriminant + payload.
            // Integer payloads are widened to i64 so that constructor (Some(42))
            // and method (unwrap) agree on the slot width. Non-integer payloads
            // (strings, structs) use their natural LLVM type.
            let inner_llvm = widen_int_to_i64(ctx, mimi_type_to_llvm(ctx, inner)?);
            let disc = BasicTypeEnum::IntType(ctx.bool_type());
            Some(BasicTypeEnum::StructType(
                ctx.struct_type(&[disc, inner_llvm], false),
            ))
        }
        Type::Result(ok, _err) => {
            // Result<T, E> represented as {i1, ok_payload, err_payload}.
            // Both payloads use i64 for integer types so that Ok(21) (literal
            // is i64) and the type map agree on slot width. Non-integer payloads
            // (strings, structs) use their natural LLVM type. Error payload is
            // always i64 (see compile_err_constructor).
            let ok_llvm = widen_int_to_i64(ctx, mimi_type_to_llvm(ctx, ok)?);
            let disc = BasicTypeEnum::IntType(ctx.bool_type());
            let err_llvm = BasicTypeEnum::IntType(ctx.i64_type());
            Some(BasicTypeEnum::StructType(
                ctx.struct_type(&[disc, ok_llvm, err_llvm], false),
            ))
        }
        Type::Func(_args, _ret) => {
            // Closures represented as {fn_ptr: i8*, env_ptr: i8*}
            Some(BasicTypeEnum::StructType(closure_struct_type(ctx)))
        }
        Type::Slice(inner) => {
            // Slice<T> represented as {ptr, len}
            let elem = mimi_type_to_llvm(ctx, inner)?;
            let ptr_ty = match elem {
                BasicTypeEnum::IntType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::FloatType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::PointerType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::StructType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::ArrayType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                _ => BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
            };
            let len = BasicTypeEnum::IntType(ctx.i64_type());
            Some(BasicTypeEnum::StructType(
                ctx.struct_type(&[ptr_ty, len], false),
            ))
        }
        Type::Nothing => None,
        Type::TypeVar(_) | Type::ForAll(_, _) => None,
        Type::ImplTrait(_) => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        Type::DynTrait(_) => {
            // Fat pointer: { data: i8*, vtable: i8* }
            let i8_ptr = ctx.ptr_type(inkwell::AddressSpace::default());
            Some(BasicTypeEnum::StructType(ctx.struct_type(
                &[
                    BasicTypeEnum::PointerType(i8_ptr),
                    BasicTypeEnum::PointerType(i8_ptr),
                ],
                false,
            )))
        }
    }
}

/// Build an LLVM function type from a return type and parameter metadata types.
pub fn build_fn_type_for<'ctx>(
    ctx: &'ctx Context,
    ret_type: BasicTypeEnum<'ctx>,
    param_types: &[BasicMetadataTypeEnum<'ctx>],
) -> inkwell::types::FunctionType<'ctx> {
    match ret_type {
        BasicTypeEnum::IntType(t) => t.fn_type(param_types, false),
        BasicTypeEnum::FloatType(t) => t.fn_type(param_types, false),
        BasicTypeEnum::PointerType(t) => t.fn_type(param_types, false),
        BasicTypeEnum::StructType(t) => t.fn_type(param_types, false),
        BasicTypeEnum::ArrayType(t) => t.fn_type(param_types, false),
        _ => ctx.i64_type().fn_type(param_types, false),
    }
}

pub fn basic_to_metadata<'ctx>(
    ctx: &'ctx Context,
    ty: BasicTypeEnum<'ctx>,
) -> BasicMetadataTypeEnum<'ctx> {
    match ty {
        BasicTypeEnum::IntType(t) => BasicMetadataTypeEnum::IntType(t),
        BasicTypeEnum::FloatType(t) => BasicMetadataTypeEnum::FloatType(t),
        BasicTypeEnum::PointerType(t) => BasicMetadataTypeEnum::PointerType(t),
        BasicTypeEnum::StructType(t) => BasicMetadataTypeEnum::StructType(t),
        BasicTypeEnum::ArrayType(t) => BasicMetadataTypeEnum::ArrayType(t),
        BasicTypeEnum::VectorType(_t) => BasicMetadataTypeEnum::IntType(ctx.i64_type()),
        BasicTypeEnum::ScalableVectorType(_) => BasicMetadataTypeEnum::IntType(ctx.i64_type()),
    }
}

/// Convert a BasicValueEnum to its metadata value for calls.
pub fn basic_value_to_metadata_value<'ctx>(
    val: &BasicValueEnum<'ctx>,
    i64_ty: inkwell::types::IntType<'ctx>,
) -> BasicMetadataValueEnum<'ctx> {
    match val {
        BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(*iv),
        BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(*fv),
        BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(*pv),
        BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(*sv),
        BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(*av),
        BasicValueEnum::VectorValue(vv) => BasicMetadataValueEnum::VectorValue(*vv),
        BasicValueEnum::ScalableVectorValue(_) => {
            BasicMetadataValueEnum::IntValue(i64_ty.const_int(0, false))
        }
    }
}

/// A `#[repr(C)]` record is "simple" if it fits in a single 64-bit integer
/// under the System V AMD64 ABI: all fields are `i32` and there are at most
/// two fields. Such records are passed/returned as a single `i64` in LLVM IR.
pub fn is_simple_reprc_record(fields: &[Field]) -> bool {
    if fields.len() > 2 {
        return false;
    }
    fields
        .iter()
        .all(|f| matches!(&f.ty, Type::Name(n, _) if n == "i32"))
}

/// Map a Mimi Type to LLVM for extern FFI (C ABI). i32 maps to LLVM i32 (int32_t)
/// instead of the internal i64, ensuring correct ABI compatibility with C functions.
pub fn mimi_type_to_llvm_extern<'ctx>(
    ctx: &'ctx Context,
    ty: &Type,
) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Type::Name(name, _args) => match name.as_str() {
            "i32" => Some(BasicTypeEnum::IntType(ctx.i32_type())),
            "bool" => Some(BasicTypeEnum::IntType(ctx.i8_type())),
            _ => mimi_type_to_llvm(ctx, ty),
        },
        // For extern FFI, references are just pointers (no struct wrapping)
        Type::Ref(_, inner) | Type::RefMut(_, inner) => {
            let inner_llvm = mimi_type_to_llvm(ctx, inner)?;
            let ptr = match inner_llvm {
                BasicTypeEnum::IntType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::FloatType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::PointerType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::StructType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                BasicTypeEnum::ArrayType(_) => {
                    BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default()))
                }
                _ => BasicTypeEnum::PointerType(ctx.ptr_type(AddressSpace::default())),
            };
            Some(ptr)
        }
        Type::Shared(_)
        | Type::LocalShared(_)
        | Type::Weak(_)
        | Type::WeakLocal(_)
        | Type::CShared(_)
        | Type::CBorrow(_)
        | Type::CBorrowMut(_)
        | Type::RawPtr(_)
        | Type::RawPtrMut(_) => Some(BasicTypeEnum::PointerType(
            ctx.ptr_type(AddressSpace::default()),
        )),
        _ => mimi_type_to_llvm(ctx, ty),
    }
}

/// Closure struct type: {fn_ptr: i8*, env_ptr: i8*}
pub fn closure_struct_type<'ctx>(ctx: &'ctx Context) -> StructType<'ctx> {
    let i8_ptr = ctx.ptr_type(AddressSpace::default());
    let fields = [
        BasicTypeEnum::PointerType(i8_ptr),
        BasicTypeEnum::PointerType(i8_ptr),
    ];
    ctx.struct_type(&fields, false)
}
