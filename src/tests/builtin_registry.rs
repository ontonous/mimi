#[cfg(test)]
mod tests {
    use crate::codegen::builtins::is_builtin;

    /// Canonical registry of all Mimi builtin functions with layer support.
    /// Flags: C=codegen (true = in is_builtin() + has compile arm),
    ///        I=interp,
    ///        T=typechecker (in infer_expr match).
    const ALL: &[(&str, bool, bool, bool)] = &[
        ("print", true, true, true),
        ("println", true, true, true),
        ("eprintln", true, true, true),
        ("input", true, true, true),
        ("file_exists", true, true, true),
        ("read_file", true, true, true),
        ("write_file", true, true, true),
        ("assert", true, true, true),
        ("assert_eq", true, true, true),
        ("assert_ne", true, true, true),
        ("assert_approx_eq", true, true, true),
        ("to_string", true, true, true),
        ("to_int", true, true, true),
        ("to_float", true, true, true),
        ("str_char_at", true, true, true),
        ("str_contains", true, true, true),
        ("str_starts_with", true, true, true),
        ("str_ends_with", true, true, true),
        ("str_parse_int", true, true, true),
        ("str_parse_float", true, true, true),
        ("str_index_of", true, true, true),
        ("str_repeat", true, true, true),
        ("str_trim", true, true, true),
        ("str_to_upper", true, true, true),
        ("str_to_lower", true, true, true),
        ("str_substring", true, true, true),
        ("str_split", true, true, true),
        ("str_join", true, true, true),
        ("str_replace", true, true, true),
        ("str_to_c_str", true, true, true),
        ("c_str_to_string", true, true, true),
        ("sqrt", true, true, true),
        ("floor", true, true, true),
        ("ceil", true, true, true),
        ("round", true, true, true),
        ("abs", true, true, true),
        ("min", true, true, true),
        ("max", true, true, true),
        ("pow", true, true, true),
        ("random", true, true, true),
        ("pi", true, true, true),
        ("range", true, true, true),
        ("len", true, true, true),
        ("push", true, true, true),
        ("pop", true, true, true),
        ("contains", true, true, true),
        ("sum", true, true, true),
        ("reverse", true, true, true),
        ("flatten", true, true, true),
        ("sort", true, true, true),
        ("zip", true, true, true),
        ("enumerate", true, true, true),
        ("has_key", true, true, true),
        ("keys", true, true, true),
        ("values", true, true, true),
        ("map_new", true, true, true),
        ("map_get", true, true, true),
        ("map_set", true, true, true),
        ("map_remove", true, true, true),
        ("map_size", true, true, true),
        ("map_from_list", true, true, true),
        ("now", true, true, true),
        ("timestamp", true, true, true),
        ("now_ms", true, true, true),
        ("timestamp_ms", true, true, true),
        ("sleep", true, true, true),
        ("getenv", true, true, true),
        ("args", true, true, true),
        ("to_json", true, true, true),
        ("from_json", true, true, true),
        ("json_is_valid", true, true, true),
        ("json_get_string", true, true, true),
        ("json_get_int", true, true, true),
        ("json_get_element", true, true, true),
        ("socket", true, true, true),
        ("connect", true, true, true),
        ("bind", true, true, true),
        ("listen", true, true, true),
        ("accept", true, true, true),
        ("send", true, true, true),
        ("recv", true, true, true),
        ("close_fd", true, true, true),
        ("http_get", true, true, true),
        ("http_post", true, true, true),
        ("regex_match", true, true, true),
        ("regex_find", true, true, true),
        ("regex_replace", true, true, true),
        ("exit", true, true, true),
        ("from_int", true, true, true),
        // lexer/parse are in is_builtin() but codegen returns a compile error
        ("lexer", true, true, true),
        ("parse", true, true, true),
        // TC + interp only (no codegen implementation)
        ("type_name", false, true, true),
        ("type_fields", false, true, true),
        ("type_variants", false, true, true),
        ("alloc", false, true, true),
        ("allocator_system", false, true, true),
        ("allocator_arena", false, true, true),
        ("allocator_bump", false, true, true),
        ("arena_reset", false, true, true),
        ("bump_used", false, true, true),
        ("ast_dump", false, true, true),
        ("ast_eval", true, true, true),
        ("filter", false, true, true),
        ("map", false, true, true),
        ("reduce", false, true, true),
    ];

    #[test]
    fn test_is_builtin_coverage() {
        let mut failures = Vec::new();
        for &(name, codegen, _, _) in ALL {
            let listed = is_builtin(name);
            if codegen && !listed {
                failures.push(format!("'{}' should be in is_builtin()", name));
            }
            if !codegen && listed {
                failures.push(format!("'{}' should NOT be in is_builtin()", name));
            }
        }
        assert!(failures.is_empty(), "is_builtin() registry mismatches:\n{}", failures.join("\n"));
    }
}
