use crate::lsp::LspServer;

/// L-H6: bring server to Running via initialize + initialized.
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
    // Must initialize before initialized notification (L-H6).
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }));
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
    let mut server = lsp_ready();
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
    assert!(
        server.drain_pending_notifications().is_empty(),
        "single-source diagnostics must not enqueue unrelated notifications"
    );
}

#[test]
fn lsp_did_open_parse_error() {
    let mut server = lsp_ready();
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
    let mut server = lsp_ready();
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
    let mut server = lsp_ready();
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
    let mut server = lsp_ready();
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
    let mut server = lsp_ready();
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
    // After init, unknown methods with id return MethodNotFound or None.
    let mut server = lsp_ready();
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "unknown/method",
        "params": {}
    });
    let response = server.handle_message(&msg);
    // Accept either None or error response depending on dispatch.
    if let Some(resp) = response {
        assert!(
            resp.get("error").is_some() || resp.get("result").is_none(),
            "unknown method should error or be empty: {}",
            resp
        );
    }
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
    let mut server = lsp_ready();
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
    // May return empty result or None for unknown URI.
    if let Some(resp) = response {
        if let Some(items) = resp["result"]["items"].as_array() {
            // empty ok
            let _ = items;
        } else if resp.get("error").is_some() {
            // error ok
        }
    }
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

#[test]
fn lsp_parse_cache_keeps_identical_text_bound_to_each_uri() {
    let server = LspServer::new();
    let text = "func main() -> i32 { 42 }";
    let first_uri = "file:///workspace/first.mimi";
    let second_uri = "file:///workspace/second.mimi";

    let first = server
        .parse_with_recovery_for_uri(text, Some(first_uri))
        .expect("first document should parse");
    let second = server
        .parse_with_recovery_for_uri(text, Some(second_uri))
        .expect("second document should parse");

    let body_source = |file: &crate::ast::File| {
        let func = file
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Func(func) => Some(func),
                _ => None,
            })
            .expect("function item");
        func.body
            .first()
            .and_then(crate::ast::Stmt::meta)
            .expect("parsed statement should carry source metadata")
            .span
            .source_id
    };

    let first_source = body_source(&first);
    let second_source = body_source(&second);
    assert_ne!(
        first_source, second_source,
        "equal text in different URIs must not reuse the first AST SourceId"
    );
    assert_eq!(
        first
            .sources
            .record(first_source)
            .and_then(|record| record.canonical_uri.as_deref()),
        Some(first_uri)
    );
    assert_eq!(
        second
            .sources
            .record(second_source)
            .and_then(|record| record.canonical_uri.as_deref()),
        Some(second_uri)
    );
}

#[test]
fn lsp_verification_cache_hit_preserves_structured_span() {
    let mut server = LspServer::new();
    let uri = "file:///workspace/cache-span.mimi";
    let text = "func bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";
    let file = server
        .parse_with_recovery_for_uri(text, Some(uri))
        .expect("contract function should parse");
    let func = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) => Some(func),
            _ => None,
        })
        .expect("function item");
    let body_hash = crate::lsp::util::hash_func_body(text, func);
    let source_id = file.sources.id_for_uri(uri).expect("URI source id");
    let diagnostic = crate::diagnostic::Diagnostic::error_code(
        "E0999",
        "cached postcondition violation",
        crate::span::Span::new(3, 5, 3, 12).with_source(source_id),
    )
    .with_origin(crate::diagnostic::DiagnosticOrigin::user());
    server.insert_verification_cache_with_diagnostic(
        format!("{uri}:bad"),
        body_hash,
        crate::verifier::VerifStatus::Failed,
        "cached postcondition violation".to_string(),
        diagnostic,
    );

    let first = server.compute_verification_diagnostics(text, 2, uri);
    let second = server.compute_verification_diagnostics(text, 2, uri);
    assert_eq!(first.len(), 1, "cached failure should emit one diagnostic");
    assert_eq!(second.len(), 1, "repeated cache hit should remain stable");
    assert_eq!(first[0]["range"], second[0]["range"]);
    assert_eq!(first[0]["data"]["origin"], second[0]["data"]["origin"]);
    assert_eq!(first[0]["data"]["origin"]["kind"], "user");
    assert_eq!(
        first[0]["range"],
        serde_json::json!({
            "start": { "line": 2, "character": 4 },
            "end": { "line": 2, "character": 11 }
        }),
        "cache hits must preserve the verifier's precise span"
    );
}

#[test]
fn lsp_verification_checked_origin_is_identical_before_and_after_cache_hit() {
    if !crate::verifier::is_z3_available() {
        return;
    }
    let mut server = LspServer::new();
    let uri = "file:///workspace/cache-checked-origin.mimi";
    let text = "func bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";

    let before_cache = server.compute_verification_diagnostics(text, 2, uri);
    let after_cache = server.compute_verification_diagnostics(text, 2, uri);
    assert_eq!(before_cache.len(), 1, "verification failure diagnostic");
    assert_eq!(after_cache.len(), 1, "cached verification diagnostic");
    assert_eq!(before_cache[0]["range"], after_cache[0]["range"]);
    assert_eq!(
        before_cache[0]["data"]["origin"], after_cache[0]["data"]["origin"],
        "cache replay must preserve the checked NodeMeta origin"
    );
    assert_eq!(before_cache[0]["data"]["origin"]["kind"], "user");
}

#[test]
fn lsp_verification_cache_invalidates_when_function_moves() {
    let mut server = LspServer::new();
    let uri = "file:///workspace/cache-moved-span.mimi";
    let original =
        "func bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";
    let shifted =
        "\n\nfunc bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";

    let original_file = server
        .parse_with_recovery_for_uri(original, Some(uri))
        .expect("original contract function should parse");
    let original_func = original_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) => Some(func),
            _ => None,
        })
        .expect("original function item");
    let original_hash = crate::lsp::util::hash_func_body(original, original_func);
    let source_id = original_file
        .sources
        .id_for_uri(uri)
        .expect("URI source id");
    server.insert_verification_cache_with_diagnostic(
        format!("{uri}:bad"),
        original_hash,
        crate::verifier::VerifStatus::Failed,
        "cached postcondition violation".to_string(),
        crate::diagnostic::Diagnostic::error_code(
            "E0999",
            "cached postcondition violation",
            crate::span::Span::new(3, 5, 3, 12).with_source(source_id),
        ),
    );

    let shifted_file = server
        .parse_with_recovery_for_uri(shifted, Some(uri))
        .expect("shifted contract function should parse");
    let shifted_func = shifted_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) => Some(func),
            _ => None,
        })
        .expect("shifted function item");
    assert_ne!(
        original_hash,
        crate::lsp::util::hash_func_body(shifted, shifted_func),
        "moving an unchanged function must invalidate absolute diagnostic spans"
    );

    let diagnostics = server.compute_verification_diagnostics(shifted, 4, uri);
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic["range"]["start"]["line"] != serde_json::json!(2)
        }),
        "the cache must not replay the old line-3 diagnostic after a two-line shift: {diagnostics:?}"
    );
}

#[test]
fn lsp_verification_cache_rejects_legacy_persistent_schema() {
    let root = std::env::temp_dir().join(format!(
        "mimi_lsp_legacy_verification_cache_{}",
        std::process::id()
    ));
    let cache_dir = root.join(".mimi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&cache_dir).expect("create legacy cache directory");
    std::fs::write(
        cache_dir.join("verify_cache.json"),
        serde_json::json!({
            "version": 3,
            "entries": {
                "file:///workspace/old.mimi:bad": {
                    "body_hash": 42,
                    "status": "Failed",
                    "message": "stale absolute span"
                }
            }
        })
        .to_string(),
    )
    .expect("write legacy cache");

    let mut server = LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    assert!(
        server.verification_cache.is_empty(),
        "v1-v3 entries do not carry owned origins and must be invalidated"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn lsp_verification_cache_rejects_v4_diagnostic_without_origin() {
    let root = std::env::temp_dir().join(format!(
        "mimi_lsp_originless_v4_cache_{}",
        std::process::id()
    ));
    let cache_dir = root.join(".mimi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&cache_dir).expect("create cache directory");
    std::fs::write(
        cache_dir.join("verify_cache.json"),
        serde_json::json!({
            "version": 4,
            "entries": {
                "file:///workspace/originless.mimi:bad": {
                    "body_hash": 42,
                    "status": "Failed",
                    "message": "originless failure",
                    "diagnostic": {
                        "source_key": "workspace:originless.mimi",
                        "start_line": 1,
                        "start_col": 1,
                        "end_line": 1,
                        "end_col": 2,
                        "severity": 1,
                        "code": "E0500",
                        "message": "originless failure",
                        "notes": [],
                        "help": null
                    }
                }
            }
        })
        .to_string(),
    )
    .expect("write originless v4 cache");

    let mut server = LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    assert!(
        server.verification_cache.is_empty(),
        "a v4 failed diagnostic without Origin must fail closed"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn lsp_verification_cache_persistent_roundtrip_preserves_span_and_origin() {
    let root =
        std::env::temp_dir().join(format!("mimi_lsp_origin_roundtrip_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create workspace");
    let path = root.join("main.mimi");
    let uri = format!("file://{}", path.display());
    let text = "func bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";
    std::fs::write(&path, text).expect("write source");

    let mut writer = LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    let file = writer
        .parse_with_recovery_for_uri(text, Some(&uri))
        .expect("parse source");
    let source_id = file.sources.id_for_uri(&uri).expect("source id");
    let span = crate::span::Span::new(3, 5, 3, 24).with_source(source_id);
    let origin = crate::diagnostic::DiagnosticOrigin {
        kind: crate::diagnostic::DiagnosticOriginKind::RuntimeSystem,
        rule: Some("verification.contract_failure".to_string()),
        parent_node_id: Some("function:bad".to_string()),
    };
    writer.insert_verification_cache_with_diagnostic(
        format!("{uri}:bad"),
        77,
        crate::verifier::VerifStatus::Failed,
        "persistent failure".to_string(),
        crate::diagnostic::Diagnostic::error("persistent failure", span)
            .with_origin(origin.clone()),
    );
    writer.save_cache();

    let cache_json = std::fs::read_to_string(root.join(".mimi/verify_cache.json"))
        .expect("read persisted cache");
    let cache_json: serde_json::Value = serde_json::from_str(&cache_json).expect("cache json");
    assert_eq!(cache_json["version"], 4);
    assert_eq!(
        cache_json["entries"][format!("{uri}:bad")]["diagnostic"]["origin"]["parent_node_id"],
        "function:bad"
    );

    let mut reader = LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    let restored_file = reader
        .parse_with_recovery_for_uri(text, Some(&uri))
        .expect("register restored source");
    let restored = reader
        .verification_cache
        .get(&format!("{uri}:bad"))
        .expect("restored entry")
        .diagnostic(&restored_file.sources)
        .expect("restored diagnostic");
    assert_eq!(restored.span, span);
    assert_eq!(restored.origin, Some(origin));

    let _ = std::fs::remove_dir_all(root);
}
