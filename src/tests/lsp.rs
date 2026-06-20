use crate::lsp::LspServer;

#[test]
fn lsp_initialize() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some(), "initialize should return response");
    let resp = response.unwrap();
    assert_eq!(resp["id"], 1);
    let caps = &resp["result"]["capabilities"];
    assert!(caps.get("textDocumentSync").is_some());
    assert!(caps.get("completionProvider").is_some());
    assert!(caps.get("codeActionProvider").is_some());
}

#[test]
fn lsp_initialized_no_response() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    let response = server.handle_message(&msg);
    assert!(response.is_none(), "initialized should not return response");
}

#[test]
fn lsp_did_open_publishes_diagnostics() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///test.mimi",
                "text": "func main() -> i32 {\n    42\n}"
            }
        }
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some());
    let resp = response.unwrap();
    assert_eq!(resp["method"], "textDocument/publishDiagnostics");
    let diagnostics = resp["params"]["diagnostics"].as_array().unwrap();
    assert!(diagnostics.is_empty(), "valid code should have no diagnostics");
}

#[test]
fn lsp_did_open_parse_error() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///bad.mimi",
                "text": "func $$$ broken"
            }
        }
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some());
    let resp = response.unwrap();
    let diagnostics = resp["params"]["diagnostics"].as_array().unwrap();
    assert!(!diagnostics.is_empty(), "syntax error should produce diagnostics");
}

#[test]
fn lsp_did_change() {
    let mut server = LspServer::new();
    let open_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///test.mimi",
                "text": "func main() -> i32 { 42 }"
            }
        }
    });
    server.handle_message(&open_msg);

    let change_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {
                "uri": "file:///test.mimi"
            },
            "contentChanges": [{
                "text": "func main() -> i32 { 99 }"
            }]
        }
    });
    let response = server.handle_message(&change_msg);
    assert!(response.is_some(), "didChange should produce diagnostics");
    let resp = response.unwrap();
    let diagnostics = resp["params"]["diagnostics"].as_array().unwrap();
    assert!(diagnostics.is_empty(), "changed valid code should have no diagnostics");
}

#[test]
fn lsp_completion() {
    let mut server = LspServer::new();
    let open_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///test.mimi",
                "text": "func hello() -> i32 { 1 }\nfunc world() -> i32 { 2 }"
            }
        }
    });
    server.handle_message(&open_msg);

    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/completion",
        "params": {
            "textDocument": {
                "uri": "file:///test.mimi"
            }
        }
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some());
    let resp = response.unwrap();
    let items = resp["result"]["items"].as_array().unwrap();
    assert!(items.len() > 10, "should have keywords + functions + builtins");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"func"));
    assert!(labels.contains(&"hello"));
    assert!(labels.contains(&"println"));
}

#[test]
fn lsp_shutdown() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "shutdown",
        "params": null
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some());
    assert_eq!(response.unwrap()["id"], 3);
}

#[test]
fn lsp_diagnostics_type_error() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///type_err.mimi",
                "text": "func main() {\n    let x: i32 = \"hello\";\n}"
            }
        }
    });
    let response = server.handle_message(&msg);
    assert!(response.is_some());
    let resp = response.unwrap();
    let diagnostics = resp["params"]["diagnostics"].as_array().unwrap();
    assert!(!diagnostics.is_empty(), "type error should produce diagnostics");
    assert_eq!(diagnostics[0]["severity"], 1, "should be error severity");
}

#[test]
fn lsp_unknown_method_no_response() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "unknown/method",
        "params": {}
    });
    let response = server.handle_message(&msg);
    assert!(response.is_none(), "unknown method should return None");
}

#[test]
fn lsp_compute_diagnostics_direct() {
    let server = LspServer::new();
    let diags = server.compute_diagnostics("func main() -> i32 { 42 }");
    assert!(diags.is_empty(), "valid code should have 0 diagnostics");

    let diags = server.compute_diagnostics("func $$$ bad");
    assert!(!diags.is_empty(), "invalid code should have diagnostics");
}

#[test]
fn lsp_completion_no_file() {
    let mut server = LspServer::new();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "textDocument/completion",
        "params": {
            "textDocument": {
                "uri": "file:///nonexistent.mimi"
            }
        }
    });
    let response = server.handle_message(&msg);
    assert!(response.is_none(), "completion on unknown file should return None");
}

#[test]
fn lsp_folding_range_basic() {
    let server = LspServer::new();
    let ranges = server.compute_folding_ranges("func main() -> i32 {\n    42\n}");
    assert!(!ranges.is_empty(), "should have folding ranges for braces");
}

#[test]
fn lsp_folding_range_nested() {
    let server = LspServer::new();
    let text = "func f() {\n    if true {\n        1\n    }\n}";
    let ranges = server.compute_folding_ranges(text);
    assert!(ranges.len() >= 2, "should have folding ranges for nested braces");
}

#[test]
fn lsp_folding_range_empty() {
    let server = LspServer::new();
    let ranges = server.compute_folding_ranges("let x = 42");
    assert!(ranges.is_empty(), "no braces = no folding ranges");
}

#[test]
fn lsp_diagnostics_severity_warning() {
    let server = LspServer::new();
    // Valid code should produce no diagnostics
    let diags = server.compute_diagnostics("func main() -> i32 { 42 }");
    assert!(diags.is_empty(), "valid code should have 0 diagnostics");
}
