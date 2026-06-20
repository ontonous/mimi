use crate::ast::Type;
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::AddressSpace;

pub fn mimi_type_to_llvm<'ctx>(ctx: &'ctx Context, ty: &Type) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Type::Name(name, args) => match name.as_str() {
            "i32" | "i64" => Some(BasicTypeEnum::IntType(ctx.i64_type())),
            "f64" => Some(BasicTypeEnum::FloatType(ctx.f64_type())),
            "bool" => Some(BasicTypeEnum::IntType(ctx.bool_type())),
            "string" => {
                let i8_ptr = ctx.i8_type().ptr_type(AddressSpace::default());
                let i64 = ctx.i64_type();
                let fields = [BasicTypeEnum::PointerType(i8_ptr), BasicTypeEnum::IntType(i64)];
                Some(BasicTypeEnum::StructType(ctx.struct_type(&fields, false)))
            }
            "Result" if args.len() == 2 => {
                let ok = mimi_type_to_llvm(ctx, &args[0])?;
                let disc = BasicTypeEnum::IntType(ctx.bool_type());
                let err = BasicTypeEnum::IntType(ctx.i64_type());
                Some(BasicTypeEnum::StructType(ctx.struct_type(&[disc, ok, err], false)))
            }
            "Option" if args.len() == 1 => {
                let inner = mimi_type_to_llvm(ctx, &args[0])?;
                let disc = BasicTypeEnum::IntType(ctx.bool_type());
                Some(BasicTypeEnum::StructType(ctx.struct_type(&[disc, inner], false)))
            }
            "unit" | "nothing" => None,
            _ => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        },
        Type::Ref(_, inner) | Type::RefMut(_, inner) => {
            let inner_llvm = mimi_type_to_llvm(ctx, inner)?;
            let ptr = match inner_llvm {
                BasicTypeEnum::IntType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::FloatType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::PointerType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::StructType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::ArrayType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                _ => BasicTypeEnum::PointerType(ctx.i8_type().ptr_type(AddressSpace::default())),
            };
            Some(ptr)
        }
        Type::Tuple(elems) => {
            let mut llvm_elems = Vec::new();
            for e in elems {
                llvm_elems.push(mimi_type_to_llvm(ctx, e)?);
            }
            Some(BasicTypeEnum::StructType(ctx.struct_type(&llvm_elems, false)))
        }
        Type::Shared(_) | Type::LocalShared(_) | Type::Weak(_) | Type::WeakLocal(_)
            | Type::CShared(_) | Type::CBorrow(_) | Type::CBorrowMut(_)
            | Type::RawPtr(_) | Type::RawPtrMut(_) =>
            Some(BasicTypeEnum::PointerType(ctx.i8_type().ptr_type(AddressSpace::default()))),
        Type::RawString => {
            Some(BasicTypeEnum::PointerType(ctx.i8_type().ptr_type(AddressSpace::default())))
        }
        Type::Infer => None,
        Type::ExternFunc(_, _) => {
            // Function pointer - represented as void* in LLVM
            Some(BasicTypeEnum::PointerType(ctx.i8_type().ptr_type(AddressSpace::default())))
        }
        Type::CBuffer(_) => {
            // CBuffer - represented as void* in LLVM
            Some(BasicTypeEnum::PointerType(ctx.i8_type().ptr_type(AddressSpace::default())))
        }
        Type::Cap(_) => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        Type::Newtype(_, inner) => mimi_type_to_llvm(ctx, inner),
        Type::Allocator => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        Type::Array(inner, size) => {
            let elem = mimi_type_to_llvm(ctx, inner)?;
            match elem {
                BasicTypeEnum::IntType(t) => Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32))),
                BasicTypeEnum::FloatType(t) => Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32))),
                BasicTypeEnum::PointerType(t) => Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32))),
                BasicTypeEnum::StructType(t) => Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32))),
                BasicTypeEnum::ArrayType(t) => Some(BasicTypeEnum::ArrayType(t.array_type(*size as u32))),
                _ => Some(BasicTypeEnum::ArrayType(ctx.i64_type().array_type(*size as u32))),
            }
        }
        Type::Option(inner) => {
            // Option<T> represented as {i1, T} — discriminant + payload
            let inner_llvm = mimi_type_to_llvm(ctx, inner)?;
            let disc = BasicTypeEnum::IntType(ctx.bool_type());
            Some(BasicTypeEnum::StructType(ctx.struct_type(&[disc, inner_llvm], false)))
        }
        Type::Result(ok, err) => {
            // Result<T, E> represented as {i1, T, i64} — discriminant + ok payload + err payload (as i64).
            // The error field uses i64 to keep the struct layout consistent across all E types.
            // Integer values are sign-extended from their native width; pointer values use ptrtoint.
            let ok_llvm = mimi_type_to_llvm(ctx, ok)?;
            let disc = BasicTypeEnum::IntType(ctx.bool_type());
            let err_llvm = BasicTypeEnum::IntType(ctx.i64_type());
            Some(BasicTypeEnum::StructType(ctx.struct_type(&[disc, ok_llvm, err_llvm], false)))
        }
        Type::Func(_args, _ret) => {
            // Closures represented as {fn_ptr: i8*, env_ptr: i8*}
            Some(BasicTypeEnum::StructType(closure_struct_type(ctx)))
        }
        Type::Slice(inner) => {
            // Slice<T> represented as {ptr, len}
            let elem = mimi_type_to_llvm(ctx, inner)?;
            let ptr_ty = match elem {
                BasicTypeEnum::IntType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::FloatType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::PointerType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::StructType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                BasicTypeEnum::ArrayType(t) => BasicTypeEnum::PointerType(t.ptr_type(AddressSpace::default())),
                _ => BasicTypeEnum::PointerType(ctx.i8_type().ptr_type(AddressSpace::default())),
            };
            let len = BasicTypeEnum::IntType(ctx.i64_type());
            Some(BasicTypeEnum::StructType(ctx.struct_type(&[ptr_ty, len], false)))
        }
        Type::Nothing => None,
        Type::ImplTrait(_) => Some(BasicTypeEnum::IntType(ctx.i64_type())),
        Type::DynTrait(_) => {
            // Fat pointer: { data: i8*, vtable: i8* }
            let i8_ptr = ctx.i8_type().ptr_type(inkwell::AddressSpace::default());
            Some(BasicTypeEnum::StructType(ctx.struct_type(&[
                BasicTypeEnum::PointerType(i8_ptr),
                BasicTypeEnum::PointerType(i8_ptr),
            ], false)))
        }
    }
}

pub fn basic_to_metadata<'ctx>(ctx: &'ctx Context, ty: BasicTypeEnum<'ctx>) -> BasicMetadataTypeEnum<'ctx> {
    match ty {
        BasicTypeEnum::IntType(t) => BasicMetadataTypeEnum::IntType(t),
        BasicTypeEnum::FloatType(t) => BasicMetadataTypeEnum::FloatType(t),
        BasicTypeEnum::PointerType(t) => BasicMetadataTypeEnum::PointerType(t),
        BasicTypeEnum::StructType(t) => BasicMetadataTypeEnum::StructType(t),
        BasicTypeEnum::ArrayType(t) => BasicMetadataTypeEnum::ArrayType(t),
        _ => BasicMetadataTypeEnum::IntType(ctx.i64_type()),
    }
}

/// Closure struct type: {fn_ptr: i8*, env_ptr: i8*}
pub fn closure_struct_type<'ctx>(ctx: &'ctx Context) -> StructType<'ctx> {
    let i8_ptr = ctx.i8_type().ptr_type(AddressSpace::default());
    let fields = [
        BasicTypeEnum::PointerType(i8_ptr),
        BasicTypeEnum::PointerType(i8_ptr),
    ];
    ctx.struct_type(&fields, false)
}
