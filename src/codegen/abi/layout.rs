//! Single-source native ABI slot widening.

use inkwell::context::Context;
use inkwell::types::{BasicTypeEnum, StructType};

/// Widen an integer used as an Option/Result payload slot to i64.
///
/// Container payloads historically widen every sub-64-bit integer, including
/// `bool`.  Keeping that rule here preserves the cross-emitter ABI while
/// removing the copies that used to live in three lowering paths.
pub(in crate::codegen) fn widen_container_payload<'ctx>(
    context: &'ctx Context,
    ty: BasicTypeEnum<'ctx>,
) -> BasicTypeEnum<'ctx> {
    match ty {
        BasicTypeEnum::IntType(integer) if integer.get_bit_width() != 64 => {
            BasicTypeEnum::IntType(context.i64_type())
        }
        other => other,
    }
}

/// Widen an integer stored in a tuple/product field to i64, preserving i1.
///
/// Product tuples keep `bool` as i1 but widen every other narrow integer so
/// literal construction and both native emitters use one layout.
pub(in crate::codegen) fn widen_product_field<'ctx>(
    context: &'ctx Context,
    ty: BasicTypeEnum<'ctx>,
) -> BasicTypeEnum<'ctx> {
    match ty {
        BasicTypeEnum::IntType(integer)
            if integer.get_bit_width() > 1 && integer.get_bit_width() < 64 =>
        {
            BasicTypeEnum::IntType(context.i64_type())
        }
        other => other,
    }
}

/// Apply the product-field rule to every field of an LLVM struct.
pub(in crate::codegen) fn widen_product_fields<'ctx>(
    context: &'ctx Context,
    ty: StructType<'ctx>,
) -> StructType<'ctx> {
    let fields = ty
        .get_field_types()
        .into_iter()
        .map(|field| widen_product_field(context, field))
        .collect::<Vec<_>>();
    context.struct_type(&fields, ty.is_packed())
}

/// Lower a surface Option/Result payload into its canonical ABI slot.
///
/// Named records retain their declared field widths. Product tuples use the
/// product-field rule; scalar integer payloads use the container rule.
pub(in crate::codegen) fn widen_surface_container_payload<'ctx>(
    context: &'ctx Context,
    surface: &crate::ast::Type,
    lowered: BasicTypeEnum<'ctx>,
) -> BasicTypeEnum<'ctx> {
    match (surface.unlocated(), lowered) {
        (crate::ast::Type::Tuple(_), BasicTypeEnum::StructType(product)) => {
            BasicTypeEnum::StructType(widen_product_fields(context, product))
        }
        (_, other) => widen_container_payload(context, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_and_product_integer_rules_are_distinct_and_stable() {
        let context = Context::create();
        assert_eq!(
            widen_container_payload(&context, context.bool_type().into()),
            BasicTypeEnum::IntType(context.i64_type())
        );
        assert_eq!(
            widen_product_field(&context, context.bool_type().into()),
            BasicTypeEnum::IntType(context.bool_type())
        );
        assert_eq!(
            widen_product_field(&context, context.i32_type().into()),
            BasicTypeEnum::IntType(context.i64_type())
        );
    }
}
