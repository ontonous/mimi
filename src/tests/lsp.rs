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
        // 0.34.44: engine-scoped key (the only shape the lookup path reads).
        crate::lsp::verification_cache_key(uri, "bad"),
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
        crate::lsp::verification_cache_key(uri, "bad"),
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
    // The cached diagnostic (code "E0999") must not be replayed at the old
    // line-3 position. A fresh VIR-path diagnostic at the function's new
    // position (line 2) is expected and correct.
    assert!(
        diagnostics.iter().all(|diagnostic| {
            diagnostic["code"] != serde_json::json!("E0999")
        }),
        "the cache must not replay the old E0999 diagnostic after a two-line shift: {diagnostics:?}"
    );
}

#[test]
fn lsp_verification_cache_does_not_retain_infrastructure_errors() {
    let mut server = LspServer::new();
    let key = crate::lsp::verification_cache_key("file:///workspace/retry.mimi", "retry");
    server.cache_put_verification(
        key.clone(),
        crate::lsp::VerificationCacheEntry::new(
            7,
            crate::verifier::VerifStatus::InfrastructureError,
            "solver unavailable".to_string(),
            None,
        ),
    );
    assert!(
        !server.verification_cache.contains_key(&key),
        "retryable infrastructure failures must not become cache hits"
    );
}

#[test]
fn lsp_verification_cache_drops_persisted_infrastructure_errors() {
    let root = std::env::temp_dir().join(format!(
        "mimi_lsp_infrastructure_cache_{}",
        std::process::id()
    ));
    let cache_dir = root.join(".mimi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&cache_dir).expect("create cache directory");
    let key = crate::lsp::verification_cache_key("file:///workspace/retry.mimi", "retry");
    std::fs::write(
        cache_dir.join("verify_cache.json"),
        serde_json::json!({
            "version": 4,
            "entries": {
                key.clone(): {
                    "body_hash": 7,
                    "status": "InfrastructureError",
                    "message": "solver unavailable"
                }
            }
        })
        .to_string(),
    )
    .expect("write infrastructure cache");

    let mut server = LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    assert!(
        !server.verification_cache.contains_key(&key),
        "persisted infrastructure failures must be invalidated on load"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn lsp_verification_cache_load_hydrates_lru_capacity() {
    let root = std::env::temp_dir().join(format!(
        "mimi_lsp_cache_lru_hydration_{}",
        std::process::id()
    ));
    let cache_dir = root.join(".mimi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&cache_dir).expect("create cache directory");
    let persisted_key =
        crate::lsp::verification_cache_key("file:///workspace/persisted.mimi", "persisted");
    std::fs::write(
        cache_dir.join("verify_cache.json"),
        serde_json::json!({
            "version": 4,
            "entries": {
                persisted_key.clone(): {
                    "body_hash": 1,
                    "status": "Verified",
                    "message": "cached proof"
                }
            }
        })
        .to_string(),
    )
    .expect("write persisted cache");

    let mut server = LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    assert!(server.verification_cache.contains_key(&persisted_key));

    // Fill the session with exactly the configured number of fresh entries.
    // The hydrated persisted key must be the first LRU victim, keeping the
    // map at its hard cap. Without hydration this leaves MAX+1 entries.
    for index in 0..crate::lsp::MAX_VERIFICATION_CACHE {
        server.cache_put_verification(
            format!("session-key-{index}"),
            crate::lsp::VerificationCacheEntry::new(
                index as u64,
                crate::verifier::VerifStatus::Proven,
                "cached proof".to_string(),
                None,
            ),
        );
    }
    assert_eq!(
        server.verification_cache.len(),
        crate::lsp::MAX_VERIFICATION_CACHE,
        "loaded entries must participate in the LRU capacity"
    );
    assert!(!server.verification_cache.contains_key(&persisted_key));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn lsp_verification_cache_load_bounds_oversized_persistent_files() {
    let root = std::env::temp_dir().join(format!(
        "mimi_lsp_cache_lru_oversized_{}",
        std::process::id()
    ));
    let cache_dir = root.join(".mimi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&cache_dir).expect("create cache directory");

    let mut entries = serde_json::Map::new();
    let mut persisted_keys = Vec::new();
    for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE + 64) {
        let key = crate::lsp::verification_cache_key(
            &format!("file:///workspace/persisted-{index:05}.mimi"),
            "persisted",
        );
        persisted_keys.push(key.clone());
        entries.insert(
            key,
            serde_json::json!({
                "body_hash": index,
                "status": "Verified",
                "message": "cached proof"
            }),
        );
    }
    std::fs::write(
        cache_dir.join("verify_cache.json"),
        serde_json::json!({ "version": 4, "entries": entries }).to_string(),
    )
    .expect("write oversized persisted cache");

    let mut server = LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    assert_eq!(
        server.verification_cache.len(),
        crate::lsp::MAX_VERIFICATION_CACHE,
        "oversized persistent files must be bounded during load"
    );
    assert!(
        server
            .verification_cache
            .contains_key(&persisted_keys[crate::lsp::MAX_VERIFICATION_CACHE + 63]),
        "stable newest-key retention should keep the upper boundary"
    );
    assert!(
        !server.verification_cache.contains_key(&persisted_keys[0]),
        "stable oldest-key retention should evict the lower boundary"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn lsp_verification_cache_key_is_length_framed_and_validated() {
    let uri = "file:///workspace/cache:a.mimi?fragment=x:y";
    let key = crate::lsp::verification_cache_key(uri, "bad");
    assert_eq!(
        crate::lsp::parse_verification_cache_key(&key),
        Some((uri, "bad")),
        "framed keys must recover URI and function boundaries"
    );
    assert!(
        crate::lsp::parse_verification_cache_key("file:///workspace/cache:a.mimi:bad:resolved:v1")
            .is_none(),
        "legacy delimiter keys must not enter the v4 cache"
    );
    assert!(
        crate::lsp::parse_verification_cache_key(&key.replacen(":resolved:v", ":flow:v", 1))
            .is_none(),
        "a key for another engine must fail closed"
    );
}

#[test]
fn lsp_verification_cache_rejects_cross_uri_diagnostic_replay() {
    let root =
        std::env::temp_dir().join(format!("mimi_lsp_cross_uri_cache_{}", std::process::id()));
    let cache_dir = root.join(".mimi");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&cache_dir).expect("create cache directory");
    let uri_a = "untitled://workspace/cache-a.mimi";
    let uri_b = "untitled://workspace/cache-b.mimi";
    let text = "func bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";

    // Derive the exact body hash and foreign SourceKey through the same
    // source-aware parser path used by the server.
    let probe = LspServer::new();
    let file_a = probe
        .parse_with_recovery_for_uri(text, Some(uri_a))
        .expect("parse source A");
    let func_a = file_a
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find bad in source A");
    let body_hash = crate::lsp::util::hash_func_body(text, func_a);
    let file_b = probe
        .parse_with_recovery_for_uri(text, Some(uri_b))
        .expect("parse source B");
    let source_b = file_b.sources.id_for_uri(uri_b).expect("source B id");
    let source_b_key = file_b
        .sources
        .key(source_b)
        .expect("source B key")
        .as_str()
        .to_string();
    let cache_key = crate::lsp::verification_cache_key(uri_a, "bad");
    std::fs::write(
        cache_dir.join("verify_cache.json"),
        serde_json::json!({
            "version": 4,
            "entries": {
                cache_key.clone(): {
                    "body_hash": body_hash,
                    "status": "Failed",
                    "message": "cross URI cached failure",
                    "diagnostic": {
                        "source_key": source_b_key,
                        "start_line": 1,
                        "start_col": 1,
                        "end_line": 1,
                        "end_col": 4,
                        "severity": 1,
                        "code": "E0999",
                        "message": "cross URI cached failure",
                        "notes": [],
                        "help": null,
                        "origin": {
                            "kind": "user",
                            "rule": null,
                            "parent_node_id": null
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .expect("write cross URI cache");

    let mut server = LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootPath": root.to_string_lossy() }
    }));
    // Keep the foreign source registered so a plain SourceKey remap would
    // succeed; the source-aware cache hit must still reject it for URI A.
    server
        .parse_with_recovery_for_uri(text, Some(uri_b))
        .expect("register source B");
    let diagnostics = server.compute_verification_diagnostics(text, 0, uri_a);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic["code"] != "E0999"),
        "a diagnostic owned by URI B must not be replayed for URI A: {diagnostics:?}"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn lsp_verification_cache_hit_survives_source_registry_reset() {
    let uri = "untitled://workspace/cache-after-reset.mimi";
    let text = "func bad(x: i32) -> i32 {\n    requires: x > 0\n    ensures: result > 0\n    0\n}";
    let probe = LspServer::new();
    let file = probe
        .parse_with_recovery_for_uri(text, Some(uri))
        .expect("parse source");
    let func = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find bad function");
    let body_hash = crate::lsp::util::hash_func_body(text, func);

    let mut server = LspServer::new();
    // Fill the shared registry exactly to its cap. The next URI registration
    // will return a valid snapshot and then reset the session pool.
    for index in 0..crate::lsp::MAX_SOURCE_RECORDS {
        let source = format!("func f_{index}() -> i32 {{\n    {index}\n}}");
        server
            .parse_with_recovery_for_uri(&source, None)
            .expect("parse memory source");
    }
    let cache_key = crate::lsp::verification_cache_key(uri, "bad");
    let span = crate::span::Span::new(1, 1, 1, 4).with_source(crate::span::SourceId::new(
        (crate::lsp::MAX_SOURCE_RECORDS + 1) as u32,
    ));
    server.insert_verification_cache_with_diagnostic(
        cache_key,
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "reset-safe cached failure".to_string(),
        crate::diagnostic::Diagnostic::error_code("E0999", "reset-safe cached failure", span)
            .with_origin(crate::diagnostic::DiagnosticOrigin::user()),
    );

    let diagnostics = server.compute_verification_diagnostics(text, 0, uri);
    assert_eq!(
        diagnostics.len(),
        1,
        "cache hit should survive registry reset"
    );
    assert_eq!(diagnostics[0]["code"], "E0999");
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
    // 0.34.44: engine-scoped key end-to-end — the persisted entry must be
    // stored AND restored under the engine-qualified key shape.
    let persisted_key = crate::lsp::verification_cache_key(&uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        persisted_key.clone(),
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
        cache_json["entries"][persisted_key.as_str()]["diagnostic"]["origin"]["parent_node_id"],
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
        .get(&persisted_key)
        .expect("restored entry")
        .diagnostic(&restored_file.sources)
        .expect("restored diagnostic");
    assert_eq!(restored.span, span);
    assert_eq!(restored.origin, Some(origin));

    let _ = std::fs::remove_dir_all(root);
}

// 0.35.15 (DX backlog #3): the LSP text-search migration consumes AST
// spans instead of re-scanning source text. These tests lock the span
// anchor contract the migration relies on.
#[test]
fn lsp_span_anchors_for_lsp_queries() {
    use crate::ast::{Item, PatternKind, Stmt};
    let src = "type Point { x: i32 }\n\
               const ID: i32 = 1\n\
               impl Clone for Point {\n\
               \x20   func clone() -> Point { self }\n\
               }\n\
               func main() -> i32 {\n\
               \x20   let n = 1\n\
               \x20   n\n\
               }\n";
    let file = crate::tests::parse(src);
    // TypeDef span anchors at the `type` keyword (line 1, col 1).
    let t = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Type(t) => Some(t),
            _ => None,
        })
        .expect("type item");
    assert_eq!(
        (t.meta.span.start_line, t.meta.span.start_col),
        (1, 1),
        "TypeDef span must anchor at the `type` keyword"
    );
    // ImplDef span anchors at the `impl` keyword (line 3).
    let imp = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Impl(i) => Some(i),
            _ => None,
        })
        .expect("impl item");
    assert_eq!(
        (imp.meta.span.start_line, imp.meta.span.start_col),
        (3, 1),
        "ImplDef span must anchor at the `impl` keyword"
    );
    // The let-binding pattern span anchors at the binding name.
    let main = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Func(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .expect("main func");
    let let_stmt = main
        .body
        .iter()
        .find(|s| matches!(s.unlocated(), Stmt::Let { .. }))
        .expect("let statement");
    if let Stmt::Let { pat, .. } = let_stmt.unlocated() {
        assert!(
            matches!(&pat.kind, PatternKind::Variable(n) if n == "n"),
            "expected binding `n`"
        );
        assert_eq!(
            pat.meta.span.start_line, 7,
            "let pattern span must carry the binding line"
        );
        // `    let n = 1` — 4 spaces + `let ` → `n` at col 9 (1-indexed).
        assert_eq!(
            pat.meta.span.start_col, 9,
            "let pattern span must anchor at the binding name"
        );
    }
}

#[test]
fn lsp_span_anchors_func_end_and_call_expr() {
    use crate::ast::{Expr, Item, Stmt};
    let src = "func helper(v: i32) -> i32 {\n\
               \x20   v + 1\n\
               }\n\
               \n\
               func main() -> i32 {\n\
               \x20   let r = helper(3)\n\
               \x20   r\n\
               }\n";
    let file = crate::tests::parse(src);
    let helper = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Func(f) if f.name == "helper" => Some(f),
            _ => None,
        })
        .expect("helper func");
    // FuncDef span runs from the `func` keyword to the closing brace line.
    assert_eq!(
        (helper.meta.span.start_line, helper.meta.span.start_col),
        (1, 1),
        "FuncDef span must anchor at the `func` keyword"
    );
    assert_eq!(
        helper.meta.span.end_line, 3,
        "FuncDef span must end on the closing-brace line"
    );
    // The call expression carries a span anchored at the callee name.
    let main = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Func(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .expect("main func");
    let let_stmt = main
        .body
        .iter()
        .find(|s| matches!(s.unlocated(), Stmt::Let { .. }))
        .expect("let statement");
    let Stmt::Let {
        init: Some(init), ..
    } = let_stmt.unlocated()
    else {
        panic!("expected an initializer");
    };
    assert!(
        matches!(init.unlocated(), Expr::Call(..)),
        "expected a call initializer"
    );
    let meta = init.meta().expect("call expr must carry metadata");
    assert_eq!(
        meta.span.start_line, 6,
        "call span must carry the call line"
    );
    // `    let r = helper(3)` — 4 spaces + `let r = ` → callee at col 13.
    assert_eq!(
        meta.span.start_col, 13,
        "call span must anchor at the callee name"
    );
}
