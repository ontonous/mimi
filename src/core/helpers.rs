use crate::ast::*;
use std::collections::HashMap;

/// Compute the Levenshtein edit distance between two strings.
/// Uses grapheme clusters (char-level) for correct non-ASCII comparison.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();
    let mut matrix = vec![vec![0usize; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate().take(a_len + 1) {
        row[0] = i;
    }
    for (j, cell) in matrix[0].iter_mut().enumerate().take(b_len + 1) {
        *cell = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = std::cmp::min(
                std::cmp::min(
                    matrix[i - 1][j] + 1, // deletion
                    matrix[i][j - 1] + 1, // insertion
                ),
                matrix[i - 1][j - 1] + cost, // substitution
            );
        }
    }

    matrix[a_len][b_len]
}

/// Find the closest matching name from a list of candidates.
/// Returns the best match if its edit distance is <= max_distance.
pub(crate) fn suggest_name(
    name: &str,
    candidates: &[String],
    max_distance: usize,
) -> Option<String> {
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
fn occurs_check(name: &str, ty: &Type, _generics: &[GenericParam]) -> bool {
    match ty {
        Type::Name(n, args) => {
            if n == name {
                return true;
            }
            args.iter().any(|a| occurs_check(name, a, _generics))
        }
        Type::Ref(_, inner) => occurs_check(name, inner, _generics),
        Type::RefMut(_, inner) => occurs_check(name, inner, _generics),
        Type::Option(inner) => occurs_check(name, inner, _generics),
        Type::Result(ok, err) => {
            occurs_check(name, ok, _generics) || occurs_check(name, err, _generics)
        }
        Type::Tuple(elems) => elems.iter().any(|e| occurs_check(name, e, _generics)),
        Type::Func(args, ret) => {
            args.iter().any(|a| occurs_check(name, a, _generics))
                || occurs_check(name, ret, _generics)
        }
        Type::Shared(inner) => occurs_check(name, inner, _generics),
        Type::LocalShared(inner) => occurs_check(name, inner, _generics),
        Type::Weak(inner) => occurs_check(name, inner, _generics),
        Type::WeakLocal(inner) => occurs_check(name, inner, _generics),
        Type::RawPtr(inner) => occurs_check(name, inner, _generics),
        Type::RawPtrMut(inner) => occurs_check(name, inner, _generics),
        Type::CShared(inner) => occurs_check(name, inner, _generics),
        Type::CBorrow(inner) => occurs_check(name, inner, _generics),
        Type::CBorrowMut(inner) => occurs_check(name, inner, _generics),
        Type::Newtype(_, inner) => occurs_check(name, inner, _generics),
        Type::ExternFunc(args, ret) => {
            args.iter().any(|a| occurs_check(name, a, _generics))
                || occurs_check(name, ret, _generics)
        }
        Type::CBuffer(inner) => occurs_check(name, inner, _generics),
        Type::Array(inner, _) => occurs_check(name, inner, _generics),
        Type::Slice(inner) => occurs_check(name, inner, _generics),
        Type::Cap(_)
        | Type::Nothing
        | Type::RawString
        | Type::Allocator
        | Type::Infer
        | Type::ImplTrait(_)
        | Type::DynTrait(_)
        | Type::TypeVar(_)
        | Type::ForAll(_, _) => false,
    }
}

/// Substitute type parameters in a type.
/// If substitution would cause infinite recursion (self-referential type),
/// returns the original type unchanged to let downstream checks catch the mismatch.
pub fn subst_type_params(
    ty: &Type,
    generics: &[GenericParam],
    type_map: &HashMap<String, Type>,
) -> Type {
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
                let new_args: Vec<Type> = args
                    .iter()
                    .map(|a| subst_type_params(a, generics, type_map))
                    .collect();
                Type::Name(name.clone(), new_args)
            }
        }
        Type::Ref(lt, inner) => Type::Ref(
            lt.clone(),
            Box::new(subst_type_params(inner, generics, type_map)),
        ),
        Type::RefMut(lt, inner) => Type::RefMut(
            lt.clone(),
            Box::new(subst_type_params(inner, generics, type_map)),
        ),
        Type::Option(inner) => Type::Option(Box::new(subst_type_params(inner, generics, type_map))),
        Type::Result(ok, err) => Type::Result(
            Box::new(subst_type_params(ok, generics, type_map)),
            Box::new(subst_type_params(err, generics, type_map)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| subst_type_params(e, generics, type_map))
                .collect(),
        ),
        Type::Func(args, ret) => Type::Func(
            args.iter()
                .map(|a| subst_type_params(a, generics, type_map))
                .collect(),
            Box::new(subst_type_params(ret, generics, type_map)),
        ),
        Type::Shared(inner) => Type::Shared(Box::new(subst_type_params(inner, generics, type_map))),
        Type::LocalShared(inner) => {
            Type::LocalShared(Box::new(subst_type_params(inner, generics, type_map)))
        }
        Type::Weak(inner) => Type::Weak(Box::new(subst_type_params(inner, generics, type_map))),
        Type::WeakLocal(inner) => {
            Type::WeakLocal(Box::new(subst_type_params(inner, generics, type_map)))
        }
        Type::RawPtr(inner) => Type::RawPtr(Box::new(subst_type_params(inner, generics, type_map))),
        Type::RawPtrMut(inner) => {
            Type::RawPtrMut(Box::new(subst_type_params(inner, generics, type_map)))
        }
        Type::CShared(inner) => {
            Type::CShared(Box::new(subst_type_params(inner, generics, type_map)))
        }
        Type::CBorrow(inner) => {
            Type::CBorrow(Box::new(subst_type_params(inner, generics, type_map)))
        }
        Type::CBorrowMut(inner) => {
            Type::CBorrowMut(Box::new(subst_type_params(inner, generics, type_map)))
        }
        Type::Newtype(name, inner) => Type::Newtype(
            name.clone(),
            Box::new(subst_type_params(inner, generics, type_map)),
        ),
        Type::Cap(_) | Type::Nothing | Type::RawString | Type::Allocator | Type::Infer => {
            ty.clone()
        }
        Type::ExternFunc(args, ret) => Type::ExternFunc(
            args.iter()
                .map(|a| subst_type_params(a, generics, type_map))
                .collect(),
            Box::new(subst_type_params(ret, generics, type_map)),
        ),
        Type::CBuffer(inner) => {
            Type::CBuffer(Box::new(subst_type_params(inner, generics, type_map)))
        }
        Type::Array(inner, size) => Type::Array(
            Box::new(subst_type_params(inner, generics, type_map)),
            *size,
        ),
        Type::Slice(inner) => Type::Slice(Box::new(subst_type_params(inner, generics, type_map))),
        Type::ImplTrait(traits) => Type::ImplTrait(traits.clone()),
        Type::DynTrait(traits) => Type::DynTrait(traits.clone()),
        Type::TypeVar(_) => ty.clone(), // Bug-3 clarification: TypeVar represents inference variables (u32 IDs)
        // NOT user-defined type parameters. User params like `T` in `type Foo[T] = T`
        // are stored as Type::Name("T", vec![]), which IS replaced by subst_type_params.
        // TypeVar is created by the inference engine and should be resolved by unify, not
        // by generic parameter substitution.
        Type::ForAll(params, body) => Type::ForAll(
            params.clone(),
            Box::new(subst_type_params(body, generics, type_map)),
        ),
    }
}

pub(crate) fn same_type(a: &Type, b: &Type) -> bool {
    // A4: same_type is now a pure structural equality check.
    // Escape hatches (Any, _, unknown, Infer) have been removed from same_type
    // and are handled exclusively by UnificationTable::unify, which has the
    // context to bind inference variables. This eliminates the class of bugs
    // where same_type returns true for Any-vs-anything, bypassing type safety.
    //
    // Callers that need escape-hatch compatibility should use unify() instead.
    // Type::Infer still matches itself structurally (both sides unknown).
    //
    // Normalize Type::Name("Result", [T, E]) <-> Type::Result(T, E) and Type::Name("Option", [T]) <-> Type::Option(T)
    // Compare args directly without cloning to allocate new enum variants.
    match (a, b) {
        (Type::Name(na, aa), Type::Name(nb, ab)) => {
            // A4: "unknown" is a cascade placeholder — only matches itself.
            if na == "unknown" || nb == "unknown" {
                return na == nb;
            }
            na == nb
                && aa.len() == ab.len()
                && aa.iter().zip(ab.iter()).all(|(x, y)| same_type(x, y))
        }
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
        (Type::Tuple(a), Type::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_type(x, y))
        }
        (Type::Func(a_args, a_ret), Type::Func(b_args, b_ret)) => {
            a_args.len() == b_args.len()
                && a_args
                    .iter()
                    .zip(b_args.iter())
                    .all(|(x, y)| same_type(x, y))
                && same_type(a_ret, b_ret)
        }
        (Type::Cap(a), Type::Cap(b)) => a == b,
        (Type::Shared(a), Type::Shared(b)) => same_type(a, b),
        (Type::LocalShared(a), Type::LocalShared(b)) => same_type(a, b),
        (Type::Weak(a), Type::Weak(b)) => same_type(a, b),
        (Type::WeakLocal(a), Type::WeakLocal(b)) => same_type(a, b),
        // A4: Newtypes with same name and same inner type are equal.
        // Different-name newtypes do NOT match (consistent with unify).
        // Previous code fell through to inner-type comparison via the
        // catch-all below, allowing Newtype("UserId", i32) == Newtype("OrderId", i32).
        (Type::Newtype(n1, a), Type::Newtype(n2, b)) => {
            n1 == n2 && same_type(a, b)
        }
        // Constructor or transparent: Newtype(name,inner) matches Name(n) if name==n or inner
        (Type::Newtype(n, inner), Type::Name(n2, _))
        | (Type::Name(n2, _), Type::Newtype(n, inner)) => {
            if n == n2 {
                true
            } else {
                same_type(inner, &Type::Name(n2.clone(), vec![]))
            }
        }
        // Newtype is transparent — same_type with non-Name, non-Newtype types
        (Type::Newtype(_, inner), other) if !matches!(other, Type::Newtype(..)) => {
            same_type(inner, other)
        }
        (other, Type::Newtype(_, inner)) if !matches!(other, Type::Newtype(..)) => {
            same_type(inner, other)
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
                && a_args
                    .iter()
                    .zip(b_args.iter())
                    .all(|(x, y)| same_type(x, y))
                && same_type(a_ret, b_ret)
        }
        (Type::CBuffer(a), Type::CBuffer(b)) => same_type(a, b),
        (Type::RawPtr(a), Type::RawPtr(b)) => same_type(a, b),
        (Type::RawPtrMut(a), Type::RawPtrMut(b)) => same_type(a, b),
        (Type::CShared(a), Type::CShared(b)) => same_type(a, b),
        (Type::CBorrow(a), Type::CBorrow(b)) => same_type(a, b),
        (Type::CBorrowMut(a), Type::CBorrowMut(b)) => same_type(a, b),
        (Type::TypeVar(a), Type::TypeVar(b)) => a == b,
        (Type::ForAll(p1, b1), Type::ForAll(p2, b2)) => p1 == p2 && same_type(b1, b2),
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

pub(crate) fn is_trait_coercion(
    declared: &Type,
    init_ty: &Type,
    impls: &HashMap<(String, String), Vec<String>>,
) -> bool {
    match (declared, init_ty) {
        (Type::DynTrait(trait_names), Type::Name(ty_name, _)) => trait_names
            .iter()
            .all(|trait_name| impls.contains_key(&(trait_name.clone(), ty_name.clone()))),
        _ => false,
    }
}

pub(crate) fn is_int(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64")
}

pub(crate) fn is_float(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "f64")
}

pub(crate) fn is_numeric(t: &Type) -> bool {
    matches!(t, Type::Name(n, _) if n == "i32" || n == "i64" || n == "f64")
}

/// CG-H2 (audit): predicates whether the codegen for `to_json` can serialize
/// the given type. The codegen supports:
/// - Primitive scalars: i32, i64, f64, bool, string, unit
/// - List<T> where T is a primitive or a Record (via mimi_list_*_to_json
///   and mimi_list_record_to_json)
/// - Record types (field-by-field sprintf serialization)
/// - Newtype (transparent — delegates to inner type)
/// - Any (escape hatch — interpreter handles all value types)
/// - Infer/_ (defer to runtime)
///
/// Genuinely unsupported: Option, Result, Map, Set, Tuple (no codegen path).
pub(crate) fn is_json_serializable(t: &Type) -> bool {
    match t {
        Type::Infer => true,                           // _ placeholder — defer
        Type::Newtype(_, inner) => is_json_serializable(inner), // transparent
        Type::Name(n, args) => {
            // Primitive scalars
            if matches!(n.as_str(), "i32" | "i64" | "f64" | "bool" | "string" | "unit" | "Any") {
                return true;
            }
            // List<T> is supported if T is serializable
            if n == "List" && !args.is_empty() {
                return is_json_serializable(&args[0]);
            }
            // Records are supported by the codegen record-to-json sprintf path.
            // We can't distinguish Records from other Name types here, so we
            // allow any Name with args (e.g. "Point", "User") — the codegen will
            // produce its own error if the record has unsupported field types.
            // Exclude known unsupported container types.
            if !args.is_empty() && !matches!(n.as_str(),
                "Option" | "Result" | "Map" | "Set" | "Tuple" | "Channel"
                | "Future" | "Weak" | "AtomicI32" | "AtomicI64" | "AtomicBool") {
                return true;
            }
            // Bare names without args that aren't primitives — allow (might be a record alias)
            if args.is_empty() && !matches!(n.as_str(),
                "Option" | "Result" | "Map" | "Set" | "Tuple" | "Channel"
                | "Future" | "Weak" | "AtomicI32" | "AtomicI64" | "AtomicBool") {
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Compute the common type of two numeric operands for binary operators.
/// Returns `None` if either operand is not numeric.  Widening follows the
/// usual numeric-promotion rules: any `f64` operand produces `f64`, otherwise
/// mixed integer widths produce `i64`.
pub(crate) fn common_numeric_type(a: &Type, b: &Type) -> Option<Type> {
    if !is_numeric(a) || !is_numeric(b) {
        return None;
    }
    if same_type(a, b) {
        return Some(a.clone());
    }
    if is_float(a) || is_float(b) {
        Some(Type::Name("f64".into(), vec![]))
    } else {
        Some(Type::Name("i64".into(), vec![]))
    }
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
        Type::Name(n, args) => format!(
            "{}<{}>",
            n,
            args.iter().map(fmt_type).collect::<Vec<_>>().join(", ")
        ),
        Type::Ref(lt, inner) => {
            if let Some(l) = lt {
                format!("&'{} {}", l, fmt_type(inner))
            } else {
                format!("&{}", fmt_type(inner))
            }
        }
        Type::RefMut(lt, inner) => {
            if let Some(l) = lt {
                format!("&'{} mut {}", l, fmt_type(inner))
            } else {
                format!("&mut {}", fmt_type(inner))
            }
        }
        Type::Option(inner) => format!("Option<{}>", fmt_type(inner)),
        Type::Result(ok, err) => format!("Result<{}, {}>", fmt_type(ok), fmt_type(err)),
        Type::Tuple(elems) => format!(
            "({})",
            elems.iter().map(fmt_type).collect::<Vec<_>>().join(", ")
        ),
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
        // Newtype is transparent when wrapping a non-Newtype inner type.
        // This aligns fmt_type with same_type: if same_type(Newtype(x, A), B) is true,
        // then fmt_type(Newtype(x, A)) == fmt_type(B).
        Type::Newtype(_name, inner) => {
            if !matches!(inner.as_ref(), Type::Newtype(..)) {
                fmt_type(inner)
            } else {
                format!("newtype {} {}", _name, fmt_type(inner))
            }
        }
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
        Type::Infer => "infer".to_string(),
        Type::ExternFunc(args, ret) => {
            let args_str: Vec<String> = args.iter().map(fmt_type).collect();
            format!(
                "extern \"C\" fn({}) -> {}",
                args_str.join(", "),
                fmt_type(ret)
            )
        }
        Type::CBuffer(inner) => format!("CBuffer<{}>", fmt_type(inner)),
        Type::TypeVar(id) => format!("?T{}", id),
        Type::ForAll(params, body) => {
            format!("forall {}. {}", params.join(", "), fmt_type(body))
        }
    }
}

/// Collect unique named lifetimes from a type (e.g., from `&'a i32` → collects "a")
pub(crate) fn collect_lifetimes(ty: &Type) -> Vec<String> {
    match ty {
        Type::Ref(Some(lt), inner) | Type::RefMut(Some(lt), inner) => {
            let mut lifetimes = vec![lt.clone()];
            lifetimes.extend(collect_lifetimes(inner));
            lifetimes
        }
        Type::Ref(None, inner) | Type::RefMut(None, inner) => collect_lifetimes(inner),
        Type::Option(inner) => collect_lifetimes(inner),
        Type::Result(ok, err) => {
            let mut lifetimes = collect_lifetimes(ok);
            lifetimes.extend(collect_lifetimes(err));
            lifetimes
        }
        Type::Tuple(elems) => {
            let mut lifetimes = Vec::new();
            for elem in elems {
                lifetimes.extend(collect_lifetimes(elem));
            }
            lifetimes
        }
        Type::Func(args, ret) => {
            let mut lifetimes = Vec::new();
            for arg in args {
                lifetimes.extend(collect_lifetimes(arg));
            }
            lifetimes.extend(collect_lifetimes(ret));
            lifetimes
        }
        Type::Name(_, args) => {
            let mut lifetimes = Vec::new();
            for arg in args {
                lifetimes.extend(collect_lifetimes(arg));
            }
            lifetimes
        }
        _ => Vec::new(),
    }
}

/// Check if a type contains any elided lifetime (Ref(None, _) or RefMut(None, _)).
pub(crate) fn type_contains_elided_lifetime(ty: &Type) -> bool {
    match ty {
        Type::Ref(None, _) | Type::RefMut(None, _) => true,
        Type::Ref(Some(_), inner) | Type::RefMut(Some(_), inner) => {
            type_contains_elided_lifetime(inner)
        }
        Type::Option(inner) => type_contains_elided_lifetime(inner),
        Type::Result(ok, err) => {
            type_contains_elided_lifetime(ok) || type_contains_elided_lifetime(err)
        }
        Type::Tuple(elems) => elems.iter().any(type_contains_elided_lifetime),
        Type::Func(args, ret) => {
            args.iter().any(type_contains_elided_lifetime) || type_contains_elided_lifetime(ret)
        }
        Type::Name(_, args) => args.iter().any(type_contains_elided_lifetime),
        _ => false,
    }
}

/// Apply lifetime elision: replace all `Ref(None, _)` with `Ref(Some(lt), _)` in a type.
pub(crate) fn elide_lifetime(ty: &Type, lt: &str) -> Type {
    match ty {
        Type::Ref(None, inner) => {
            Type::Ref(Some(lt.to_string()), Box::new(elide_lifetime(inner, lt)))
        }
        Type::Ref(Some(name), inner) => {
            Type::Ref(Some(name.clone()), Box::new(elide_lifetime(inner, lt)))
        }
        Type::RefMut(None, inner) => {
            Type::RefMut(Some(lt.to_string()), Box::new(elide_lifetime(inner, lt)))
        }
        Type::RefMut(Some(name), inner) => {
            Type::RefMut(Some(name.clone()), Box::new(elide_lifetime(inner, lt)))
        }
        Type::Option(inner) => Type::Option(Box::new(elide_lifetime(inner, lt))),
        Type::Result(ok, err) => Type::Result(
            Box::new(elide_lifetime(ok, lt)),
            Box::new(elide_lifetime(err, lt)),
        ),
        Type::Tuple(elems) => Type::Tuple(elems.iter().map(|e| elide_lifetime(e, lt)).collect()),
        Type::Func(args, ret) => Type::Func(
            args.iter().map(|a| elide_lifetime(a, lt)).collect(),
            Box::new(elide_lifetime(ret, lt)),
        ),
        Type::Name(name, args) => Type::Name(
            name.clone(),
            args.iter().map(|a| elide_lifetime(a, lt)).collect(),
        ),
        other => other.clone(),
    }
}
