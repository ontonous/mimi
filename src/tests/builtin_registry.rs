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
        ("char_code", true, true, true),
        ("chr", true, true, true),
        ("str_char_at", true, true, true),
        ("str_contains", true, true, true),
        ("str_starts_with", true, true, true),
        ("str_ends_with", true, true, true),
        ("str_parse_int", true, true, true),
        ("str_parse_float", true, true, true),
        ("str_index_of", true, true, true),
        ("option_value_or", true, true, true),
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
        ("regex_find_all", true, true, true),
        ("regex_capture_groups", true, true, true),
        ("read_file_partial", true, true, true),
        ("read_file_bytes", true, true, true),
        ("write_file_bytes", true, true, true),
        ("read_lines_each", true, true, true),
        ("read_lines_json", true, true, true),
        ("exit", true, true, true),
        ("exec_pipe", true, true, true),
        ("from_int", true, true, true),
        // lexer/mms_parse are in is_builtin() but codegen returns a compile error
        ("lexer", true, true, true),
        ("mms_parse", true, true, true),
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
        assert!(
            failures.is_empty(),
            "is_builtin() registry mismatches:\n{}",
            failures.join("\n")
        );
    }

    /// TC-C4: cross-check ALL registry I/T flags against source match arms.
    /// Codegen flag is already checked via is_builtin(); here we ensure names
    /// claimed as interpreter- or typechecker-supported appear as string
    /// literals in the corresponding dispatch sources.
    #[test]
    fn test_builtin_registry_interp_and_typecheck_layers() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let call_src = std::fs::read_to_string(format!("{}/src/interp/call.rs", manifest))
            .expect("read interp/call.rs");
        // Type inference for builtins lives under infer/call/simple.rs (and helpers).
        let infer_dir = format!("{}/src/core/infer", manifest);
        let mut infer_src = String::new();
        if let Ok(entries) = std::fs::read_dir(&infer_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                    if let Ok(s) = std::fs::read_to_string(&p) {
                        infer_src.push_str(&s);
                        infer_src.push('\n');
                    }
                }
            }
        }
        // Also scan nested call/ directory.
        let infer_call = format!("{}/src/core/infer/call", manifest);
        if let Ok(entries) = std::fs::read_dir(&infer_call) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                    if let Ok(s) = std::fs::read_to_string(&p) {
                        infer_src.push_str(&s);
                        infer_src.push('\n');
                    }
                }
            }
        }

        let mut failures = Vec::new();
        for &(name, _codegen, interp, typecheck) in ALL {
            let lit = format!("\"{}\"", name);
            if interp && !call_src.contains(&lit) {
                failures.push(format!(
                    "'{}' marked I=true but not found as string literal in interp/call.rs",
                    name
                ));
            }
            if typecheck && !infer_src.contains(&lit) {
                failures.push(format!(
                    "'{}' marked T=true but not found as string literal under src/core/infer/",
                    name
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "builtin registry layer mismatches (TC-C4):\n{}",
            failures.join("\n")
        );
    }
}
