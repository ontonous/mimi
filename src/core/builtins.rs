//! Canonical callable identity catalog for language builtins.
//!
//! Name classification belongs to the checker boundary. Backends may decide
//! that a known builtin is unsupported, but they must not maintain a separate
//! list that changes semantic resolution.

use crate::core::ir::{
    OwnershipTypeKind, Permission, PrimitiveType, ResolvedType, ResolvedTypeId, ResolvedTypeTable,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBuiltinMethod {
    pub identity: String,
    pub permission: Permission,
}

pub fn is_builtin_callable(name: &str) -> bool {
    matches!(
        name,
        "println"
            | "print"
            | "eprintln"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "format"
            | "assert_approx_eq"
            | "range"
            | "len"
            | "to_string"
            | "abs"
            | "min"
            | "max"
            | "push"
            | "pop"
            | "sqrt"
            | "floor"
            | "ceil"
            | "round"
            | "int_to_string"
            | "float_to_string"
            | "string_to_int"
            | "exit"
            | "lexer"
            | "mms_parse"
            | "ast_eval"
            | "input"
            | "file_exists"
            | "read_file"
            | "write_file"
            | "char_code"
            | "chr"
            | "str_char_at"
            | "listdir"
            | "is_dir"
            | "is_file"
            | "path_join"
            | "path_ext"
            | "path_basename"
            | "path_dirname"
            | "walk_dir"
            | "mkdir_p"
            | "remove_file"
            | "exec"
            | "exec_safe"
            | "file_stat"
            | "append_file"
            | "set_env"
            | "exec_pipe"
            | "read_file_partial"
            | "read_file_bytes"
            | "write_file_bytes"
            | "read_lines_json"
            | "read_lines_json_builtin"
            | "read_lines_each"
            | "sha256"
            | "base64_encode"
            | "base64_decode"
            | "str_contains"
            | "str_starts_with"
            | "str_ends_with"
            | "pow"
            | "random"
            | "pi"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "sinh"
            | "cosh"
            | "tanh"
            | "ln"
            | "log"
            | "log2"
            | "log10"
            | "exp"
            | "exp2"
            | "cbrt"
            | "str_parse_int"
            | "str_parse_float"
            | "to_int"
            | "to_float"
            | "str_index_of"
            | "str_repeat"
            | "str_trim"
            | "str_to_upper"
            | "str_to_lower"
            | "str_substring"
            | "contains"
            | "sum"
            | "reverse"
            | "flatten"
            | "sort"
            | "sort_f64"
            | "sort_str"
            | "zip"
            | "enumerate"
            | "str_split"
            | "str_join"
            | "str_replace"
            // v0.31.6: bare contract-language spellings of the string builtins.
            // The verifier's Z3 layer special-cases `char_at(s,i)` /
            // `starts_with(s,p)` / `ends_with(s,p)` (expr.rs); without these the
            // call-site catalog classified them Unknown and typed-body lowering
            // fail-closed ("closed Unknown call target"). `contains` is already
            // listed above.
            | "char_at"
            | "starts_with"
            | "ends_with"
            | "has_key"
            | "keys"
            | "values"
            | "map_new"
            | "map_get"
            | "map_set"
            | "map_remove"
            | "map_size"
            | "map_from_list"
            | "str_to_c_str"
            | "c_str_to_string"
            | "now"
            | "timestamp"
            | "now_ms"
            | "timestamp_ms"
            | "sleep"
            | "getenv"
            | "args"
            | "atomic_i32_new"
            | "atomic_i32_load"
            | "atomic_i32_store"
            | "atomic_i32_fetch_add"
            | "atomic_i32_compare_exchange"
            | "atomic_i32_drop"
            | "atomic_i64_new"
            | "atomic_i64_load"
            | "atomic_i64_store"
            | "atomic_i64_fetch_add"
            | "atomic_i64_drop"
            | "atomic_bool_new"
            | "atomic_bool_load"
            | "atomic_bool_store"
            | "atomic_bool_drop"
            | "mutex_new"
            | "mutex_lock"
            | "mutex_get"
            | "mutex_set"
            | "mutex_unlock"
            | "mutex_drop"
            | "channel_new"
            | "channel_send"
            | "channel_recv"
            | "channel_try_recv"
            | "channel_drop"
            | "session_send"
            | "session_recv"
            | "session_close"
            | "session_open"
            | "session_pair"
            | "actor_mailbox_depth"
            | "actor_is_muted"
            | "actor_set_mailbox_depth"
            | "actor_set_max_children"
            | "actor_spawn_count"
            | "actor_max_children"
            | "broadcast"
            | "spawn_detached"
            | "assert_state"
            | "inject_fault"
            | "shadow_alloc"
            | "shadow_tag"
            | "shadow_check"
            | "shadow_free"
            | "test_sandbox"
            | "option_value_or"
            | "to_json"
            | "from_json"
            | "json_get_string"
            | "json_get_int"
            | "json_get_element"
            | "json_is_valid"
            | "json_array_length"
            | "json_has_key"
            | "socket"
            | "connect"
            | "bind"
            | "listen"
            | "accept"
            | "send"
            | "recv"
            | "close_fd"
            | "http_get"
            | "http_post"
            | "from_int"
            | "regex_match"
            | "regex_find"
            | "regex_replace"
            | "regex_find_all"
            | "regex_capture_groups"
    )
}

/// Higher-order operations resolved as language intrinsics but lowered by a
/// dedicated consumer path rather than the ordinary backend builtin registry.
pub fn is_language_intrinsic_callable(name: &str) -> bool {
    matches!(name, "map" | "filter" | "reduce")
}

pub fn is_language_constructor(name: &str) -> bool {
    matches!(name, "Some" | "None" | "Ok" | "Err")
}

/// Resolve a language-provided method from the checker-finalized receiver
/// type. Surface receiver spelling is deliberately not an input.
pub fn resolve_builtin_method(
    receiver: &ResolvedTypeId,
    method: &str,
    types: &ResolvedTypeTable,
) -> Option<ResolvedBuiltinMethod> {
    let (family, known, permission) = match types.get(receiver)? {
        ResolvedType::Option(inner) => {
            let known = matches!(
                method,
                "unwrap"
                    | "expect"
                    | "unwrap_or"
                    | "is_some"
                    | "is_none"
                    | "ok_or"
                    | "map"
                    | "and_then"
                    | "map_err"
            ) || (method == "deref"
                && matches!(
                    types.get(inner),
                    Some(ResolvedType::Ownership {
                        kind: OwnershipTypeKind::Shared | OwnershipTypeKind::LocalShared,
                        ..
                    })
                ));
            ("option", known, observation_or_consume(method))
        }
        ResolvedType::Result { .. } => (
            "result",
            matches!(
                method,
                "unwrap"
                    | "expect"
                    | "unwrap_or"
                    | "is_ok"
                    | "is_err"
                    | "ok_or"
                    | "map"
                    | "and_then"
                    | "map_err"
            ),
            observation_or_consume(method),
        ),
        ResolvedType::Ownership { kind, .. } => match kind {
            OwnershipTypeKind::Shared => (
                "shared",
                matches!(method, "clone" | "deref" | "inner"),
                Permission::View,
            ),
            OwnershipTypeKind::LocalShared => (
                "local_shared",
                matches!(method, "clone" | "deref" | "inner"),
                Permission::View,
            ),
            OwnershipTypeKind::Weak => ("weak", method == "upgrade", Permission::View),
            OwnershipTypeKind::WeakLocal => ("weak_local", method == "upgrade", Permission::View),
        },
        ResolvedType::Primitive(PrimitiveType::String) => (
            "string",
            matches!(
                method,
                "len"
                    | "trim"
                    | "to_upper"
                    | "to_lower"
                    | "parse_int"
                    | "parse_float"
                    | "contains"
                    | "starts_with"
                    | "ends_with"
                    | "split"
                    | "replace"
                    | "repeat"
                    | "char_at"
                    | "substring"
                    | "index_of"
            ),
            Permission::View,
        ),
        ResolvedType::Nominal { item, .. } if item.as_str() == "builtin:type:List" => {
            ("list", method == "len", Permission::View)
        }
        ResolvedType::Nominal { item, .. } if item.as_str() == "builtin:type:Set" => (
            "set",
            matches!(
                method,
                "size" | "len" | "is_empty" | "contains" | "insert" | "remove" | "to_list"
            ),
            Permission::View,
        ),
        _ => return None,
    };
    known.then(|| ResolvedBuiltinMethod {
        identity: format!("builtin.method.{family}.{method}"),
        permission,
    })
}

fn observation_or_consume(method: &str) -> Permission {
    if matches!(method, "is_some" | "is_none" | "is_ok" | "is_err" | "deref") {
        Permission::View
    } else {
        Permission::Consume
    }
}
