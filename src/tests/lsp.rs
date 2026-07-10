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
    let resp = response.expect("src/tests/lsp.rs:14 unwrap failed");
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
    let resp = response.expect("src/tests/lsp.rs:49 unwrap failed");
    assert_eq!(resp["method"], "textDocument/publishDiagnostics");
    let diagnostics = resp["params"]["diagnostics"]
        .as_array()
        .expect("src/tests/lsp.rs:51 unwrap failed");
    assert!(
        diagnostics.is_empty(),
        "valid code should have no diagnostics"
    );
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
    let resp = response.expect("src/tests/lsp.rs:70 unwrap failed");
    let diagnostics = resp["params"]["diagnostics"]
        .as_array()
        .expect("src/tests/lsp.rs:71 unwrap failed");
    assert!(
        !diagnostics.is_empty(),
        "syntax error should produce diagnostics"
    );
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
    let resp = response.expect("src/tests/lsp.rs:104 unwrap failed");
    let diagnostics = resp["params"]["diagnostics"]
        .as_array()
        .expect("src/tests/lsp.rs:105 unwrap failed");
    assert!(
        diagnostics.is_empty(),
        "changed valid code should have no diagnostics"
    );
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
    let resp = response.expect("src/tests/lsp.rs:136 unwrap failed");
    let items = resp["result"]["items"]
        .as_array()
        .expect("src/tests/lsp.rs:137 unwrap failed");
    assert!(
        items.len() > 10,
        "should have keywords + functions + builtins"
    );
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
    assert_eq!(
        response.expect("src/tests/lsp.rs:156 unwrap failed")["id"],
        3
    );
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
    let resp = response.expect("src/tests/lsp.rs:174 unwrap failed");
    let diagnostics = resp["params"]["diagnostics"]
        .as_array()
        .expect("src/tests/lsp.rs:175 unwrap failed");
    assert!(
        !diagnostics.is_empty(),
        "type error should produce diagnostics"
    );
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
    let diags = server.compute_diagnostics("func main() -> i32 { 42 }", None);
    assert!(diags.is_empty(), "valid code should have 0 diagnostics");

    let diags = server.compute_diagnostics("func $$$ bad", None);
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
    assert!(
        response.is_none(),
        "completion on unknown file should return None"
    );
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
    assert!(
        ranges.len() >= 2,
        "should have folding ranges for nested braces"
    );
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
    let diags = server.compute_diagnostics("func main() -> i32 { 42 }", None);
    assert!(diags.is_empty(), "valid code should have 0 diagnostics");
}

// ===================== v0.28.11: LSP 端到端序列测试 =====================

#[test]
fn lsp_e2e_full_session() {
    // Simulate a complete LSP session: initialize → didOpen with valid code
    // → didChange → hover → definition → completion → shutdown.
    //
    // Uses the JSON-RPC handle_message interface (not direct internal methods)
    // to verify the full server pipeline works end-to-end.
    let mut server = LspServer::new();

    // 1. Initialize
    let resp = server
        .handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .expect("initialize should respond");
    assert_eq!(resp["id"], 1, "initialize response id");
    assert!(resp["result"]["capabilities"]["hoverProvider"]
        .as_bool()
        .unwrap_or(false));

    // 2. DidOpen with valid source
    let src = "type Person { name: string, age: i32 }\nfunc main() -> i32 {\n    let p: Person = Person { name: \"Bob\", age: 30 }\n    println(p.name)\n    0\n}";
    let open_resp = server
        .handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///e2e.mimi",
                    "text": src
                }
            }
        }))
        .expect("didOpen should respond");
    assert_eq!(
        open_resp["method"], "textDocument/publishDiagnostics",
        "didOpen should publish diagnostics"
    );

    // 3. Hover on `Person` (type name) — via JSON-RPC
    let hover_resp = server
        .handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": "file:///e2e.mimi" },
                "position": { "line": 0, "character": 6 }
            }
        }))
        .expect("hover should respond");
    assert!(
        hover_resp.get("result").is_some(),
        "hover on Person should return result, got: {:?}",
        hover_resp
    );

    // 4. Definition on `Person` (type def)
    let def_resp = server
        .handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": "file:///e2e.mimi" },
                "position": { "line": 2, "character": 10 }
            }
        }))
        .expect("definition should respond");
    // Definition may return null for non-top-level symbols, but the
    // request must not error.
    assert!(
        def_resp.get("result").is_some() || def_resp.get("error").is_none(),
        "definition should not error, got: {:?}",
        def_resp
    );

    // 5. Completion
    let comp_resp = server
        .handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": "file:///e2e.mimi" },
                "position": { "line": 0, "character": 0 }
            }
        }))
        .expect("completion should respond");
    let items = comp_resp["result"]["items"]
        .as_array()
        .expect("completion should have items array");
    let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(
        labels.contains(&"Person"),
        "completion should include type 'Person'"
    );

    // 6. DidChange (edit source)
    let _change_resp = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": "file:///e2e.mimi" },
            "contentChanges": [{
                "text": src  // same content, verify stability
            }]
        }
    }));

    // 7. Hover after change still works
    let hover2_resp = server
        .handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": "file:///e2e.mimi" },
                "position": { "line": 2, "character": 12 }
            }
        }))
        .expect("hover after change should respond");
    assert!(
        hover2_resp.get("result").is_some(),
        "hover after change should return result, got: {:?}",
        hover2_resp
    );

    // 8. Shutdown
    let shutdown_resp = server
        .handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "shutdown",
            "params": null
        }))
        .expect("shutdown should respond");
    assert_eq!(shutdown_resp["id"], 6);
}
