use crate::ast::Type;
use inkwell::context::Context;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::AddressSpace;

pub fn mimi_type_to_llvm<'ctx>(ctx: &'ctx Context, ty: &Type) -> Option<BasicTypeEnum<'ctx>> {
    match ty {
        Type::Name(name, _) => match name.as_str() {
            "i32" | "i64" => Some(BasicTypeEnum::IntType(ctx.i64_type())),
            "f64" => Some(BasicTypeEnum::FloatType(ctx.f64_type())),
            "bool" => Some(BasicTypeEnum::IntType(ctx.bool_type())),
            "string" => {
                let i8_ptr = ctx.i8_type().ptr_type(AddressSpace::default());
                let i64 = ctx.i64_type();
                let fields = [BasicTypeEnum::PointerType(i8_ptr), BasicTypeEnum::IntType(i64)];
                Some(BasicTypeEnum::StructType(ctx.struct_type(&fields, false)))
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
        Type::Shared(_) | Type::LocalShared(_) | Type::Weak(_)
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
            // Result<T, E> represented as {i1, T} — discriminant + ok payload (err ignored in codegen for now)
            let ok_llvm = mimi_type_to_llvm(ctx, ok)?;
            let _err_llvm = mimi_type_to_llvm(ctx, err);
            let disc = BasicTypeEnum::IntType(ctx.bool_type());
            Some(BasicTypeEnum::StructType(ctx.struct_type(&[disc, ok_llvm], false)))
        }
        Type::Func(args, ret) => {
            // Function pointers represented as i64 (opaque)
            let _ = (args, ret);
            Some(BasicTypeEnum::IntType(ctx.i64_type()))
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
        Type::DynTrait(_) => Some(BasicTypeEnum::IntType(ctx.i64_type())),
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
