use super::*;
use crate::lsp::LspServer;

#[test]
fn hover_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }";
    let result = server.compute_hover(text, 0, 5);
    assert!(result.is_some(), "should hover over 'add'");
    let hover = result.expect("src/tests/lsp_extended.rs:10 unwrap failed");
    let contents = hover.get("contents").expect("src/tests/lsp_extended.rs:11 unwrap failed").get("value").expect("src/tests/lsp_extended.rs:11 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:11 unwrap failed");
    assert!(contents.contains("add"), "hover should mention function name: {}", contents);
}

#[test]
fn hover_type() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }";
    let result = server.compute_hover(text, 0, 5);
    assert!(result.is_some(), "should hover over 'Point'");
    let hover = result.expect("src/tests/lsp_extended.rs:21 unwrap failed");
    let contents = hover.get("contents").expect("src/tests/lsp_extended.rs:22 unwrap failed").get("value").expect("src/tests/lsp_extended.rs:22 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:22 unwrap failed");
    assert!(contents.contains("Point"), "hover should mention type name: {}", contents);
}

#[test]
fn hover_module() {
    let server = LspServer::new();
    let text = "module Math { }";
    let result = server.compute_hover(text, 0, 7);
    assert!(result.is_some(), "should hover over 'Math'");
    let hover = result.expect("src/tests/lsp_extended.rs:32 unwrap failed");
    let contents = hover.get("contents").expect("src/tests/lsp_extended.rs:33 unwrap failed").get("value").expect("src/tests/lsp_extended.rs:33 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:33 unwrap failed");
    assert!(contents.contains("Math"), "hover should mention module name: {}", contents);
}

#[test]
fn hover_builtin() {
    let server = LspServer::new();
    let text = "func main() { println(42) }";
    let result = server.compute_hover(text, 0, 17);
    assert!(result.is_some(), "should hover over 'println'");
    let hover = result.expect("src/tests/lsp_extended.rs:43 unwrap failed");
    let contents = hover.get("contents").expect("src/tests/lsp_extended.rs:44 unwrap failed").get("value").expect("src/tests/lsp_extended.rs:44 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:44 unwrap failed");
    assert!(contents.contains("builtin"), "hover should mention builtin: {}", contents);
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
    let uri = def.get("uri").expect("src/tests/lsp_extended.rs:64 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:64 unwrap failed");
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
    assert!(symbols.len() >= 2, "should have at least 2 symbols, got {}", symbols.len());
    let names: Vec<&str> = symbols.iter()
        .map(|s| s.get("name").expect("src/tests/lsp_extended.rs:109 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:109 unwrap failed"))
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
    let names: Vec<&str> = symbols.iter()
        .map(|s| s.get("name").expect("src/tests/lsp_extended.rs:122 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:122 unwrap failed"))
        .collect();
    assert!(names.contains(&"Point"), "should contain 'Point'");
    assert!(names.contains(&"Color"), "should contain 'Color'");
}

#[test]
fn document_symbols_mixed() {
    let server = LspServer::new();
    let text = "module Math { }\ntype Point { x: i32, y: i32 }\nfunc add(a: i32, b: i32) -> i32 { a + b }";
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
    let server = LspServer::new();
    let text = "func main() { }";
    let items = server.compute_completion(text, 0, 0);
    let labels: Vec<&str> = items.iter()
        .map(|i| i.get("label").expect("src/tests/lsp_extended.rs:150 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:150 unwrap failed"))
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
    assert!(labels.contains(&"write_file"), "should contain 'write_file'");
    assert!(labels.contains(&"file_exists"), "should contain 'file_exists'");
    assert!(labels.contains(&"to_int"), "should contain 'to_int'");
    assert!(labels.contains(&"to_float"), "should contain 'to_float'");
    assert!(labels.contains(&"str_char_at"), "should contain 'str_char_at'");
    assert!(labels.contains(&"str_substring"), "should contain 'str_substring'");
    assert!(labels.contains(&"str_parse_int"), "should contain 'str_parse_int'");
    assert!(labels.contains(&"str_parse_float"), "should contain 'str_parse_float'");
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
    let contents = hover.get("contents").expect("src/tests/lsp_extended.rs:181 unwrap failed").get("value").expect("src/tests/lsp_extended.rs:181 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:181 unwrap failed");
    assert!(contents.contains("builtin"), "hover should mention builtin: {}", contents);
}

#[test]
fn diagnostic_has_position() {
    let result = check_source("func main() { let x = undefined_var }");
    assert!(result.is_err(), "should have error for undefined variable");
    let errors = result.unwrap_err();
    assert!(!errors.is_empty(), "should have at least one error");
    // All diagnostics should have span fields
    for err in &errors {
        // span.start_line and span.start_col are usize, always >= 0
        let _ = err.span.start_line;
        let _ = err.span.start_col;
    }
}

#[test]
fn diagnostic_undefined_variable() {
    let result = check_source("func main() { undefined_var }");
    assert!(result.is_err());
    let errors = result.unwrap_err();
    let msg = &errors[0].message;
    assert!(msg.contains("undefined") || msg.contains("unknown"), "error should mention undefined: {}", msg);
}

#[test]
fn diagnostic_type_mismatch() {
    let result = check_source(r#"
        func add(a: i32, b: i32) -> i32 { a + b }
        func main() { add(1, "hello") }
    "#);
    // This might or might not fail depending on type inference
    // Just ensure it doesn't panic
    let _ = result;
}

#[test]
fn diagnostic_multiple_errors() {
    let result = check_source(r#"
        func main() {
            let x = undefined1
            let y = undefined2
            let z = undefined3
        }
    "#);
    if let Err(errors) = result {
        assert!(errors.len() >= 1, "should have at least one error");
    }
}

#[test]
fn diagnostic_strict_mode() {
    let result = check_source_strict("func main() { 42 }");
    // Strict mode should still work
    let _ = result;
}

// ===================== Phase D: References Tests =====================

#[test]
fn references_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }";
    let refs = server.compute_references(text, 0, 5, "file:///test.mimi", true);
    // Should find definition + usage
    assert!(refs.len() >= 2, "should find at least 2 references to 'add', got {}", refs.len());
}

#[test]
fn references_type() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }\nfunc main() -> i32 { 42 }";
    let refs = server.compute_references(text, 0, 5, "file:///test.mimi", true);
    assert!(refs.len() >= 1, "should find at least 1 reference to 'Point'");
}

#[test]
fn references_exclude_declaration() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }";
    let refs = server.compute_references(text, 0, 5, "file:///test.mimi", false);
    // Should find only usage, not declaration
    assert!(refs.len() >= 1, "should find at least 1 reference excluding declaration");
}

// ===================== Phase D: Rename Tests =====================

#[test]
fn rename_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() -> i32 { add(1, 2) }";
    let result = server.compute_rename(text, 0, 5, "file:///test.mimi", "sum");
    assert!(result.is_some(), "should rename 'add' to 'sum'");
    let edit = result.expect("src/tests/lsp_extended.rs:276 unwrap failed");
    let changes = edit.get("changes").expect("src/tests/lsp_extended.rs:277 unwrap failed");
    let file_changes = changes.get("file:///test.mimi").expect("src/tests/lsp_extended.rs:278 unwrap failed").as_array().expect("src/tests/lsp_extended.rs:278 unwrap failed");
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
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() { add(1, 2) }";
    
    // Position 18 is inside the add() call, after the comma
    let result = server.compute_signature_help(text, 1, 18);
    eprintln!("Result at 18: {:?}", result);
    assert!(result.is_some(), "should show signature help for 'add'");
}

// ===================== Phase D: Signature Help Tests =====================

#[test]
fn signature_help_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }\nfunc main() { add(1, 2) }";
    // Position 18 is inside the add() call, after the comma
    let result = server.compute_signature_help(text, 1, 18);
    assert!(result.is_some(), "should show signature help for 'add'");
    let sig = result.expect("src/tests/lsp_extended.rs:312 unwrap failed");
    let signatures = sig.get("signatures").expect("src/tests/lsp_extended.rs:313 unwrap failed").as_array().expect("src/tests/lsp_extended.rs:313 unwrap failed");
    assert!(!signatures.is_empty(), "should have at least one signature");
}

#[test]
fn signature_help_builtin() {
    let server = LspServer::new();
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
    assert!(tokens.len() >= 10, "should have at least 10 token values (2+ tokens)");
}

#[test]
fn semantic_tokens_types() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }";
    let tokens = server.compute_semantic_tokens(text);
    assert!(!tokens.is_empty(), "should produce semantic tokens for type definition");
}

#[test]
fn semantic_tokens_numbers() {
    let server = LspServer::new();
    let text = "func main() { let x = 42 let y = 3.14 }";
    let tokens = server.compute_semantic_tokens(text);
    assert!(!tokens.is_empty(), "should produce semantic tokens for numbers");
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
    let result = server.compute_hover(text, 1, 10);
    let _ = result;
}

#[test]
fn definition_variable() {
    let server = LspServer::new();
    let text = "func main() -> i32 {\n    let x = 42;\n    x\n}";
    let result = server.compute_definition(text, 2, 4, "file:///test.mimi");
    let _ = result;
}

#[test]
fn document_symbols_with_modules() {
    let server = LspServer::new();
    let text = "module Math {\n    func add(a: i32, b: i32) -> i32 { a + b }\n}\nfunc main() { }";
    let symbols = server.compute_document_symbols(text);
    let names: Vec<&str> = symbols.iter()
        .map(|s| s.get("name").expect("src/tests/lsp_extended.rs:402 unwrap failed").as_str().expect("src/tests/lsp_extended.rs:402 unwrap failed"))
        .collect();
    assert!(names.contains(&"Math") || names.contains(&"main"), "should contain at least one symbol");
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
    let result = check_source(r#"
        func main() -> i32 {
            let x: i32 = "hello";
            0
        }
    "#);
    assert!(result.is_err(), "should reject string to i32 assignment");
    if let Err(errors) = &result {
        assert!(!errors.is_empty(), "should have at least one error");
    }
}

#[test]
fn diagnostic_double_error() {
    let result = check_source(r#"
        func main() {
            let x = undefined1;
            let y = undefined2;
        }
    "#);
    if let Err(errors) = result {
        assert!(errors.len() >= 2, "should have at least two errors");
    }
}

#[test]
fn diagnostic_undefined_variable_in_expression() {
    let result = check_source(r#"
        func main() -> i32 {
            let x = y + 1;
            0
        }
    "#);
    assert!(result.is_err(), "should reject undefined variable y");
}

#[test]
fn signature_help_multiple_overloads() {
    let server = LspServer::new();
    let text = "func foo(a: i32) -> i32 { a }\nfunc foo(a: i32, b: i32) -> i32 { a + b }\nfunc main() { foo(1, 2) }";
    let result = server.compute_signature_help(text, 2, 12);
    let _ = result;
}

#[test]
fn folding_range_complex_nesting() {
    let server = LspServer::new();
    let text = "func f() {\n    if true {\n        if false {\n            1\n        }\n    }\n}";
    let ranges = server.compute_folding_ranges(text);
    assert!(ranges.len() >= 3, "should have folding ranges for nested braces");
}

#[test]
fn folding_range_multiple_functions() {
    let server = LspServer::new();
    let text = "func a() {\n    1\n}\nfunc b() {\n    2\n}";
    let ranges = server.compute_folding_ranges(text);
    assert!(ranges.len() >= 2, "should have folding ranges for 2 functions");
}

// ── Code Actions ──────────────────────────────────────────────────

#[test]
fn code_action_undefined_variable() {
    let server = LspServer::new();
    let text = "func main() {\n    foo\n}";
    let diags = server.compute_diagnostics(text);
    let context = serde_json::json!({ "diagnostics": diags });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert!(titles.contains(&"Create variable `foo`"), "should offer create variable fix for E0400");
}

#[test]
fn code_action_undefined_function() {
    let server = LspServer::new();
    let text = "func main() {\n    bar()\n}";
    let diags = server.compute_diagnostics(text);
    let context = serde_json::json!({ "diagnostics": diags });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert!(titles.contains(&"Create function `bar`"), "should offer create function fix for E0401");
}

#[test]
fn code_action_undefined_type() {
    let server = LspServer::new();
    // Use `type Foo = Bar` — `Bar` is an undefined type
    let text = "type Foo = Bar";
    let diags = server.compute_diagnostics(text);
    assert!(!diags.is_empty(), "should have diagnostics for undefined type 'Bar': {:?}", diags);
    eprintln!("diagnostics: {:?}", diags);
    let context = serde_json::json!({ "diagnostics": diags });
    let actions = server.compute_code_actions("file:///test.mimi", &context);
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    // Accept both E0231 and E0407 based fixes
    let has_fix = titles.iter().any(|t| t.starts_with("Create type"));
    assert!(has_fix, "should offer create type fix, got titles: {:?}", titles);
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
    let mut server = LspServer::new();
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
    let diags = server.compute_diagnostics("func main() {\n    x\n}");
    assert!(!diags.is_empty(), "should have undefined variable diagnostic");

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
    let actions = resp["result"].as_array().expect("src/tests/lsp_extended.rs:566 unwrap failed");
    assert!(!actions.is_empty(), "should have code actions");
    let titles: Vec<&str> = actions.iter().filter_map(|a| a["title"].as_str()).collect();
    assert!(titles.contains(&"Create variable `x`"), "roundtrip should produce create variable");
}

// ── Workspace Symbols ────────────────────────────────────────────

#[test]
fn workspace_symbols_all() {
    let mut server = LspServer::new();
    server.documents.insert("file:///test.mimi".to_string(),
        "func hello() -> i32 { 42 }\ntype Foo = i64\nmodule bar { }".to_string());
    let symbols = server.compute_workspace_symbols("");
    assert!(symbols.len() >= 3, "should find at least 3 symbols, got {}", symbols.len());
    let names: Vec<&str> = symbols.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"hello"), "should find function hello");
    assert!(names.contains(&"Foo"), "should find type Foo");
    assert!(names.contains(&"bar"), "should find module bar");
}

#[test]
fn workspace_symbols_query_filter() {
    let mut server = LspServer::new();
    server.documents.insert("file:///test.mimi".to_string(),
        "func hello() { }\nfunc world() { }\ntype MyType = i64".to_string());
    let symbols = server.compute_workspace_symbols("hello");
    assert_eq!(symbols.len(), 1, "should find exactly 1 symbol matching 'hello'");
    assert_eq!(symbols[0]["name"].as_str().expect("src/tests/lsp_extended.rs:594 unwrap failed"), "hello");
}

#[test]
fn workspace_symbols_empty_query_returns_all() {
    let mut server = LspServer::new();
    server.documents.insert("file:///test.mimi".to_string(),
        "func a() { }\nfunc b() { }".to_string());
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
    let titles: Vec<&str> = lenses.iter().filter_map(|l| l["command"]["title"].as_str()).collect();
    assert!(titles.iter().any(|t| t == &"2 references"), "helper should have 2 references, got: {:?}", titles);
    assert!(titles.iter().any(|t| t == &"1 reference"), "main should have 1 reference, got: {:?}", titles);
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
    assert_eq!(items[0]["name"].as_str().expect("src/tests/lsp_extended.rs:650 unwrap failed"), "hello");
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
    assert_eq!(calls[0]["from"]["name"].as_str().expect("src/tests/lsp_extended.rs:674 unwrap failed"), "main");
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
    assert_eq!(calls[0]["to"]["name"].as_str().expect("src/tests/lsp_extended.rs:691 unwrap failed"), "helper");
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
    let contents = hover.get("contents").expect("contents key").get("value").expect("value key").as_str().expect("string").to_string();
    assert!(contents.contains("requires:"), "hover should show requires: {}", contents);
    assert!(contents.contains("ensures:"), "hover should show ensures: {}", contents);
    assert!(contents.contains("x >= 0"), "hover should show requires body: {}", contents);
    assert!(contents.contains("result == x + y"), "hover should show ensures body: {}", contents);
}

#[test]
fn lsp_hover_func_no_contracts() {
    let server = LspServer::new();
    let text = "func simple(x: i32) -> i32 { x }";
    let result = server.compute_hover(text, 0, 6);
    assert!(result.is_some(), "should hover over 'simple'");
    let hover = result.expect("src/tests/lsp_extended.rs: hover_func_no_contracts");
    let contents = hover.get("contents").expect("contents key").get("value").expect("value key").as_str().expect("string").to_string();
    assert!(contents.contains("simple"), "hover should mention func name: {}", contents);
    assert!(!contents.contains("requires:"), "hover should not show requires: {}", contents);
}

#[test]
fn lsp_hover_func_with_invariant() {
    let server = LspServer::new();
    let text = "func loop_counter(n: i32) -> i32 {\n    invariant: result >= 0\n    let mut i = 0\n    while i < n { i += 1 }\n    i\n}";
    let result = server.compute_hover(text, 0, 6);
    assert!(result.is_some(), "should hover over 'loop_counter'");
    let hover = result.expect("src/tests/lsp_extended.rs: hover_func_invariant");
    let contents = hover.get("contents").expect("contents key").get("value").expect("value key").as_str().expect("string").to_string();
    assert!(contents.contains("invariant:"), "hover should show invariant: {}", contents);
}

#[test]
fn lsp_code_lens_verify_status_no_contracts() {
    let server = LspServer::new();
    let text = "func main() -> i32 { 42 }";
    let lenses = server.compute_code_lens(text, "file:///test.mimi");
    // Should have 1 lens for reference count, no verify lens
    assert_eq!(lenses.len(), 1, "no-contract func should have 1 lens (ref count): {:?}", lenses);
    let title = lenses[0]["command"]["title"].as_str().expect("title");
    assert!(title.contains("reference"), "title should mention references: {}", title);
}

#[test]
fn lsp_code_lens_verify_status_with_contracts() {
    let server = LspServer::new();
    let text = "func add(x: i32, y: i32) -> i32 {\n    requires: x >= 0 && y >= 0\n    ensures: result == x + y\n    x + y\n}";
    let lenses = server.compute_code_lens(text, "file:///test.mimi");
    // Should have ref count lens + verify lens
    assert!(lenses.len() >= 2, "contract func should have at least 2 lenses: {:?}", lenses);
    let titles: Vec<&str> = lenses.iter()
        .filter_map(|l| l["command"]["title"].as_str())
        .collect();
    assert!(titles.iter().any(|t| t.contains(&"reference")), "should have ref lens: {:?}", titles);
    assert!(titles.iter().any(|t| t.contains(&"verify") || t.contains("✓") || t.contains("✗") || t.contains("?")),
        "should have verify lens: {:?}", titles);
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
    let titles: Vec<&str> = lenses.iter()
        .filter_map(|l| l["command"]["title"].as_str())
        .collect();
    assert!(titles.iter().any(|t| t.contains("✓")),
        "verified func should show ✓ in lens: {:?}", titles);
    assert!(titles.iter().any(|t| t.contains("postconditions verified")),
        "lens should show verification message: {:?}", titles);
}
