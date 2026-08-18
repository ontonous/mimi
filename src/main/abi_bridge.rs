//! Bridge from serialized `.mimiabi` to the historical AST-based bindgen
//! backends.
//!
//! This is the unified entry-point adapter: once a component has been
//! captured as Component IR / `.mimiabi`, all existing language backends can
//! consume it through this bridge without the caller reaching into source AST
//! directly. The long-term plan is to migrate each backend internally to
//! ComponentIR; until then this adapter keeps every language on the same
//! serialized ABI input.

use std::collections::HashMap;

use mimi::ast::{self, AstNodeMeta, AstOrigin};
use mimi::component::{MimiAbi, MimiAbiType, MimiAbiTypeRef};

fn meta(tag: &'static str) -> AstNodeMeta {
    AstNodeMeta::synthetic(AstOrigin::RuntimeSystem(tag))
}

fn type_ref_to_ast(ty: &MimiAbiTypeRef) -> ast::Type {
    match ty {
        MimiAbiTypeRef::Primitive(name) => {
            let mimi_name = match name.as_str() {
                "I8" => "i8",
                "U8" => "u8",
                "I16" => "i16",
                "U16" => "u16",
                "I32" => "i32",
                "U32" => "u32",
                "I64" => "i64",
                "U64" => "u64",
                "F32" => "f32",
                "F64" => "f64",
                "Bool" => "bool",
                "IntPtr" | "USize" | "ISize" => "i64",
                other => other,
            }
            .to_string();
            ast::Type::Name(mimi_name, Vec::new())
        }
        MimiAbiTypeRef::Named(name) => ast::Type::Name(name.clone(), Vec::new()),
        MimiAbiTypeRef::Pointer(inner) => ast::Type::RawPtrMut(Box::new(type_ref_to_ast(inner))),
        MimiAbiTypeRef::Slice(inner) => ast::Type::Slice(Box::new(type_ref_to_ast(inner))),
        MimiAbiTypeRef::Opaque(_) => ast::Type::Name("i64".to_string(), Vec::new()),
        MimiAbiTypeRef::FatPointer {
            element,
            has_capacity,
        } => {
            if *has_capacity {
                ast::Type::Name("string".to_string(), Vec::new())
            } else {
                ast::Type::Slice(Box::new(type_ref_to_ast(element)))
            }
        }
        MimiAbiTypeRef::Void => ast::Type::Name("void".to_string(), Vec::new()),
    }
}

/// Convert `.mimiabi` imports (and optionally exports) to `ast::ExternFunc`
/// records accepted by the historical bindgen backends.
pub(crate) fn to_extern_funcs(
    abi: &MimiAbi,
    include_exports: bool,
    tag: &'static str,
) -> Vec<ast::ExternFunc> {
    let mut out = Vec::new();
    for sym in &abi.imports {
        if sym.kind != "ExternFunction" {
            continue;
        }
        let mut func = ast::ExternFunc {
            meta: meta(tag),
            name: sym.name.clone(),
            params: Vec::new(),
            ret: if matches!(sym.ret, MimiAbiTypeRef::Void) {
                None
            } else {
                Some(type_ref_to_ast(&sym.ret))
            },
            requires: None,
            ensures: None,
            variadic: false,
            no_panic: sym.is_unsafe,
            returns_errno: false,
        };
        for param in &sym.params {
            func.params.push(ast::ExternParam {
                meta: meta(tag),
                name: param.name.clone(),
                ty: type_ref_to_ast(&param.ty),
                cap_mode: None,
            });
        }
        out.push(func);
    }
    if include_exports {
        for sym in &abi.exports {
            if sym.kind != "Function" && sym.kind != "ExternFunction" {
                continue;
            }
            let mut func = ast::ExternFunc {
                meta: meta(tag),
                name: sym.name.clone(),
                params: Vec::new(),
                ret: if matches!(sym.ret, MimiAbiTypeRef::Void) {
                    None
                } else {
                    Some(type_ref_to_ast(&sym.ret))
                },
                requires: None,
                ensures: None,
                variadic: false,
                no_panic: sym.is_unsafe,
                returns_errno: false,
            };
            for param in &sym.params {
                func.params.push(ast::ExternParam {
                    meta: meta(tag),
                    name: param.name.clone(),
                    ty: type_ref_to_ast(&param.ty),
                    cap_mode: None,
                });
            }
            out.push(func);
        }
    }
    out
}

/// Convert `.mimiabi` type definitions to the AST `TypeDef` map expected by
/// historical bindgen backends.
pub(crate) fn to_type_defs(abi: &MimiAbi, tag: &'static str) -> HashMap<String, ast::TypeDef> {
    let mut out = HashMap::new();
    for ty in &abi.types {
        let def = match ty {
            MimiAbiType::Struct { name, fields, .. } => {
                let record_fields = fields
                    .iter()
                    .map(|f| ast::Field {
                        meta: meta(tag),
                        name: f.name.clone(),
                        ty: type_ref_to_ast(&f.ty),
                    })
                    .collect();
                ast::TypeDef {
                    meta: meta(tag),
                    name: name.clone(),
                    pub_: true,
                    kind: ast::TypeDefKind::Record(record_fields),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: vec![ast::TypeAttribute::ReprC],
                }
            }
            MimiAbiType::Enum { name, variants, .. } => {
                let ast_variants = variants
                    .iter()
                    .map(|(variant, _): &(String, i64)| ast::Variant {
                        meta: meta(tag),
                        name: variant.clone(),
                        payload: None,
                    })
                    .collect();
                ast::TypeDef {
                    meta: meta(tag),
                    name: name.clone(),
                    pub_: true,
                    kind: ast::TypeDefKind::Enum(ast_variants),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: vec![ast::TypeAttribute::ReprC],
                }
            }
            MimiAbiType::Alias { name, target } => ast::TypeDef {
                meta: meta(tag),
                name: name.clone(),
                pub_: true,
                kind: ast::TypeDefKind::Alias(type_ref_to_ast(target)),
                generics: Vec::new(),
                derives: Vec::new(),
                attributes: Vec::new(),
            },
            MimiAbiType::Opaque { name, .. } => {
                // Opaque handles do not have a visible C layout. The historical
                // generators need a concrete scalar type; preserve the handle as
                // an i64 alias so generated code can still pass it around.
                ast::TypeDef {
                    meta: meta(tag),
                    name: name.clone(),
                    pub_: true,
                    kind: ast::TypeDefKind::Alias(ast::Type::Name("i64".to_string(), Vec::new())),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: Vec::new(),
                }
            }
        };
        out.insert(def.name.clone(), def);
    }
    out
}
