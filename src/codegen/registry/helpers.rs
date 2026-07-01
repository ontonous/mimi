#![allow(dead_code, deprecated)]

use crate::ast::Type;

/// Map a Mimi Type to JSON serialization element type tag:
/// 0 = i64/i32 (integer), 1 = f64 (float), 2 = string
pub(in crate::codegen) fn elem_type_tag(ty: &Type) -> u32 {
    match ty {
        Type::Name(n, _) if n == "f64" => 1,
        Type::Name(n, _) if n == "string" => 2,
        Type::Name(_, params) if !params.is_empty() => elem_type_tag(&params[0]),
        _ => 0,
    }
}
