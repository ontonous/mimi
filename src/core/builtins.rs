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
            | "try_input_line"
            | "file_exists"
            | "read_file"
            | "write_file"
            | "char_code"
            | "chr"
            | "str_char_at"
            | "listdir"
            | "is_dir"
            | "is_file"
            | "make_token"
            | "token_id"
            | "token_channel_new"
            | "token_channel_send"
            | "token_channel_recv"
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
            | "read_file_guarded"
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
            // SD-7/9/10 escape hatches (0.31.55)
            | "wrapping_add"
            | "wrapping_sub"
            | "wrapping_mul"
            | "is_nan"
            | "is_infinite"
            | "is_finite"
            | "is_close"
            | "f64_eq_exact"
            | "str_parse_int"
            | "str_parse_float"
            | "to_int"
            | "to_float"
            | "str_index_of"
            | "str_count_substring"
            | "str_repeat"
            | "str_trim"
            | "str_to_upper"
            | "str_to_lower"
            | "str_substring"
            | "str_substring_strict"
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
            | "get_env_guarded"
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
            | "atomic_i64_compare_exchange"
            | "atomic_i64_drop"
            | "atomic_bool_new"
            | "atomic_bool_load"
            | "atomic_bool_store"
            | "atomic_bool_compare_exchange"
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
            | "flow_pack"
            | "flow_epoch"
            | "flow_check_epoch"
            | "flow_bump_epoch"
            | "flow_unpack"
            | "flow_drop"
            | "flow_pack_count"
            | "flow_epoch_last_error"
            | "session_send"
            | "session_recv"
            | "session_close"
            | "session_open"
            | "session_pair"
            | "actor_mailbox_depth"
            | "actor_is_faulted"
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
            | "http_get_guarded"
            | "http_post"
            | "from_int"
            | "regex_match"
            | "regex_find"
            | "regex_replace"
            | "regex_find_all"
            | "regex_capture_groups"
            // Higher-order list operations (shared across backends)
            | "map_list"
            | "filter_list"
            | "reduce_list"
            | "sort_list"
            | "find"
            | "any"
            | "all"
            | "is_empty"
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

/// Canonical arity table for language builtins (U1, 0.35.44).
///
/// Single source of truth for the argument count of every builtin. The
/// bytecode VM registry and codegen both consume it; `usize::MAX` marks a
/// variadic builtin (mirrors the VM registry's `usize::MAX` convention).
/// Adding a builtin requires registering its arity here first — the VM
/// registry asserts this table knows every registered name (arity-consistency
/// test in `registry.rs`), so the name→arity contract lives in one place.
pub fn builtin_arity(name: &str) -> Option<usize> {
    match name {
        "__slice" => Some(3),
        "abs" => Some(1),
        "accept" => Some(1),
        "acos" => Some(1),
        "actor_is_faulted" => Some(1),
        "actor_is_muted" => Some(1),
        "actor_mailbox_depth" => Some(1),
        "actor_max_children" => Some(0),
        "actor_set_mailbox_depth" => Some(2),
        "actor_set_max_children" => Some(1),
        "actor_spawn_count" => Some(0),
        "all" => Some(2),
        "alloc" => Some(2),
        "allocator_arena" => Some(0),
        "allocator_bump" => Some(0),
        "allocator_system" => Some(0),
        "any" => Some(2),
        "append_file" => Some(2),
        "arena_reset" => Some(1),
        "args" => Some(0),
        "asin" => Some(1),
        "assert" => Some(usize::MAX),
        "assert_approx_eq" => Some(2),
        "assert_eq" => Some(2),
        "assert_ne" => Some(2),
        "assert_state" => Some(2),
        "ast_dump" => Some(1),
        "ast_eval" => Some(1),
        "atan" => Some(1),
        "atan2" => Some(2),
        "atomic_bool_compare_exchange" => Some(3),
        "atomic_bool_drop" => Some(1),
        "atomic_bool_load" => Some(1),
        "atomic_bool_new" => Some(1),
        "atomic_bool_store" => Some(2),
        "atomic_i32_compare_exchange" => Some(3),
        "atomic_i32_drop" => Some(1),
        "atomic_i32_fetch_add" => Some(2),
        "atomic_i32_load" => Some(1),
        "atomic_i32_new" => Some(1),
        "atomic_i32_store" => Some(2),
        "atomic_i64_compare_exchange" => Some(3),
        "atomic_i64_drop" => Some(1),
        "atomic_i64_fetch_add" => Some(2),
        "atomic_i64_load" => Some(1),
        "atomic_i64_new" => Some(1),
        "atomic_i64_store" => Some(2),
        "base64_decode" => Some(1),
        "base64_encode" => Some(1),
        "bind" => Some(2),
        "broadcast" => Some(2),
        "bump_used" => Some(0),
        "c_str_to_string" => Some(1),
        "cbrt" => Some(1),
        "ceil" => Some(1),
        "channel_drop" => Some(1),
        "channel_new" => Some(0),
        "channel_recv" => Some(1),
        "channel_send" => Some(2),
        "channel_try_recv" => Some(1),
        "flow_bump_epoch" => Some(1),
        "flow_check_epoch" => Some(2),
        "flow_drop" => Some(1),
        "flow_epoch" => Some(1),
        "flow_epoch_last_error" => Some(0),
        "flow_pack" => Some(1),
        "flow_pack_count" => Some(0),
        "flow_unpack" => Some(1),
        "char_at" => Some(2),
        "char_code" => Some(2),
        "chr" => Some(1),
        "clone" => Some(1),
        "close_fd" => Some(1),
        "connect" => Some(3),
        "contains" => Some(2),
        "cos" => Some(1),
        "cosh" => Some(1),
        "deref" => Some(1),
        "ends_with" => Some(2),
        "enumerate" => Some(1),
        "eprintln" => Some(usize::MAX),
        "eq" => Some(2),
        "exec" => Some(1),
        "exec_pipe" => Some(1),
        "exec_safe" => Some(usize::MAX),
        "exit" => Some(usize::MAX),
        "exp" => Some(1),
        "exp2" => Some(1),
        "f64_eq_exact" => Some(2),
        "fields" => Some(1),
        "file_exists" => Some(1),
        "file_stat" => Some(1),
        "filter" => Some(2),
        "filter_list" => Some(2),
        "find" => Some(2),
        "flatten" => Some(1),
        "float" => Some(1),
        "float_to_string" => Some(1),
        "floor" => Some(1),
        "format" => Some(usize::MAX),
        "from_int" => Some(1),
        "from_json" => Some(1),
        "from_json_typed" => Some(2),
        "get_env_guarded" => Some(2),
        "getenv" => Some(1),
        "has_key" => Some(2),
        "http_get" => Some(1),
        "http_get_guarded" => Some(2),
        "http_post" => Some(2),
        "index_of" => Some(2),
        "inject_fault" => Some(1),
        "inner" => Some(1),
        "input" => Some(0),
        "input_bool" => Some(0),
        "input_float" => Some(0),
        "input_int" => Some(0),
        "input_line" => Some(0),
        "try_input_line" => Some(0),
        "insert" => Some(usize::MAX),
        "int" => Some(1),
        "int_to_string" => Some(1),
        "is_close" => Some(3),
        "is_dir" => Some(1),
        "is_empty" => Some(1),
        "is_file" => Some(1),
        "is_finite" => Some(1),
        "is_infinite" => Some(1),
        "is_nan" => Some(1),
        "json_array_length" => Some(1),
        "json_get_element" => Some(2),
        "json_get_int" => Some(2),
        "json_get_string" => Some(2),
        "json_has_key" => Some(2),
        "json_is_valid" => Some(1),
        "keys" => Some(1),
        "len" => Some(1),
        "lexer" => Some(1),
        "listdir" => Some(1),
        "listen" => Some(2),
        "ln" => Some(1),
        "log" => Some(usize::MAX),
        "log10" => Some(1),
        "log2" => Some(1),
        "map" => Some(2),
        "map_from_list" => Some(1),
        "map_get" => Some(2),
        "map_list" => Some(2),
        "map_new" => Some(0),
        "map_remove" => Some(2),
        "map_set" => Some(3),
        "map_size" => Some(1),
        "max" => Some(2),
        "min" => Some(2),
        "mkdir_p" => Some(1),
        "mms_parse" => Some(1),
        "mutex_drop" => Some(1),
        "mutex_get" => Some(1),
        "mutex_lock" => Some(1),
        "mutex_new" => Some(1),
        "mutex_set" => Some(2),
        "mutex_unlock" => Some(1),
        "now" => Some(0),
        "now_ms" => Some(0),
        "option_value_or" => Some(2),
        "parse_float" => Some(1),
        "parse_int" => Some(1),
        "path_basename" => Some(1),
        "path_dirname" => Some(1),
        "path_ext" => Some(1),
        "path_join" => Some(2),
        "pi" => Some(0),
        "pop" => Some(1),
        "pow" => Some(2),
        "print" => Some(usize::MAX),
        "print_err" => Some(usize::MAX),
        "println" => Some(usize::MAX),
        "push" => Some(2),
        "make_token" => Some(0),
        "token_channel_new" => Some(0),
        "token_channel_recv" => Some(1),
        "token_channel_send" => Some(2),
        "token_id" => Some(1),
        "random" => Some(0),
        "range" => Some(2),
        "read_file" => Some(1),
        "read_file_bytes" => Some(1),
        "read_file_guarded" => Some(2),
        "read_file_partial" => Some(2),
        "read_lines_each" => Some(2),
        "read_lines_json" => Some(1),
        "read_lines_json_builtin" => Some(1),
        "recv" => Some(2),
        "reduce" => Some(3),
        "reduce_list" => Some(3),
        "regex_capture_groups" => Some(2),
        "regex_find" => Some(2),
        "regex_find_all" => Some(2),
        "regex_match" => Some(2),
        "regex_replace" => Some(3),
        "remove" => Some(2),
        "remove_file" => Some(1),
        "repeat" => Some(2),
        "replace" => Some(3),
        "reverse" => Some(1),
        "round" => Some(1),
        "send" => Some(2),
        "session_close" => Some(1),
        "session_open" => Some(0),
        "session_pair" => Some(0),
        "session_recv" => Some(1),
        "session_send" => Some(2),
        "set_env" => Some(2),
        "sha256" => Some(1),
        "shadow_alloc" => Some(3),
        "shadow_check" => Some(2),
        "shadow_free" => Some(1),
        "shadow_tag" => Some(2),
        "sin" => Some(1),
        "sinh" => Some(1),
        "size" => Some(1),
        "sleep" => Some(1),
        "socket" => Some(3),
        "sort" => Some(1),
        "sort_f64" => Some(1),
        "sort_list" => Some(1),
        "sort_str" => Some(1),
        "spawn_detached" => Some(1),
        "split" => Some(usize::MAX),
        "sqrt" => Some(1),
        "starts_with" => Some(2),
        "str" => Some(1),
        "str_char_at" => Some(2),
        "str_contains" => Some(2),
        "str_count_substring" => Some(2),
        "str_ends_with" => Some(2),
        "str_index_of" => Some(2),
        "str_join" => Some(2),
        "str_parse_float" => Some(1),
        "str_parse_int" => Some(1),
        "str_repeat" => Some(2),
        "str_replace" => Some(3),
        "str_split" => Some(2),
        "str_starts_with" => Some(2),
        "str_substring" => Some(3),
        "str_substring_strict" => Some(3),
        "str_to_c_str" => Some(1),
        "str_to_lower" => Some(1),
        "str_to_upper" => Some(1),
        "str_trim" => Some(1),
        "string_to_int" => Some(1),
        "substring" => Some(3),
        "sum" => Some(1),
        "tan" => Some(1),
        "tanh" => Some(1),
        "test_sandbox" => Some(1),
        "timestamp" => Some(0),
        "timestamp_ms" => Some(0),
        "to_float" => Some(1),
        "to_int" => Some(1),
        "to_json" => Some(1),
        "to_list" => Some(1),
        "to_lower" => Some(1),
        "to_string" => Some(1),
        "to_upper" => Some(1),
        "trim" => Some(1),
        "type_fields" => Some(1),
        "type_name" => Some(1),
        "type_variants" => Some(1),
        "values" => Some(1),
        "walk_dir" => Some(1),
        "wrapping_add" => Some(2),
        "wrapping_mul" => Some(2),
        "wrapping_sub" => Some(2),
        "write_file" => Some(2),
        "write_file_bytes" => Some(2),
        "zip" => Some(2),
        _ => None,
    }
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
                        kind: OwnershipTypeKind::Shared,
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
            OwnershipTypeKind::Weak => ("weak", method == "upgrade", Permission::View),
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
        ResolvedType::Nominal { item, .. } if item.as_str() == "builtin:type:SessionChan" => (
            "session",
            matches!(method, "send" | "recv" | "close"),
            Permission::Consume,
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
