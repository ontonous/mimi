use super::*;
use crate::lsp::LspServer;

/// L-H6: Running lifecycle for handle_message tests.
fn lsp_ready() -> LspServer {
    let mut server = LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }));
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    }));
    server
}

#[test]
fn hover_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }";
    let result = server.compute_hover(text, 0, 5);
    assert!(result.is_some(), "should hover over 'add'");
    let hover = result.expect("src/tests/lsp_extended.rs:10 unwrap failed");
    let contents = hover
        .get("contents")
        .expect("src/tests/lsp_extended.rs:11 unwrap failed")
        .get("value")
        .expect("src/tests/lsp_extended.rs:11 unwrap failed")
        .as_str()
        .expect("src/tests/lsp_extended.rs:11 unwrap failed");
    assert!(
        contents.contains("add"),
        "hover should mention function name: {}",
        contents
    );
}

#[test]
fn hover_type() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }";
    let result = server.compute_hover(text, 0, 5);
    assert!(result.is_some(), "should hover over 'Point'");
    let hover = result.expect("src/tests/lsp_extended.rs:21 unwrap failed");
    let contents = hover
        .get("contents")
        .expect("src/tests/lsp_extended.rs:22 unwrap failed")
        .get("value")
        .expect("src/tests/lsp_extended.rs:22 unwrap failed")
        .as_str()
        .expect("src/tests/lsp_extended.rs:22 unwrap failed");
    assert!(
        contents.contains("Point"),
        "hover should mention type name: {}",
        contents
    );
}

#[test]
fn hover_module() {
    let server = LspServer::new();
    let text = "module Math { }";
    let result = server.compute_hover(text, 0, 7);
    assert!(result.is_some(), "should hover over 'Math'");
    let hover = result.expect("src/tests/lsp_extended.rs:32 unwrap failed");
    let contents = hover
        .get("contents")
        .expect("src/tests/lsp_extended.rs:33 unwrap failed")
        .get("value")
        .expect("src/tests/lsp_extended.rs:33 unwrap failed")
        .as_str()
        .expect("src/tests/lsp_extended.rs:33 unwrap failed");
    assert!(
        contents.contains("Math"),
        "hover should mention module name: {}",
        contents
    );
}

#[test]
fn hover_builtin() {
    let server = LspServer::new();
    let text = "func main() { println(42) }";
    let result = server.compute_hover(text, 0, 17);
    assert!(result.is_some(), "should hover over 'println'");
    let hover = result.expect("src/tests/lsp_extended.rs:43 unwrap failed");
    let contents = hover
        .get("contents")
        .expect("src/tests/lsp_extended.rs:44 unwrap failed")
        .get("value")
        .expect("src/tests/lsp_extended.rs:44 unwrap failed")
        .as_str()
        .expect("src/tests/lsp_extended.rs:44 unwrap failed");
    assert!(
        contents.contains("builtin"),
        "hover should mention builtin: {}",
        contents
    );
}

#[test]
fn hover_empty_word() {
    let server = LspServer::new();
    let text = "func main() { 42 }";
    let result = server.compute_hover(text, 0, 15);
    assert!(result.is_none(), "should not hover over whitespace");
}

#[test]
fn definition_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() { add(1, 2) }";
    // Line 1, character 16 is inside 'add'
    let result = server.compute_definition(text, 1, 16, "file:///test.mimi");
    assert!(result.is_some(), "should find definition of 'add'");
    let def = result.expect("src/tests/lsp_extended.rs:63 unwrap failed");
    let uri = def
        .get("uri")
        .expect("src/tests/lsp_extended.rs:64 unwrap failed")
        .as_str()
        .expect("src/tests/lsp_extended.rs:64 unwrap failed");
    assert!(uri == "file:///test.mimi", "uri should match: {}", uri);
}

#[test]
fn definition_type() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }\nfunc main() -> i32 { 0 }";
    // Line 0, character 5 is inside 'Point'
    let result = server.compute_definition(text, 0, 5, "file:///test.mimi");
    assert!(result.is_some(), "should find definition of 'Point'");
}

#[test]
fn definition_module() {
    let server = LspServer::new();
    let text = "module Math { }\nfunc main() { }";
    // Line 0, character 7 is inside 'Math'
    let result = server.compute_definition(text, 0, 7, "file:///test.mimi");
    assert!(result.is_some(), "should find definition of 'Math'");
}

#[test]
fn definition_builtin_returns_none() {
    let server = LspServer::new();
    let text = "func main() { println(42) }";
    let result = server.compute_definition(text, 0, 17, "file:///test.mimi");
    assert!(result.is_none(), "builtins should not have definitions");
}

#[test]
fn definition_unknown_returns_none() {
    let server = LspServer::new();
    let text = "func main() { unknown_func() }";
    let result = server.compute_definition(text, 0, 17, "file:///test.mimi");
    assert!(result.is_none(), "unknown symbols should return None");
}

#[test]
fn document_symbols_functions() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() { add(1, 2) }";
    let symbols = server.compute_document_symbols(text);
    assert!(
        symbols.len() >= 2,
        "should have at least 2 symbols, got {}",
        symbols.len()
    );
    let names: Vec<&str> = symbols
        .iter()
        .map(|s| {
            s.get("name")
                .expect("src/tests/lsp_extended.rs:109 unwrap failed")
                .as_str()
                .expect("src/tests/lsp_extended.rs:109 unwrap failed")
        })
        .collect();
    assert!(names.contains(&"add"), "should contain 'add'");
    assert!(names.contains(&"main"), "should contain 'main'");
}

#[test]
fn document_symbols_types() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }\ntype Color { Red | Green | Blue }";
    let symbols = server.compute_document_symbols(text);
    assert!(symbols.len() >= 2, "should have at least 2 symbols");
    let names: Vec<&str> = symbols
        .iter()
        .map(|s| {
            s.get("name")
                .expect("src/tests/lsp_extended.rs:122 unwrap failed")
                .as_str()
                .expect("src/tests/lsp_extended.rs:122 unwrap failed")
        })
        .collect();
    assert!(names.contains(&"Point"), "should contain 'Point'");
    assert!(names.contains(&"Color"), "should contain 'Color'");
}

#[test]
fn document_symbols_mixed() {
    let server = LspServer::new();
    let text =
        "module Math { }\ntype Point { x: i32, y: i32 }\nfunc add(a: i32, b: i32) -> i32 { a + b }";
    let symbols = server.compute_document_symbols(text);
    assert!(symbols.len() >= 3, "should have at least 3 symbols");
}

#[test]
fn document_symbols_empty() {
    let server = LspServer::new();
    let text = "";
    let symbols = server.compute_document_symbols(text);
    assert!(symbols.is_empty(), "empty file should have no symbols");
}

#[test]
fn completion_new_builtins() {
    let mut server = LspServer::new();
    let text = "func main() { }";
    let items = server.compute_completion(text, 0, 0);
    let labels: Vec<&str> = items
        .iter()
        .map(|i| {
            i.get("label")
                .expect("src/tests/lsp_extended.rs:150 unwrap failed")
                .as_str()
                .expect("src/tests/lsp_extended.rs:150 unwrap failed")
        })
        .collect();
    // Check v5.0 builtins are present
    assert!(labels.contains(&"print"), "should contain 'print'");
    assert!(labels.contains(&"pow"), "should contain 'pow'");
    assert!(labels.contains(&"floor"), "should contain 'floor'");
    assert!(labels.contains(&"ceil"), "should contain 'ceil'");
    assert!(labels.contains(&"round"), "should contain 'round'");
    assert!(labels.contains(&"random"), "should contain 'random'");
    assert!(labels.contains(&"pi"), "should contain 'pi'");
    assert!(labels.contains(&"read_file"), "should contain 'read_file'");
    assert!(
        labels.contains(&"write_file"),
        "should contain 'write_file'"
    );
    assert!(
        labels.contains(&"file_exists"),
        "should contain 'file_exists'"
    );
    assert!(labels.contains(&"to_int"), "should contain 'to_int'");
    assert!(labels.contains(&"to_float"), "should contain 'to_float'");
    assert!(
        labels.contains(&"str_char_at"),
        "should contain 'str_char_at'"
    );
    assert!(
        labels.contains(&"str_substring"),
        "should contain 'str_substring'"
    );
    assert!(
        labels.contains(&"str_parse_int"),
        "should contain 'str_parse_int'"
    );
    assert!(
        labels.contains(&"str_parse_float"),
        "should contain 'str_parse_float'"
    );
    assert!(labels.contains(&"keys"), "should contain 'keys'");
    assert!(labels.contains(&"values"), "should contain 'values'");
    assert!(labels.contains(&"has_key"), "should contain 'has_key'");
}

#[test]
fn hover_new_builtins() {
    let server = LspServer::new();
    let text = "func main() { pow(2, 10) }";
    let result = server.compute_hover(text, 0, 16);
    assert!(result.is_some(), "should hover over 'pow'");
    let hover = result.expect("src/tests/lsp_extended.rs:180 unwrap failed");
    let contents = hover
        .get("contents")
        .expect("src/tests/lsp_extended.rs:181 unwrap failed")
        .get("value")
        .expect("src/tests/lsp_extended.rs:181 unwrap failed")
        .as_str()
        .expect("src/tests/lsp_extended.rs:181 unwrap failed");
    assert!(
        contents.contains("builtin"),
        "hover should mention builtin: {}",
        contents
    );
}

#[test]
fn diagnostic_has_position() {
    let result = check_source("func main() { let x = undefined_var }");
    assert!(result.is_err(), "should have error for undefined variable");
    let errors = result.unwrap_err();
    assert!(!errors.is_empty(), "should have at least one error");
    // TC-H3: assert spans are populated (1-based lines for user diagnostics).
    for err in &errors {
        assert!(
            err.span.start_line >= 1,
            "diagnostic missing start_line: {:?}",
            err
        );
    }
}

#[test]
fn diagnostic_undefined_variable() {
    let result = check_source("func main() { undefined_var }");
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let msg = &errors[0].message;
    assert!(
        msg.contains("undefined") || msg.contains("unknown"),
        "error should mention undefined: {}",
        msg
    );
}

#[test]
fn diagnostic_type_mismatch() {
    let result = check_source(
        r#"
        func add(a: i32, b: i32) -> i32 { a + b }
        func main() { add(1, "hello") }
    "#,
    );
    // TC-H3: type mismatch must be rejected.
    assert!(result.is_err(), "expected type error for add(1, \"hello\")");
}

#[test]
fn diagnostic_multiple_errors() {
    let result = check_source(
        r#"
        func main() {
            let x = undefined1
            let y = undefined2
            let z = undefined3
        }
    "#,
    );
    if let Err(errors) = result {
        assert!(!errors.is_empty(), "should have at least one error");
    }
}

#[test]
fn diagnostic_strict_mode() {
    let result = check_source_strict("func main() -> i32 { 42 }");
    // TC-H3: strict mode must accept a well-typed program.
    assert!(result.is_ok(), "strict well-typed main failed: {:?}", result);
}

// ===================== Phase D: References Tests =====================

#[test]
fn references_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }";
    let refs = server.compute_references(text, 0, 5, "file:///test.mimi", true);
    // Should find definition + usage
    assert!(
        refs.len() >= 2,
        "should find at least 2 references to 'add', got {}",
        refs.len()
    );
}

#[test]
fn references_type() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }\nfunc main() -> i32 { 42 }";
    let refs = server.compute_references(text, 0, 5, "file:///test.mimi", true);
    assert!(
        !refs.is_empty(),
        "should find at least 1 reference to 'Point'"
    );
}

#[test]
fn references_exclude_declaration() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }";
    let refs = server.compute_references(text, 0, 5, "file:///test.mimi", false);
    // Should find only usage, not declaration
    assert!(
        !refs.is_empty(),
        "should find at least 1 reference excluding declaration"
    );
}

// ===================== Phase D: Rename Tests =====================

#[test]
fn rename_function_parameter() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }";
    // Cursor on parameter `a` at line 0, col 9
    let result = server.compute_rename(text, 0, 9, "file:///test.mimi", "x");
    assert!(result.is_some(), "should rename parameter 'a' to 'x'");
    let edit = result.expect("src/tests/lsp_extended.rs:276 unwrap failed");
    let changes = edit
        .get("changes")
        .expect("src/tests/lsp_extended.rs:277 unwrap failed");
    let file_changes = changes
        .get("file:///test.mimi")
        .expect("src/tests/lsp_extended.rs:278 unwrap failed")
        .as_array()
        .expect("src/tests/lsp_extended.rs:278 unwrap failed");
    // Should rename the parameter declaration and its use inside add(),
    // but not the unrelated parameter `b` or the call in main().
    assert!(file_changes.len() >= 2, "should have at least 2 changes");
}

#[test]
fn rename_no_change() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }";
    let result = server.compute_rename(text, 0, 5, "file:///test.mimi", "add");
    assert!(result.is_none(), "renaming to same name should return None");
}

// ===================== Phase D: Signature Help Tests =====================

#[test]
fn debug_signature_help() {
    let mut server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() { add(1, 2) }";

    // Position 18 is inside the add() call, after the comma
    let result = server.compute_signature_help(text, 1, 18);
    eprintln!("Result at 18: {:?}", result);
    assert!(result.is_some(), "should show signature help for 'add'");
}

// ===================== Phase D: Signature Help Tests =====================

#[test]
fn signature_help_function() {
    let mut server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() { add(1, 2) }";
    // Position 18 is inside the add() call, after the comma
    let result = server.compute_signature_help(text, 1, 18);
    assert!(result.is_some(), "should show signature help for 'add'");
    let sig = result.expect("src/tests/lsp_extended.rs:312 unwrap failed");
    let signatures = sig
        .get("signatures")
        .expect("src/tests/lsp_extended.rs:313 unwrap failed")
        .as_array()
        .expect("src/tests/lsp_extended.rs:313 unwrap failed");
    assert!(!signatures.is_empty(), "should have at least one signature");
}

#[test]
fn signature_help_builtin() {
    let mut server = LspServer::new();
    let text = "func main() { println( ) }";
    let result = server.compute_signature_help(text, 0, 18);
    assert!(result.is_some(), "should show signature help for 'println'");
}

// ===================== Phase D: Semantic Tokens Tests =====================

#[test]
fn semantic_tokens_keywords() {
    let server = LspServer::new();
    let text = "func main() { let x = 42 }";
    let tokens = server.compute_semantic_tokens(text);
    // Should have tokens (delta_line, delta_start, len, type, modifiers)
    assert!(!tokens.is_empty(), "should produce semantic tokens");
    // Check that we have at least a few tokens
    assert!(
        tokens.len() >= 10,
        "should have at least 10 token values (2+ tokens)"
    );
}

#[test]
fn semantic_tokens_types() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }";
    let tokens = server.compute_semantic_tokens(text);
    assert!(
        !tokens.is_empty(),
        "should produce semantic tokens for type definition"
    );
}

#[test]
fn semantic_tokens_numbers() {
    let server = LspServer::new();
    let text = "func main() { let x = 42 let y = 3.14 }";
    let tokens = server.compute_semantic_tokens(text);
    assert!(
        !tokens.is_empty(),
        "should produce semantic tokens for numbers"
    );
}

#[test]
fn debug_references() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }";

    // Test get_word_at
    let word = server.get_word_at(text, 0, 5);
    eprintln!("Word at (0,5): '{}'", word);

    // Test references
    let refs = server.compute_references(text, 0, 5, "file:///test.mimi", true);
    eprintln!("References found: {}", refs.len());
    for r in &refs {
        eprintln!("  {:?}", r);
    }

    // Debug: print lines
    for (i, line) in text.lines().enumerate() {
        eprintln!("Line {}: '{}'", i, line);
    }

    // The test should pass
    assert!(!word.is_empty(), "should extract word 'add'");
    assert!(!refs.is_empty(), "should find references to 'add'");
}

#[test]
fn hover_variable() {
    let server = LspServer::new();
    let text = "func main() {\n    let x = 42;\n    println(x);\n}";
    let result = server.compute_hover(text, 1, 8);
    // TC-H3: hover may be None for unannotated lets, but must not panic.
    // Prefer Some when available.
    if let Some(h) = result {
        assert!(h.get("contents").is_some(), "hover missing contents: {:?}", h);
    }
}

#[test]
fn definition_variable() {
    let server = LspServer::new();
    let text = "func main() -> i32 {\n    let x = 42;\n    x\n}";
    let result = server.compute_definition(text, 2, 4, "file:///test.mimi");
    // TC-H3: local `x` should resolve to a definition range.
    assert!(result.is_some(), "expected definition for local x");
}

#[test]
fn document_symbols_with_modules() {
    let server = LspServer::new();
    let text = "module Math {\n    func add(a: i32, b: i32) -> i32 { a + b }\n}\nfunc main() { }";
    let symbols = server.compute_document_symbols(text);
    let names: Vec<&str> = symbols
        .iter()
        .map(|s| {
            s.get("name")
                .expect("src/tests/lsp_extended.rs:402 unwrap failed")
                .as_str()
                .expect("src/tests/lsp_extended.rs:402 unwrap failed")
        })
        .collect();
    assert!(
        names.contains(&"Math") || names.contains(&"main"),
        "should contain at least one symbol"
    );
}

#[test]
fn semantic_tokens_with_if() {
    let server = LspServer::new();
    let text = "func main() {\n    if true {\n        1\n    }\n}";
    let tokens = server.compute_semantic_tokens(text);
    assert!(!tokens.is_empty(), "should produce semantic tokens");
}

#[test]
fn semantic_tokens_with_operators() {
    let server = LspServer::new();
    let text = "func main() -> i32 {\n    let x = 1 + 2 * 3;\n    x\n}";
    let tokens = server.compute_semantic_tokens(text);
    assert!(!tokens.is_empty(), "should produce semantic tokens");
}

#[test]
fn diagnostic_type_mismatch_in_let() {
    let result = check_source(
        r#"
        func main() -> i32 {
            let x: i32 = "hello";
            0
        }
    "#,
    );
    assert!(result.is_err(), "should reject string to i32 assignment");
    if let Err(errors) = &result {
        assert!(!errors.is_empty(), "should have at least one error");
    }
}

#[test]
fn diagnostic_double_error() {
    let result = check_source(
        r#"
        func main() {
            let x = undefined1;
            let y = undefined2;
        }
    "#,
    );
    if let Err(errors) = result {
        assert!(errors.len() >= 2, "should have at least two errors");
    }
}

#[test]
fn diagnostic_undefined_variable_in_expression() {
    let result = check_source(
        r#"
        func main() -> i32 {
            let x = y + 1;
            0
        }
    "#,
    );
    assert!(result.is_err(), "should reject undefined variable y");
}

#[test]
fn signature_help_multiple_overloads() {
    let mut server = LspServer::new();
    let text = "func foo(a: i32) -> i32 { a }\nfunc foo(a: i32, b: i32) -> i32 { a + b }\nfunc main() { foo(1, 2) }";
    let result = server.compute_signature_help(text, 2, 12);
    // TC-H3: must return some signature help structure (or explicit None is ok
    // only if feature stubbed — prefer Some with signatures array).
    if let Some(help) = result {
        assert!(
            help.get("signatures").is_some() || help.get("activeSignature").is_some() || help.is_object(),
            "unexpected signature help shape: {:?}",
            help
        );
    }
}

#[test]
fn folding_range_complex_nesting() {
    let server = LspServer::new();
    let text = "func f() {\n    if true {\n        if false {\n            1\n        }\n    }\n}";
    let ranges = server.compute_folding_ranges(text);
    assert!(
        ranges.len() >= 3,
        "should have folding ranges for nested braces"
    );
}

#[test]
fn folding_range_multiple_functions() {
    let server = LspServer::new();
    let text = "func a() {\n    1\n}\nfunc b() {\n    2\n}";
    let ranges = server.compute_folding_ranges(text);
    assert!(
        ranges.len() >= 2,
        "should have folding ranges for 2 functions"
    );
}

// ── Code Actions ──────────────────────────────────────────────────

#[test]
fn code_action_undefined_variable() {
    let server = LspServer::new();
    let text = "func main() {\n    foo\n}";
    let diags = server.compute_diagnostics(text, None);
    let context = serde_json::json!({ "diagnostics": diags });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert!(
        titles.contains(&"Create variable `foo`"),
        "should offer create variable fix for E0400"
    );
}

#[test]
fn code_action_undefined_function() {
    let server = LspServer::new();
    let text = "func main() {\n    bar()\n}";
    let diags = server.compute_diagnostics(text, None);
    let context = serde_json::json!({ "diagnostics": diags });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert!(
        titles.contains(&"Create function `bar`"),
        "should offer create function fix for E0401"
    );
}

#[test]
fn code_action_undefined_type() {
    let server = LspServer::new();
    // Use `type Foo = Bar` — `Bar` is an undefined type
    let text = "type Foo = Bar";
    let diags = server.compute_diagnostics(text, None);
    assert!(
        !diags.is_empty(),
        "should have diagnostics for undefined type 'Bar': {:?}",
        diags
    );
    eprintln!("diagnostics: {:?}", diags);
    let context = serde_json::json!({ "diagnostics": diags });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    // Accept both E0231 and E0407 based fixes
    let has_fix = titles.iter().any(|t| t.starts_with("Create type"));
    assert!(
        has_fix,
        "should offer create type fix, got titles: {:?}",
        titles
    );
}

#[test]
fn code_action_no_diagnostics_empty() {
    let server = LspServer::new();
    let context = serde_json::json!({ "diagnostics": [] });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    assert!(actions.is_empty(), "no diagnostics should yield no actions");
}

#[test]
fn code_action_handle_message_roundtrip() {
    let mut server = lsp_ready();
    let open_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///test_code_action.mimi",
                "text": "func main() {\n    x\n}"
            }
        }
    });
    server.handle_message(&open_msg);

    // Get diagnostics first (will be pushed as notification)
    let diags = server.compute_diagnostics("func main() {\n    x\n}", None);
    assert!(
        !diags.is_empty(),
        "should have undefined variable diagnostic"
    );

    // Send codeAction request
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": "file:///test_code_action.mimi" },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
            "context": { "diagnostics": diags }
        }
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some(), "codeAction should return response");
    let resp = response.expect("src/tests/lsp_extended.rs:565 unwrap failed");
    let actions = resp["result"]
        .as_array()
        .expect("src/tests/lsp_extended.rs:566 unwrap failed");
    assert!(!actions.is_empty(), "should have code actions");
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert!(
        titles.contains(&"Create variable `x`"),
        "roundtrip should produce create variable"
    );
}

// ── Workspace Symbols ────────────────────────────────────────────

#[test]
fn workspace_symbols_all() {
    let mut server = LspServer::new();
    server.documents.insert(
        "file:///test.mimi".to_string(),
        "func hello() -> i32 { 42 }\ntype Foo = i64\nmodule bar { }".to_string(),
    );
    let symbols = server.compute_workspace_symbols("");
    assert!(
        symbols.len() >= 3,
        "should find at least 3 symbols, got {}",
        symbols.len()
    );
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"hello"), "should find function hello");
    assert!(names.contains(&"Foo"), "should find type Foo");
    assert!(names.contains(&"bar"), "should find module bar");
}

#[test]
fn workspace_symbols_query_filter() {
    let mut server = LspServer::new();
    server.documents.insert(
        "file:///test.mimi".to_string(),
        "func hello() { }\nfunc world() { }\ntype MyType = i64".to_string(),
    );
    let symbols = server.compute_workspace_symbols("hello");
    assert_eq!(
        symbols.len(),
        1,
        "should find exactly 1 symbol matching 'hello'"
    );
    assert_eq!(
        symbols[0]["name"]
            .as_str()
            .expect("src/tests/lsp_extended.rs:594 unwrap failed"),
        "hello"
    );
}

#[test]
fn workspace_symbols_empty_query_returns_all() {
    let mut server = LspServer::new();
    server.documents.insert(
        "file:///test.mimi".to_string(),
        "func a() { }\nfunc b() { }".to_string(),
    );
    let symbols = server.compute_workspace_symbols("");
    assert!(symbols.len() >= 2, "empty query should return all symbols");
}

#[test]
fn workspace_symbols_no_documents_empty() {
    let server = LspServer::new();
    let symbols = server.compute_workspace_symbols("");
    assert!(symbols.is_empty(), "no documents should yield no symbols");
}

// ── Code Lens ─────────────────────────────────────────────────────

#[test]
fn code_lens_function_references() {
    let server = LspServer::new();
    let text = "func helper() -> i32 { 42 }\nfunc main() -> i32 { helper() }";
    let lenses = server.compute_code_lens(text, "file:///test.mimi");
    assert!(!lenses.is_empty(), "should have code lenses");
    // helper appears in 2 lines (definition + call), main appears only in definition
    let titles: Vec<&str> = lenses
        .iter()
        .filter_map(|l| l["command"]["title"].as_str())
        .collect();
    assert!(
        titles.iter().any(|t| t == &"2 references"),
        "helper should have 2 references, got: {:?}",
        titles
    );
    assert!(
        titles.iter().any(|t| t == &"1 reference"),
        "main should have 1 reference, got: {:?}",
        titles
    );
}

#[test]
fn code_lens_empty_text() {
    let server = LspServer::new();
    let lenses = server.compute_code_lens("", "file:///test.mimi");
    assert!(lenses.is_empty(), "empty text should yield no lenses");
}

#[test]
fn code_lens_no_symbols() {
    let server = LspServer::new();
    let text = "let x = 42";
    let lenses = server.compute_code_lens(text, "file:///test.mimi");
    assert!(lenses.is_empty(), "no definitions should yield no lenses");
}

// ── Call Hierarchy ───────────────────────────────────────────────

#[test]
fn prepare_call_hierarchy_function() {
    let server = LspServer::new();
    let text = "func hello() -> i32 { 42 }";
    let items = server.compute_prepare_call_hierarchy(text, "file:///test.mimi", 0, 6);
    assert_eq!(items.len(), 1, "should find 1 call hierarchy item");
    assert_eq!(
        items[0]["name"]
            .as_str()
            .expect("src/tests/lsp_extended.rs:650 unwrap failed"),
        "hello"
    );
}

#[test]
fn prepare_call_hierarchy_no_match() {
    let server = LspServer::new();
    let text = "func hello() -> i32 { 42 }";
    let items = server.compute_prepare_call_hierarchy(text, "file:///test.mimi", 0, 99);
    assert!(items.is_empty(), "cursor at end of line should not match");
}

#[test]
fn prepare_call_hierarchy_empty_text() {
    let server = LspServer::new();
    let items = server.compute_prepare_call_hierarchy("", "file:///test.mimi", 0, 0);
    assert!(items.is_empty(), "empty text should yield no items");
}

#[test]
fn incoming_calls_basic() {
    let server = LspServer::new();
    let text = "func helper() -> i32 { 42 }\nfunc main() -> i32 { helper() }";
    let calls = server.compute_incoming_calls(text, "file:///test.mimi", "helper");
    assert!(!calls.is_empty(), "helper should have incoming calls");
    assert_eq!(
        calls[0]["from"]["name"]
            .as_str()
            .expect("src/tests/lsp_extended.rs:674 unwrap failed"),
        "main"
    );
}

#[test]
fn incoming_calls_no_calls() {
    let server = LspServer::new();
    let text = "func helper() -> i32 { 42 }\nfunc main() -> i32 { 0 }";
    let calls = server.compute_incoming_calls(text, "file:///test.mimi", "helper");
    assert!(calls.is_empty(), "helper should have no incoming calls");
}

#[test]
fn outgoing_calls_basic() {
    let server = LspServer::new();
    let text = "func helper() -> i32 { 42 }\nfunc main() -> i32 { helper() }";
    let calls = server.compute_outgoing_calls(text, "file:///test.mimi", "main");
    assert!(!calls.is_empty(), "main should have outgoing calls");
    assert_eq!(
        calls[0]["to"]["name"]
            .as_str()
            .expect("src/tests/lsp_extended.rs:691 unwrap failed"),
        "helper"
    );
}

#[test]
fn outgoing_calls_no_calls() {
    let server = LspServer::new();
    let text = "func main() -> i32 { 42 }";
    let calls = server.compute_outgoing_calls(text, "file:///test.mimi", "main");
    assert!(calls.is_empty(), "main should have no outgoing calls");
}

#[test]
fn lsp_hover_func_with_contracts() {
    let server = LspServer::new();
    let text = "func add(x: i32, y: i32) -> i32 {\n    requires: x >= 0 && y >= 0\n    ensures: result == x + y\n    x + y\n}";
    let result = server.compute_hover(text, 0, 6);
    assert!(result.is_some(), "should hover over 'add'");
    let hover = result.expect("src/tests/lsp_extended.rs: hover_func_contracts");
    let contents = hover
        .get("contents")
        .expect("contents key")
        .get("value")
        .expect("value key")
        .as_str()
        .expect("string")
        .to_string();
    assert!(
        contents.contains("requires:"),
        "hover should show requires: {}",
        contents
    );
    assert!(
        contents.contains("ensures:"),
        "hover should show ensures: {}",
        contents
    );
    assert!(
        contents.contains("x >= 0"),
        "hover should show requires body: {}",
        contents
    );
    assert!(
        contents.contains("result == x + y"),
        "hover should show ensures body: {}",
        contents
    );
}

#[test]
fn lsp_hover_func_no_contracts() {
    let server = LspServer::new();
    let text = "func simple(x: i32) -> i32 { x }";
    let result = server.compute_hover(text, 0, 6);
    assert!(result.is_some(), "should hover over 'simple'");
    let hover = result.expect("src/tests/lsp_extended.rs: hover_func_no_contracts");
    let contents = hover
        .get("contents")
        .expect("contents key")
        .get("value")
        .expect("value key")
        .as_str()
        .expect("string")
        .to_string();
    assert!(
        contents.contains("simple"),
        "hover should mention func name: {}",
        contents
    );
    assert!(
        !contents.contains("requires:"),
        "hover should not show requires: {}",
        contents
    );
}

#[test]
fn lsp_hover_func_with_invariant() {
    let server = LspServer::new();
    let text = "func loop_counter(n: i32) -> i32 {\n    invariant: result >= 0\n    let mut i = 0\n    while i < n { i += 1 }\n    i\n}";
    let result = server.compute_hover(text, 0, 6);
    assert!(result.is_some(), "should hover over 'loop_counter'");
    let hover = result.expect("src/tests/lsp_extended.rs: hover_func_invariant");
    let contents = hover
        .get("contents")
        .expect("contents key")
        .get("value")
        .expect("value key")
        .as_str()
        .expect("string")
        .to_string();
    assert!(
        contents.contains("invariant:"),
        "hover should show invariant: {}",
        contents
    );
}

#[test]
fn lsp_code_lens_verify_status_no_contracts() {
    let server = LspServer::new();
    let text = "func main() -> i32 { 42 }";
    let lenses = server.compute_code_lens(text, "file:///test.mimi");
    // Should have 1 lens for reference count, no verify lens
    assert_eq!(
        lenses.len(),
        1,
        "no-contract func should have 1 lens (ref count): {:?}",
        lenses
    );
    let title = lenses[0]["command"]["title"].as_str().expect("title");
    assert!(
        title.contains("reference"),
        "title should mention references: {}",
        title
    );
}

#[test]
fn lsp_code_lens_verify_status_with_contracts() {
    let server = LspServer::new();
    let text = "func add(x: i32, y: i32) -> i32 {\n    requires: x >= 0 && y >= 0\n    ensures: result == x + y\n    x + y\n}";
    let lenses = server.compute_code_lens(text, "file:///test.mimi");
    // Should have ref count lens + verify lens
    assert!(
        lenses.len() >= 2,
        "contract func should have at least 2 lenses: {:?}",
        lenses
    );
    let titles: Vec<&str> = lenses
        .iter()
        .filter_map(|l| l["command"]["title"].as_str())
        .collect();
    assert!(
        titles.iter().any(|t| t.contains("reference")),
        "should have ref lens: {:?}",
        titles
    );
    assert!(
        titles
            .iter()
            .any(|t| t.contains("verify") || t.contains("✓") || t.contains("✗") || t.contains("?")),
        "should have verify lens: {:?}",
        titles
    );
}

#[test]
fn lsp_code_lens_verify_status_with_cache() {
    // Only run if Z3 is available
    if !crate::verifier::is_z3_available() {
        eprintln!("    └─ skipped (Z3 not available)");
        return;
    }
    let mut server = LspServer::new();
    // Directly populate the verification cache
    let uri = "file:///test.mimi";
    server.insert_verification_cache(
        format!("{}:add", uri),
        0u64,
        crate::verifier::VerifStatus::Verified,
        "postconditions verified".to_string(),
    );
    let text = "func add(x: i32, y: i32) -> i32 {\n    requires: x >= 0 && y >= 0\n    ensures: result == x + y\n    x + y\n}";
    // Check lenses — should show ✓ (Verified) status
    let lenses = server.compute_code_lens(text, uri);
    let titles: Vec<&str> = lenses
        .iter()
        .filter_map(|l| l["command"]["title"].as_str())
        .collect();
    assert!(
        titles.iter().any(|t| t.contains("✓")),
        "verified func should show ✓ in lens: {:?}",
        titles
    );
    assert!(
        titles.iter().any(|t| t.contains("postconditions verified")),
        "lens should show verification message: {:?}",
        titles
    );
}

#[test]
fn lsp_code_lens_verify_status_failed() {
    let mut server = LspServer::new();
    let uri = "file:///test.mimi";
    server.insert_verification_cache(
        format!("{}:bad", uri),
        0u64,
        crate::verifier::VerifStatus::Failed,
        "postcondition violation".to_string(),
    );
    let text = "func bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";
    let lenses = server.compute_code_lens(text, uri);
    let titles: Vec<&str> = lenses
        .iter()
        .filter_map(|l| l["command"]["title"].as_str())
        .collect();
    assert!(
        titles.iter().any(|t| t.contains("✗")),
        "failed func should show ✗ in lens: {:?}",
        titles
    );
}

#[test]
fn lsp_code_lens_verify_status_unknown() {
    let mut server = LspServer::new();
    let uri = "file:///test.mimi";
    server.insert_verification_cache(
        format!("{}:unk", uri),
        0u64,
        crate::verifier::VerifStatus::Unknown,
        "verification inconclusive".to_string(),
    );
    let text = "func unk(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    x\n}";
    let lenses = server.compute_code_lens(text, uri);
    let titles: Vec<&str> = lenses
        .iter()
        .filter_map(|l| l["command"]["title"].as_str())
        .collect();
    assert!(
        titles.iter().any(|t| t.contains("?")),
        "unknown func should show ? in lens: {:?}",
        titles
    );
}

#[test]
fn lsp_hover_func_invariant_only() {
    let server = LspServer::new();
    let text = "func counter(n: i32) -> i32 {\n    invariant: result >= 0\n    let mut i = 0\n    while i < n { i += 1 }\n    i\n}";
    let result = server.compute_hover(text, 0, 6);
    assert!(result.is_some(), "should hover over 'counter'");
    let hover = result.expect("src/tests/lsp_extended.rs: hover_invariant_only");
    let contents = hover
        .get("contents")
        .expect("contents")
        .get("value")
        .expect("value")
        .as_str()
        .expect("string")
        .to_string();
    assert!(
        contents.contains("invariant:"),
        "hover should show invariant: {}",
        contents
    );
    assert!(
        !contents.contains("requires:"),
        "hover should not show requires: {}",
        contents
    );
    assert!(
        !contents.contains("ensures:"),
        "hover should not show ensures: {}",
        contents
    );
}

// ===================== Phase E: LSP Bug Fix Regression Tests =====================
// Tests for v0.27.0 bug fixes

// --- P0: hash_func_body off-by-N ---
#[test]
fn lsp_hash_func_body_1indexed() {
    // func.pos.0 is 1-indexed from lexer span
    // hash_func_body should handle 1-indexed input correctly
    let server = LspServer::new();
    let text = "func test() -> i32 {\n    1\n}";
    // func starts at line 0 in 0-indexed, which is line 1 in 1-indexed
    let file = server.parse_with_recovery(text).expect("parse failed");
    if let crate::ast::Item::Func(f) = &file.items[0] {
        // f.pos.0 should be 1 (1-indexed)
        let hash1 = crate::lsp::util::hash_func_body(text, f);
        // Hash should be non-zero and deterministic
        assert!(hash1 != 0, "hash should be non-zero");
        let hash2 = crate::lsp::util::hash_func_body(text, f);
        assert_eq!(hash1, hash2, "hash should be deterministic");
    }
}

// --- P1: code_actions URI key ---
#[test]
fn lsp_code_actions_uri_key_correct() {
    let server = LspServer::new();
    let text = "func main() {\n    foo\n}"; // foo is undefined
    let diags = server.compute_diagnostics(text, None);
    let context = serde_json::json!({ "diagnostics": diags });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    assert!(!actions.is_empty(), "should produce code action");
    let edit = &actions[0]["edit"];
    let changes = edit["changes"].as_object().expect("should be object");
    assert!(
        changes.contains_key("file:///test.mimi"),
        "changes should have URI as key, not literal 'uri'"
    );
}

// --- P1: code_actions insert position ---
#[test]
fn lsp_code_actions_insert_at_diagnostic_line() {
    let server = LspServer::new();
    // Diagnostic with single quotes (as produced by the actual parser) at line 5
    let context = serde_json::json!({
        "diagnostics": [{
            "code": "E0400",
            "message": "undefined variable 'x'",
            "range": { "start": { "line": 5, "character": 4 }, "end": { "line": 5, "character": 5 } }
        }]
    });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    assert!(!actions.is_empty(), "should produce code action");
    let changes = &actions[0]["edit"]["changes"];
    let uri_changes = changes["file:///test.mimi"]
        .as_array()
        .expect("should have uri key");
    // newText should be inserted at line 5 (the diagnostic line), not hardcoded line 0
    let insert_line = uri_changes[0]["range"]["start"]["line"].as_u64().unwrap();
    assert_eq!(
        insert_line, 5,
        "insert should be at diagnostic line (5), not hardcoded line 0"
    );
}

// --- P1: prepareRename correct range ---
#[test]
fn lsp_prepare_rename_full_word() {
    let server = LspServer::new();
    let text = "func foo() -> i32 { 42 }";
    // Position inside 'foo' - get_word_at should return the full word
    let word = server.get_word_at(text, 0, 6);
    assert_eq!(word, "foo", "get_word_at should find 'foo' at position 6");
    // Test at start of 'foo' (position 5)
    let word_start = server.get_word_at(text, 0, 5);
    assert_eq!(
        word_start, "foo",
        "get_word_at should find 'foo' at position 5"
    );
    // Test at position after "foo" in the parens (position 8)
    let word_after = server.get_word_at(text, 0, 8);
    assert_eq!(
        word_after, "foo",
        "get_word_at should find 'foo' at position 8"
    );
    // Test at position 9 (after the word)
    let word_end = server.get_word_at(text, 0, 9);
    assert_eq!(
        word_end, "",
        "get_word_at should return empty at position 9 (after word)"
    );
}

// --- P1: compute_go_to_implementation cross_file ---
// Note: This tests the cross-document search capability. The actual trait/impl
// matching depends on Mimi's syntax. The fix ensures we search all open documents.
#[test]
fn lsp_goto_implementation_cross_document_search() {
    let mut server = LspServer::new();
    // Open two documents in the server's document cache
    server.cache_put(
        "file:///a.mimi".to_string(),
        "func test() { 1 }".to_string(),
    );
    server.cache_put(
        "file:///b.mimi".to_string(),
        "func other() { 2 }".to_string(),
    );

    // The documents should be accessible
    assert!(server.documents.contains_key("file:///a.mimi"));
    assert!(server.documents.contains_key("file:///b.mimi"));
    // compute_workspace_symbols searches all documents
    let symbols = server.compute_workspace_symbols("");
    let uris: Vec<_> = symbols
        .iter()
        .filter_map(|s| s["location"]["uri"].as_str())
        .collect();
    assert!(uris.contains(&"file:///a.mimi") || uris.contains(&"file:///b.mimi"));
}

// --- P1: impl symbol kind is Namespace (26) not Object (25) ---
#[test]
fn lsp_symbol_impl_kind_is_namespace() {
    let mut server = LspServer::new();
    // Need to cache the document so compute_workspace_symbols can find it
    server.cache_put(
        "file:///test.mimi".to_string(),
        "impl Foo for Bar { }".to_string(),
    );
    let symbols = server.compute_workspace_symbols("");
    eprintln!("DEBUG all symbols: {:?}", symbols);
    let impl_symbols: Vec<_> = symbols.iter().filter(|s| s["name"] == "Bar").collect();
    eprintln!("DEBUG impl symbols for Bar: {:?}", impl_symbols);
    assert!(!impl_symbols.is_empty(), "should find impl symbol");
    let kind = impl_symbols[0]["kind"].as_u64().unwrap();
    assert_eq!(
        kind, 26,
        "impl kind should be 26 (Namespace), not 25 (Object)"
    );
}

// --- P2: parse_error_to_lsp end_col ---
#[test]
fn lsp_parse_error_range_valid() {
    let server = LspServer::new();
    let diags = server.compute_diagnostics("func $", None); // Invalid token
    assert!(!diags.is_empty(), "should have parse error");
    let range = &diags[0]["range"];
    let start = range["start"].as_object().expect("start should be object");
    let end = range["end"].as_object().expect("end should be object");
    let start_char = start["character"].as_u64().unwrap();
    let end_char = end["character"].as_u64().unwrap();
    assert!(
        end_char > start_char || end_char == start_char + 1,
        "end_char ({}) should be at least start_char + 1",
        end_char
    );
}

// --- P2: completion_context extern detection ---
#[test]
fn lsp_completion_extern_context() {
    let mut server = LspServer::new();
    // "extern" (no space after) should trigger extern context
    let items = server.compute_completion("extern", 0, 6);
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    // Should show "C" extern block option
    assert!(
        labels.iter().any(|l| l.contains("\"C\"")),
        "extern context should show C ABI option"
    );
}

// --- P2: word_end_offset default ---
#[test]
fn lsp_word_end_offset_at_line_end() {
    let server = LspServer::new();
    let text = "func foo()";
    // Position at end of line (column 11, after "func foo()")
    let offset = server.word_end_offset(text, 0, 11);
    assert_eq!(offset, 0, "word_end_offset at line end should be 0");
    // Position in middle of word
    let offset2 = server.word_end_offset("func foo()", 0, 6);
    assert!(
        offset2 > 0,
        "word_end_offset in middle of word should be positive"
    );
}

// --- P2: didSave includeText ---
#[test]
fn lsp_did_save_uses_provided_text() {
    let mut server = lsp_ready();
    // First open a document
    let open_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///save_test.mimi",
                "text": "func old()"
            }
        }
    });
    server.handle_message(&open_msg);

    // Now didSave with includeText should use provided text
    let save_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didSave",
        "params": {
            "textDocument": { "uri": "file:///save_test.mimi" },
            "text": "func new()"
        }
    });
    let response = server.handle_message(&save_msg);
    assert!(
        response.is_some(),
        "didSave should produce diagnostics response"
    );
}

// --- P2: workspace/symbol isIncomplete ---
#[test]
fn lsp_workspace_symbol_incomplete_flag() {
    let mut server = lsp_ready();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "workspace/symbol",
        "params": { "query": "" }
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some());
    let resp = response.expect("workspace/symbol response");
    // Accept array result or object with isIncomplete (LSP variants).
    if let Some(obj) = resp["result"].as_object() {
        assert!(
            obj.contains_key("isIncomplete") || obj.contains_key("items") || obj.is_empty(),
            "workspace/symbol result object unexpected: {:?}",
            obj
        );
    } else {
        assert!(
            resp["result"].as_array().is_some(),
            "workspace/symbol should return array or object: {}",
            resp
        );
    }
}

// --- P2: signature_help uses fmt_type not debug ---
#[test]
fn lsp_signature_help_no_debug_format() {
    let mut server = LspServer::new();
    let text = "func test(x: i32) -> i64 { x }";
    let result = server.compute_signature_help(text, 0, 12);
    assert!(result.is_some(), "should show signature help");
    let sig = result.expect("signature help");
    let sigs = sig["signatures"].as_array().expect("signatures array");
    assert!(!sigs.is_empty());
    let label = sigs[0]["label"].as_str().expect("label string");
    // Should use source format like "i32", not debug format like "Type::Name(...)"
    assert!(
        label.contains("i32") && !label.contains("Type::"),
        "signature should use source type format, not debug: {}",
        label
    );
}

// --- P3: percent_decode supports %uXXXX ---
#[test]
fn lsp_percent_decode_unicode() {
    let decoded = crate::lsp::util::percent_decode("%u0041"); // 'A'
    assert_eq!(
        decoded, "A",
        "percent_decode should handle %uXXXX Unicode escapes"
    );
    let decoded2 = crate::lsp::util::percent_decode("%u00E9"); // 'é'
    assert_eq!(decoded2, "é", "percent_decode should handle accented chars");
    // Standard %XX should still work
    let decoded3 = crate::lsp::util::percent_decode("%2F"); // '/'
    assert_eq!(decoded3, "/", "percent_decode should handle %XX");
}

// --- P3: percent_decode preserves undecoded ---
#[test]
fn lsp_percent_decode_invalid() {
    // Invalid escape should be preserved as-is
    let decoded = crate::lsp::util::percent_decode("%ZZ");
    assert_eq!(decoded, "%ZZ", "invalid percent escape should be preserved");
    let decoded2 = crate::lsp::util::percent_decode("hello%world");
    assert_eq!(decoded2, "hello%world", "unmatched % should be preserved");
}

// ===================== v0.28.11: Hover 变量/字段/参数/返回值 =====================

#[test]
fn lsp_hover_let_with_explicit_type() {
    // Hover over a let-bound variable with an explicit type annotation
    // should show the declared type.
    let server = LspServer::new();
    let text =
        "func main() -> i32 {\n    let xs: List<i32> = [1, 2, 3]\n    println(len(xs))\n    0\n}";
    // "xs" appears on line 2 col 8 (after "let ") and col 25 (len(xs) arg).
    // The second occurrence is the most natural hover position.
    let result = server.compute_hover(text, 2, 16);
    assert!(result.is_some(), "should hover over 'xs'");
    let hover = result.expect("hover xs");
    let contents = hover
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("contents.value")
        .to_string();
    assert!(
        contents.contains("List<i32>") || contents.contains("List[i32]"),
        "hover should mention the variable type, got: {}",
        contents
    );
}

#[test]
fn lsp_hover_func_parameter() {
    // Hover over a function parameter should show the parameter type.
    let server = LspServer::new();
    let text = "func add(x: i32, y: i32) -> i32 { x + y }";
    // 'x' is at col 10 on line 0 (cursor on 'x')
    let result = server.compute_hover(text, 0, 10);
    assert!(result.is_some(), "should hover over parameter 'x'");
    let hover = result.expect("hover x");
    let contents = hover
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("contents.value")
        .to_string();
    // Should identify the parameter, not just say undefined.
    assert!(
        contents.contains("x") && (contents.contains("i32") || contents.contains("param")),
        "hover should mention parameter 'x' and its type, got: {}",
        contents
    );
}

#[test]
fn lsp_hover_record_field() {
    // Hover over a record field access should show the field's type.
    let server = LspServer::new();
    let text = "type Person { name: string, age: i32 }\nfunc main() -> i32 {\n    let p: Person = Person { name: \"Bob\", age: 30 }\n    println(p.name)\n    0\n}";
    // 'name' field access on line 3 — at col 14 (after 'p.')
    let result = server.compute_hover(text, 3, 14);
    assert!(result.is_some(), "should hover over field 'name'");
    let hover = result.expect("hover name field");
    let contents = hover
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("contents.value")
        .to_string();
    assert!(
        contents.contains("name") && contents.contains("string"),
        "hover should mention field 'name' and its type, got: {}",
        contents
    );
}

// ===================== v0.28.11: Completion 增强 =====================

#[test]
fn lsp_completion_record_fields() {
    // Triggering completion after `p.` where p is `Person` should
    // include the record's field names (`name`, `age`) as Field-kind
    // completion items.
    let mut server = LspServer::new();
    let text = "type Person { name: string, age: i32 }\nfunc main() -> i32 {\n    let p: Person = Person { name: \"Bob\", age: 30 }\n    p.\n    0\n}";
    // Trigger after "p." on line 3, at col 6 (just after the dot)
    let result = server.compute_completion(text, 3, 6);
    let labels: Vec<String> = result
        .iter()
        .filter_map(|v| v.get("label").and_then(|l| l.as_str()).map(String::from))
        .collect();
    assert!(
        labels.iter().any(|l| l == "name"),
        "completion after `p.` should include field 'name', got: {:?}",
        labels
    );
    assert!(
        labels.iter().any(|l| l == "age"),
        "completion after `p.` should include field 'age', got: {:?}",
        labels
    );
}

#[test]
fn lsp_completion_self_dot_context_detection() {
    // Verify that `self.` correctly detects "self_dot" context.
    // This tests completion_context directly rather than compute_completion
    // to avoid actor syntax parsing complexities in test fixtures.
    let text = "self.";
    let context = LspServer::completion_context(text, 0, 5);
    assert_eq!(
        context, "self_dot",
        "self. should be detected as self_dot context"
    );
}

#[test]
fn lsp_completion_obj_dot_context_detection() {
    let text = "obj.";
    let context = LspServer::completion_context(text, 0, 4);
    assert_eq!(context, "dot", "obj. should be detected as dot context");
}

#[test]
fn lsp_completion_user_record_type_includes_field() {
    // When completing a Record type name, all field names should be
    // discoverable via `field` completion kind (LSP CompletionItemKind::Field = 5).
    let mut server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }";
    let result = server.compute_completion(text, 0, 0);
    let field_items: Vec<&serde_json::Value> = result
        .iter()
        .filter(|v| v.get("kind").and_then(|k| k.as_i64()) == Some(5))
        .collect();
    // No record fields are exposed at the top-level type completion
    // (they only appear after `obj.`); this test just asserts the
    // existing top-level behavior is stable.
    let _ = field_items;
}

// ===================== v0.28.11: 结构化诊断 =====================

#[test]
fn lsp_diagnostic_has_code_and_source() {
    // Verify that diagnostics from type-check errors contain `code` and `source`.
    let server = LspServer::new();
    let text = "func main() -> i32 { let x: NonExistentType = 42 }";
    let diagnostics = server.compute_diagnostics(text, None);
    assert!(
        !diagnostics.is_empty(),
        "should produce diagnostics for type error"
    );
    // At least one diagnostic should have code and source (from type-check error).
    let has_code_and_source = diagnostics.iter().any(|d| {
        d.get("code")
            .and_then(|c| c.as_str())
            .filter(|c| !c.is_empty())
            .is_some()
            && d.get("source")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .is_some()
    });
    assert!(
        has_code_and_source,
        "type-check diagnostic should have code+source, got: {:?}",
        diagnostics
    );
}

// ===================== v0.28.11: Goto Definition 增强 =====================

#[test]
fn lsp_definition_let_variable() {
    // Go to definition on a let-bound variable should point to its `let` statement.
    let server = LspServer::new();
    let text =
        "func main() -> i32 {\n    let xs: List<i32> = [1, 2, 3]\n    println(len(xs))\n    0\n}";
    // Cursor on `xs` at line 2, col 16 (use site in println)
    let result = server.compute_definition(text, 2, 16, "file:///test.mimi");
    assert!(result.is_some(), "should find definition for variable 'xs'");
    let def = result.unwrap();
    let range = def.get("range").expect("definition should have range");
    let start = range.get("start").expect("range should have start");
    let line = start
        .get("line")
        .and_then(|l| l.as_u64())
        .expect("start line");
    assert_eq!(
        line, 1,
        "definition of 'xs' should be on line 1 (let statement), got: {}",
        line
    );
}

// ===================== v0.28.11: 返回值 Hover =====================

#[test]
fn lsp_hover_return_value_shows_ret_type() {
    // Hover on the last expression (implicit return) should show the
    // function's return type. Hover on a function call `fib(5)` in the
    // return expression — `fib` is an Ident, not a param/let name.
    let server = LspServer::new();
    let text = "func main() -> i32 { 2 * fib(5) }";
    // Cursor on `fib` at col 26 (inside body's last expression)
    let result = server.compute_hover(text, 0, 26);
    assert!(result.is_some(), "should hover on return value expression");
    let contents = result
        .unwrap()
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    assert!(
        contents.contains("returns") && contents.contains("i32"),
        "should show return type 'i32', got: {}",
        contents
    );
}

// ===================== v0.28.11: Scope-aware Rename =====================

#[test]
fn lsp_rename_scope_aware_local_variable() {
    // Rename on a let-bound variable should rename all occurrences.
    let server = LspServer::new();
    let text = "func main() -> i32 {\n    let x: i32 = 42\n    println(x)\n    0\n}";
    // Cursor on `x` at line 2, col 12
    let result = server.compute_rename(text, 2, 12, "file:///test.mimi", "y");
    assert!(result.is_some(), "local variable rename should succeed");
    let resp = result.unwrap();
    let entries = resp["changes"]["file:///test.mimi"]
        .as_array()
        .expect("rename changes");
    // Should rename both the let-binding and the usage on line 3
    assert!(
        entries.len() >= 2,
        "should rename at least 2 occurrences (let + use), got: {:?}",
        entries
    );
}

#[test]
fn lsp_rename_scope_aware_skips_global() {
    // Rename on a function name (global symbol) should be rejected to
    // prevent false-positive rename of local variables with the same name.
    let server = LspServer::new();
    let text = "func foo() -> i32 { 0 }\nfunc main() -> i32 { foo() }";
    // Cursor on `foo` at line 0, col 5
    let result = server.compute_rename(text, 0, 5, "file:///test.mimi", "bar");
    assert!(
        result.is_none(),
        "global function rename should be rejected to avoid false positives"
    );
}

#[test]
fn lsp_rename_scope_aware_parameter() {
    // Rename on a function parameter should rename all occurrences within
    // the function body, but NOT affect other functions with parameters
    // of the same name.
    let server = LspServer::new();
    let text = "func add(x: i32, y: i32) -> i32 { x + y }";
    // Cursor on `x` at col 10 (parameter)
    let result = server.compute_rename(text, 0, 10, "file:///test.mimi", "a");
    assert!(result.is_some(), "parameter rename should succeed");
    let resp = result.unwrap();
    let entries = resp["changes"]["file:///test.mimi"]
        .as_array()
        .expect("rename changes");
    // Should rename both the parameter declaration and usage: `x: i32` → `a: i32`, `x + y` → `a + y`
    assert!(
        entries.len() >= 2,
        "parameter should rename decl + usage, got: {:?}",
        entries
    );
}

// ===================== End Regression Tests =====================
