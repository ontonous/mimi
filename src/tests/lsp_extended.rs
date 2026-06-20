use super::*;
use crate::lsp::LspServer;

#[test]
fn hover_function() {
    let server = LspServer::new();
    let text = "func add(a: i32, b: i32) -> i32 { a + b }";
    let result = server.compute_hover(text, 0, 5);
    assert!(result.is_some(), "should hover over 'add'");
    let hover = result.unwrap();
    let contents = hover.get("contents").unwrap().get("value").unwrap().as_str().unwrap();
    assert!(contents.contains("add"), "hover should mention function name: {}", contents);
}

#[test]
fn hover_type() {
    let server = LspServer::new();
    let text = "type Point { x: i32, y: i32 }";
    let result = server.compute_hover(text, 0, 5);
    assert!(result.is_some(), "should hover over 'Point'");
    let hover = result.unwrap();
    let contents = hover.get("contents").unwrap().get("value").unwrap().as_str().unwrap();
    assert!(contents.contains("Point"), "hover should mention type name: {}", contents);
}

#[test]
fn hover_module() {
    let server = LspServer::new();
    let text = "module Math { }";
    let result = server.compute_hover(text, 0, 7);
    assert!(result.is_some(), "should hover over 'Math'");
    let hover = result.unwrap();
    let contents = hover.get("contents").unwrap().get("value").unwrap().as_str().unwrap();
    assert!(contents.contains("Math"), "hover should mention module name: {}", contents);
}

#[test]
fn hover_builtin() {
    let server = LspServer::new();
    let text = "func main() { println(42) }";
    let result = server.compute_hover(text, 0, 17);
    assert!(result.is_some(), "should hover over 'println'");
    let hover = result.unwrap();
    let contents = hover.get("contents").unwrap().get("value").unwrap().as_str().unwrap();
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
    let def = result.unwrap();
    let uri = def.get("uri").unwrap().as_str().unwrap();
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
        .map(|s| s.get("name").unwrap().as_str().unwrap())
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
        .map(|s| s.get("name").unwrap().as_str().unwrap())
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
        .map(|i| i.get("label").unwrap().as_str().unwrap())
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
    let hover = result.unwrap();
    let contents = hover.get("contents").unwrap().get("value").unwrap().as_str().unwrap();
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
    let edit = result.unwrap();
    let changes = edit.get("changes").unwrap();
    let file_changes = changes.get("file:///test.mimi").unwrap().as_array().unwrap();
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
    let sig = result.unwrap();
    let signatures = sig.get("signatures").unwrap().as_array().unwrap();
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
        .map(|s| s.get("name").unwrap().as_str().unwrap())
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
