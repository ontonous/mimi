use crate::ast::*;
use std::collections::HashMap;

/// Compute the Levenshtein edit distance between two strings.
#[allow(clippy::needless_range_loop)]
fn edit_distance(a: &str, b: &str) -> usize {
    let a_len = a.len();
    let b_len = b.len();
    let mut matrix = vec![vec![0usize; b_len + 1]; a_len + 1];

    for i in 0..=a_len {
        matrix[i][0] = i;
    }
    for j in 0..=b_len {
        matrix[0][j] = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a.as_bytes()[i - 1] == b.as_bytes()[j - 1] { 0 } else { 1 };
            matrix[i][j] = std::cmp::min(
                std::cmp::min(
                    matrix[i - 1][j] + 1,      // deletion
                    matrix[i][j - 1] + 1,      // insertion
                ),
                matrix[i - 1][j - 1] + cost,  // substitution
            );
        }
    }

    matrix[a_len][b_len]
}

/// Find the closest matching name from a list of candidates.
/// Returns the best match if its edit distance is <= max_distance.
pub(crate) fn suggest_name(name: &str, candidates: &[String], max_distance: usize) -> Option<String> {
    let mut best: Option<(String, usize)> = None;
    for candidate in candidates {
        let dist = edit_distance(name, candidate);
        if dist <= max_distance && dist > 0 {
            match &best {
                Some((_, best_dist)) if dist < *best_dist => {
                    best = Some((candidate.clone(), dist));
                }
                None => {
                    best = Some((candidate.clone(), dist));
                }
                _ => {}
            }
        }
    }
    best.map(|(name, _)| name)
}

/// Check if a type name is a generic type parameter
pub fn is_type_param(name: &str, generics: &[GenericParam]) -> bool {
    generics.iter().any(|g| g.name == name)
}

/// Check if a type name appears within a type (occurs check).
/// Prevents infinite recursion from self-referential type substitutions.
fn occurs_check(name: &str, ty: &Type, generics: &[GenericParam]) -> bool {
    match ty {
        Type::Name(n, args) => {
            if n == name { return true; }
            args.iter().any(|a| occurs_check(name, a, generics))
        }
        Type::Ref(_, inner) => occurs_check(name, inner, generics),
        Type::RefMut(_, inner) => occurs_check(name, inner, generics),
        Type::Option(inner) => occurs_check(name, inner, generics),
        Type::Result(ok, err) => occurs_check(name, ok, generics) || occurs_check(name, err, generics),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_check(name, e, generics)),
        Type::Func(args, ret) => args.iter().any(|a| occurs_check(name, a, generics)) || occurs_check(name, ret, generics),
        Type::Shared(inner) => occurs_check(name, inner, generics),
        Type::LocalShared(inner) => occurs_check(name, inner, generics),
        Type::Weak(inner) => occurs_check(name, inner, generics),
        Type::WeakLocal(inner) => occurs_check(name, inner, generics),
        Type::RawPtr(inner) => occurs_check(name, inner, generics),
        Type::RawPtrMut(inner) => occurs_check(name, inner, generics),
        Type::CShared(inner) => occurs_check(name, inner, generics),
        Type::CBorrow(inner) => occurs_check(name, inner, generics),
        Type::CBorrowMut(inner) => occurs_check(name, inner, generics),
        Type::Newtype(_, inner) => occurs_check(name, inner, generics),
        Type::ExternFunc(args, ret) => args.iter().any(|a| occurs_check(name, a, generics)) || occurs_check(name, ret, generics),
        Type::CBuffer(inner) => occurs_check(name, inner, generics),
        Type::Array(inner, _) => occurs_check(name, inner, generics),
        Type::Slice(inner) => occurs_check(name, inner, generics),
        Type::Cap(_) | Type::Nothing | Type::RawString | Type::Allocator | Type::Infer
        | Type::ImplTrait(_) | Type::DynTrait(_) => false,
    }
}

/// Substitute type parameters in a type.
/// If substitution would cause infinite recursion (self-referential type),
/// returns the original type unchanged to let downstream checks catch the mismatch.
pub fn subst_type_params(ty: &Type, generics: &[GenericParam], type_map: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Name(name, args) => {
            if is_type_param(name, generics) {
                if let Some(concrete) = type_map.get(name) {
                    // Occurs check: if concrete type references this parameter,
                    // return original to prevent infinite recursion.
                    if occurs_check(name, concrete, generics) {
                        ty.clone()
                    } else {
                        concrete.clone()
                    }
                } else {
                    ty.clone()
                }
            } else {
                let new_args: Vec<Type> = args.iter()
                    .map(|a| subst_type_params(a, generics, type_map))
                    .collect();
                Type::Name(name.clone(), new_args)
            }
        }
        Type::Ref(_, inner) => Type::Ref(None, Box::new(subst_type_params(inner, generics, type_map))),
        Type::RefMut(_, inner) => Type::RefMut(None, Box::new(subst_type_params(inner, generics, type_map))),
        Type::Option(inner) => Type::Option(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Result(ok, err) => Type::Result(
            Box::new(subst_type_params(ok, generics, type_map)),
            Box::new(subst_type_params(err, generics, type_map)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems.iter().map(|e| subst_type_params(e, generics, type_map)).collect(),
        ),
        Type::Func(args, ret) => Type::Func(
            args.iter().map(|a| subst_type_params(a, generics, type_map)).collect(),
            Box::new(subst_type_params(ret, generics, type_map)),
        ),
        Type::Shared(inner) => Type::Shared(Box::new(subst_type_params(inner, generics, type_map))),
        Type::LocalShared(inner) => Type::LocalShared(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Weak(inner) => Type::Weak(Box::new(subst_type_params(inner, generics, type_map))),
        Type::WeakLocal(inner) => Type::WeakLocal(Box::new(subst_type_params(inner, generics, type_map))),
        Type::RawPtr(inner) => Type::RawPtr(Box::new(subst_type_params(inner, generics, type_map))),
        Type::RawPtrMut(inner) => Type::RawPtrMut(Box::new(subst_type_params(inner, generics, type_map))),
        Type::CShared(inner) => Type::CShared(Box::new(subst_type_params(inner, generics, type_map))),
        Type::CBorrow(inner) => Type::CBorrow(Box::new(subst_type_params(inner, generics, type_map))),
        Type::CBorrowMut(inner) => Type::CBorrowMut(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Newtype(name, inner) => Type::Newtype(name.clone(), Box::new(subst_type_params(inner, generics, type_map))),
        Type::Cap(_) | Type::Nothing | Type::RawString | Type::Allocator | Type::Infer => ty.clone(),
        Type::ExternFunc(args, ret) => Type::ExternFunc(
            args.iter().map(|a| subst_type_params(a, generics, type_map)).collect(),
            Box::new(subst_type_params(ret, generics, type_map)),
        ),
        Type::CBuffer(inner) => Type::CBuffer(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Array(inner, size) => Type::Array(Box::new(subst_type_params(inner, generics, type_map)), *size),
        Type::Slice(inner) => Type::Slice(Box::new(subst_type_params(inner, generics, type_map))),
        Type::ImplTrait(traits) => Type::ImplTrait(traits.clone()),
            Type::DynTrait(traits) => Type::DynTrait(traits.clone()),
    }
}

pub(crate) fn same_type(a: &Type, b: &Type) -> bool {
    // Only treat 'unknown' as matching if BOTH sides are unknown.
    // Single-sided unknown would mask cascade errors — let the
    // real type propagate so subsequent checks detect mismatches.
    if matches!(a, Type::Name(n, _) if n == "unknown") && matches!(b, Type::Name(n, _) if n == "unknown") {
        return true;
    }
    // Normalize Type::Name("Result", [T, E]) <-> Type::Result(T, E) and Type::Name("Option", [T]) <-> Type::Option(T)
    // Compare args directly without cloning to allocate new enum variants.
    match (a, b) {
        (Type::Name(na, aa), Type::Name(nb, ab)) => na == nb && aa.len() == ab.len() && aa.iter().zip(ab.iter()).all(|(x, y)| same_type(x, y)),
        (Type::Name(n, args), Type::Result(ok, err)) if n == "Result" && args.len() == 2 => {
            same_type(&args[0], ok) && same_type(&args[1], err)
        }
        (Type::Result(ok, err), Type::Name(n, args)) if n == "Result" && args.len() == 2 => {
            same_type(ok, &args[0]) && same_type(err, &args[1])
        }
        (Type::Name(n, args), Type::Option(inner)) if n == "Option" && args.len() == 1 => {
            same_type(&args[0], inner)
        }
        (Type::Option(inner), Type::Name(n, args)) if n == "Option" && args.len() == 1 => {
            same_type(inner, &args[0])
        }
        (Type::Ref(_, a), Type::Ref(_, b)) => same_type(a, b),
        (Type::RefMut(_, a), Type::RefMut(_, b)) => same_type(a, b),
        (Type::Option(a), Type::Option(b)) => same_type(a, b),
        (Type::Result(a1, a2), Type::Result(b1, b2)) => same_type(a1, b1) && same_type(a2, b2),
        (Type::Tuple(a), Type::Tuple(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_type(x, y)),
        (Type::Func(a_args, a_ret), Type::Func(b_args, b_ret)) => {
            a_args.len() == b_args.len()
                && a_args.iter().zip(b_args.iter()).all(|(x, y)| same_type(x, y))
                && same_type(a_ret, b_ret)
        }
        (Type::Cap(a), Type::Cap(b)) => a == b,
        (Type::Shared(a), Type::Shared(b)) => same_type(a, b),
        (Type::LocalShared(a), Type::LocalShared(b)) => same_type(a, b),
        (Type::Weak(a), Type::Weak(b)) => same_type(a, b),
        (Type::WeakLocal(a), Type::WeakLocal(b)) => same_type(a, b),
        // Newtypes with same name and same inner type are equal
        (Type::Newtype(n1, a), Type::Newtype(n2, b)) => n1 == n2 && same_type(a, b),
        // A named type matches a newtype with the same inner type name
        (Type::Name(n, _), Type::Newtype(n2, _)) | (Type::Newtype(n2, _), Type::Name(n, _)) => {
            n == n2
        }
        (Type::Allocator, Type::Allocator) => true,
        (Type::Infer, Type::Infer) => true,
        (Type::Array(a_inner, a_size), Type::Array(b_inner, b_size)) => {
            a_size == b_size && same_type(a_inner, b_inner)
        }
        (Type::Slice(a), Type::Slice(b)) => same_type(a, b),
        (Type::ImplTrait(a), Type::ImplTrait(b)) => a == b,
        (Type::DynTrait(a), Type::DynTrait(b)) => a == b,
        (Type::Nothing, Type::Nothing) => true,
        (Type::RawString, Type::RawString) => true,
        (Type::ExternFunc(a_args, a_ret), Type::ExternFunc(b_args, b_ret)) => {
            a_args.len() == b_args.len()
                && a_args.iter().zip(b_args.iter()).all(|(x, y)| same_type(x, y))
                && same_type(a_ret, b_ret)
        }
        (Type::CBuffer(a), Type::CBuffer(b)) => same_type(a, b),
        (Type::RawPtr(a), Type::RawPtr(b)) => same_type(a, b),
        (Type::RawPtrMut(a), Type::RawPtrMut(b)) => same_type(a, b),
        (Type::CShared(a), Type::CShared(b)) => same_type(a, b),
        (Type::CBorrow(a), Type::CBorrow(b)) => same_type(a, b),
        (Type::CBorrowMut(a), Type::CBorrowMut(b)) => same_type(a, b),
        _ => false,
    }
}

/// Check if a concrete type can be coerced to a dyn Trait type (e.g., Circle → dyn Drawable)
/// `impls` maps (trait_name, type_name) -> method_names
/// Check for numeric type widening (i32→i64, i32→f64, i64→f64).
/// Integer literals default to i32; this allows them to flow into i64/i32/f64 parameters.
pub(crate) fn is_numeric_coercion(declared: &Type, init_ty: &Type) -> bool {
    match (declared, init_ty) {
        (Type::Name(dn, _), Type::Name(in_n, _)) => {
            let (d, i) = (dn.as_str(), in_n.as_str());
            matches!((d, i), ("i64", "i32") | ("f64", "i32") | ("f64", "i64"))
        }
        _ => false,
    }
}

pub(crate) fn is_trait_coercion(declared: &Type, init_ty: &Type, impls: &HashMap<(String, String), Vec<String>>) -> bool {
    match (declared, init_ty) {
        (Type::DynTrait(trait_names), Type::Name(ty_name, _)) => {
            trait_names.iter().all(|trait_name| {
                impls.contains_key(&(trait_name.clone(), ty_name.clone()))
            })
        }
        _ => false,
    }
}

pub(crate) fn is_int(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64")
}

pub(crate) fn is_numeric(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64" || n == "f64")
}

pub(crate) fn is_bool(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "bool")
}

pub(crate) fn is_string(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "string")
}

pub fn fmt_type(t: &Type) -> String {
    match t {
        Type::Name(n, args) if args.is_empty() => n.clone(),
        Type::Name(n, args) => format!("{}<{}>", n, args.iter().map(fmt_type).collect::<Vec<_>>().join(", ")),
        Type::Ref(lt, inner) => {
            if let Some(l) = lt { format!("&'{} {}", l, fmt_type(inner)) } else { format!("&{}", fmt_type(inner)) }
        }
        Type::RefMut(lt, inner) => {
            if let Some(l) = lt { format!("&'{} mut {}", l, fmt_type(inner)) } else { format!("&mut {}", fmt_type(inner)) }
        }
        Type::Option(inner) => format!("Option<{}>", fmt_type(inner)),
        Type::Result(ok, err) => format!("Result<{}, {}>", fmt_type(ok), fmt_type(err)),
        Type::Tuple(elems) => format!("({})", elems.iter().map(fmt_type).collect::<Vec<_>>().join(", ")),
        Type::Func(args, ret) => format!(
            "fn({}) -> {}",
            args.iter().map(fmt_type).collect::<Vec<_>>().join(", "),
            fmt_type(ret)
        ),
        Type::Cap(name) => format!("cap {}", name),
        Type::Shared(inner) => format!("shared {}", fmt_type(inner)),
        Type::LocalShared(inner) => format!("local_shared {}", fmt_type(inner)),
        Type::Weak(inner) => format!("weak {}", fmt_type(inner)),
        Type::WeakLocal(inner) => format!("weak_local {}", fmt_type(inner)),
        Type::Newtype(name, inner) => format!("newtype {} {}", name, fmt_type(inner)),
        Type::Nothing => "nothing".to_string(),
        Type::Allocator => "Allocator".to_string(),
        Type::Array(inner, size) => format!("[{}; {}]", fmt_type(inner), size),
        Type::Slice(inner) => format!("[{}]", fmt_type(inner)),
        Type::ImplTrait(traits) => format!("impl {}", traits.join(" + ")),
        Type::DynTrait(traits) => format!("dyn {}", traits.join(" + ")),
        Type::RawPtr(inner) => format!("*{}", fmt_type(inner)),
        Type::RawPtrMut(inner) => format!("*mut {}", fmt_type(inner)),
        Type::CShared(inner) => format!("c_shared {}", fmt_type(inner)),
        Type::CBorrow(inner) => format!("c_borrow {}", fmt_type(inner)),
        Type::CBorrowMut(inner) => format!("c_borrow_mut {}", fmt_type(inner)),
        Type::RawString => "raw_string".to_string(),
        Type::Infer => "_".to_string(),
        Type::ExternFunc(args, ret) => {
            let args_str: Vec<String> = args.iter().map(fmt_type).collect();
            format!("extern \"C\" fn({}) -> {}", args_str.join(", "), fmt_type(ret))
        }
        Type::CBuffer(inner) => format!("CBuffer<{}>", fmt_type(inner)),
    }
}
