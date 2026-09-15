use std::fs;
use std::path::{Path, PathBuf};

fn temp_workspace(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "mimi_diagnostic_routing_{}_{}",
        label,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create diagnostic routing workspace");
    path
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn diagnostic_path(file: &crate::ast::File, diagnostic: &crate::diagnostic::Diagnostic) -> PathBuf {
    file.sources
        .record(diagnostic.span.source_id)
        .and_then(|record| record.disk_path.as_deref())
        .expect("diagnostic source has a disk path")
        .to_path_buf()
}

#[test]
fn checker_dependency_error_retains_dependency_source() {
    let root = temp_workspace("checker");
    let main_path = root.join("main.mimi");
    let dep_path = root.join("dep.mimi");
    fs::write(&main_path, "use dep\n\nfunc main() -> i32 {\n    0\n}\n")
        .expect("write checker main");
    fs::write(
        &dep_path,
        "pub func broken() -> i32 {\n    missing_dep\n}\n",
    )
    .expect("write checker dependency");

    let mut loader = crate::loader::ModuleLoader::new(root.clone());
    loader.load_main(&main_path).expect("load checker graph");
    let file = loader.merge_all().expect("merge checker graph");
    let diagnostics = crate::core::check(&file).expect_err("both files contain an error");
    let mut routed = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message == "undefined variable 'missing_dep'")
        .collect::<Vec<_>>();
    routed.sort_by_key(|diagnostic| diagnostic.message.as_str());

    assert_eq!(routed.len(), 1, "dependency checker diagnostic");
    assert!(routed.iter().all(|diagnostic| {
        diagnostic.span.source_id.is_known()
            && diagnostic.span.start_line > 0
            && diagnostic.span.start_col > 0
    }));
    let routed_paths = routed
        .iter()
        .map(|diagnostic| {
            file.sources
                .record(diagnostic.span.source_id)
                .and_then(|record| record.disk_path.as_deref())
                .expect("diagnostic source has a disk path")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert!(routed_paths.iter().any(|path| path.ends_with("dep.mimi")));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn checker_alias_cycles_and_equal_messages_retain_each_merged_source() {
    let root = temp_workspace("alias_cycle_sources");
    let a_path = root.join("a.mimi");
    let b_path = root.join("b.mimi");
    fs::write(
        &a_path,
        "type A = B\nfunc broken_a() -> i32 { missing_value }\n",
    )
    .expect("write alias source a");
    fs::write(
        &b_path,
        "type B = A\nfunc broken_b() -> i32 { missing_value }\n",
    )
    .expect("write alias source b");

    let mut loader = crate::loader::ModuleLoader::new(root.clone());
    loader.load_main(&a_path).expect("load alias source a");
    loader.load_main(&b_path).expect("load alias source b");
    let file = loader.merge_all().expect("merge alias sources");
    let diagnostics = crate::core::check(&file).expect_err("alias graph must fail");

    for (alias, expected_path) in [("A", &a_path), ("B", &b_path)] {
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code.as_deref() == Some(crate::diagnostic::codes::E0409)
                    && diagnostic.message.contains(&format!("'{alias}'"))
            })
            .unwrap_or_else(|| panic!("missing alias-cycle diagnostic for {alias}"));
        assert_eq!(
            diagnostic_path(&file, diagnostic).as_path(),
            expected_path.as_path()
        );
    }

    let repeated = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message == "undefined variable 'missing_value'")
        .collect::<Vec<_>>();
    assert_eq!(repeated.len(), 2, "same prose at two sources must survive");
    let routed = repeated
        .iter()
        .map(|diagnostic| diagnostic_path(&file, diagnostic))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(routed, std::collections::HashSet::from([a_path, b_path]));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn checker_flow_warnings_use_declaration_sources_after_merge() {
    let root = temp_workspace("flow_warning_sources");
    let flow_path = root.join("flow.mimi");
    let main_path = root.join("main.mimi");
    fs::write(
        &flow_path,
        concat!(
            "flow Worker {\n",
            "    state Start\n",
            "    state Idle\n",
            "    transition tick(Start) -> Start {\n",
            "        { return Start {} }\n",
            "    }\n",
            "}\n",
        ),
    )
    .expect("write flow warnings source");
    fs::write(&main_path, "func main() -> i32 { 0 }\n").expect("write main source");

    let mut loader = crate::loader::ModuleLoader::new(root.clone());
    loader.load_main(&flow_path).expect("load flow source");
    loader.load_main(&main_path).expect("load main source");
    let file = loader.merge_all().expect("merge warning sources");
    let mut checker = crate::core::Checker::new(&file);
    checker.check().expect("warning fixture must type-check");

    for code in [
        crate::diagnostic::codes::W011,
        crate::diagnostic::codes::W0400,
        crate::diagnostic::codes::W0401,
    ] {
        let diagnostic = checker
            .warnings
            .iter()
            .find(|diagnostic| diagnostic.code.as_deref() == Some(code))
            .unwrap_or_else(|| panic!("missing warning {code}"));
        assert_eq!(
            diagnostic_path(&file, diagnostic).as_path(),
            flow_path.as_path()
        );
        assert!(diagnostic.span.source_id.is_known());
        let expected_line = if code == crate::diagnostic::codes::W011 {
            1
        } else {
            3
        };
        assert_eq!(
            diagnostic.span.start_line, expected_line,
            "{code} must use its declaration metadata"
        );
    }

    let _ = fs::remove_dir_all(root);
}

#[test]
fn verifier_dependency_failure_retains_dependency_source() {
    let Ok(mut verifier) = crate::verifier::Verifier::new() else {
        return;
    };
    let root = temp_workspace("verifier");
    let main_path = root.join("main.mimi");
    let dep_path = root.join("dep.mimi");
    fs::write(
        &main_path,
        "use dep\n\nfunc main() -> i32 {\n    broken(1)\n}\n",
    )
    .expect("write verifier main");
    fs::write(
        &dep_path,
        concat!(
            "pub func broken(x: i32) -> i32 {\n",
            "    requires: x > 0\n",
            "    ensures: result > 0\n",
            "    0\n",
            "}\n"
        ),
    )
    .expect("write verifier dependency");

    let mut loader = crate::loader::ModuleLoader::new(root.clone());
    loader.load_main(&main_path).expect("load verifier graph");
    let file = loader.merge_all().expect("merge verifier graph");
    let result = verifier
        .verify_file(&file)
        .into_iter()
        .find(|result| result.func_name == "broken")
        .expect("dependency verification result");
    assert_eq!(result.status, crate::verifier::VerifStatus::Failed);
    let diagnostic = result.diagnostic.expect("structured verifier diagnostic");
    assert!(diagnostic.span.source_id.is_known());
    assert!(diagnostic.span.start_line > 0 && diagnostic.span.start_col > 0);
    let record = file
        .sources
        .record(diagnostic.span.source_id)
        .expect("verifier diagnostic source record");
    assert!(record
        .disk_path
        .as_deref()
        .is_some_and(|path| path.ends_with("dep.mimi")));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn ownership_ledger_actions_and_merges_are_source_aware() {
    let root = temp_workspace("ownership");
    let main_path = root.join("main.mimi");
    fs::write(
        &main_path,
        concat!(
            "cap File\n",
            "func close(flag: bool, f: cap File) -> i32 {\n",
            "    if flag { drop(f) } else { drop(f) }\n",
            "    0\n",
            "}\n",
            "func main() -> i32 { 0 }\n"
        ),
    )
    .expect("write ownership source");
    let mut loader = crate::loader::ModuleLoader::new(root.clone());
    loader.load_main(&main_path).expect("load ownership source");
    let file = loader.merge_all().expect("merge ownership source");
    let program = crate::core::check_program(&file).expect("check ownership source");
    let owner = crate::core::NodeId("function:close".into());
    let analysis = program
        .resource_analysis(&owner)
        .expect("close resource analysis");
    assert!(analysis.actions.iter().all(|action| {
        action.span.source_id.is_known() && action.span.start_line > 0 && action.span.start_col > 0
    }));
    let cfg = program.callable_cfg(&owner).expect("close cfg");
    let merges = analysis.branch_merges(cfg);
    assert!(merges.iter().all(|merge| {
        merge.span.source_id.is_known() && merge.span.start_line > 0 && merge.span.start_col > 0
    }));
    assert!(analysis
        .actions
        .iter()
        .filter(|action| action.kind == crate::core::CanonicalActionKind::Drop)
        .all(|action| action.span.start_line == 3));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_publishes_dependency_diagnostic_on_dependency_uri_only() {
    let root = temp_workspace("lsp");
    let main_path = root.join("main.mimi");
    let dep_path = root.join("dep.mimi");
    let main_text = "use dep\n\nfunc main() -> i32 {\n    0\n}\n";
    fs::write(&main_path, main_text).expect("write LSP main");
    fs::write(
        &dep_path,
        "pub func broken() -> i32 {\n    missing_dep\n}\n",
    )
    .expect("write LSP dependency");
    let root_uri = file_uri(&root);
    let main_uri = file_uri(&main_path);
    let dep_uri = file_uri(&dep_path);

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let notifications = server.compute_diagnostic_notifications(main_text, &main_uri);
    let dependency = notifications
        .iter()
        .find(|notification| notification["params"]["uri"].as_str() == Some(dep_uri.as_str()))
        .expect("dependency publishDiagnostics notification");
    let diagnostics = dependency["params"]["diagnostics"]
        .as_array()
        .expect("dependency diagnostics array");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["message"]
            .as_str()
            .is_some_and(|message| message.contains("undefined variable 'missing_dep'"))
            && diagnostic["range"]["start"]["line"] == 1
            && diagnostic["range"]["start"]["character"] == 4
    }));
    assert!(notifications.iter().all(|notification| {
        notification["params"]["uri"].as_str() != Some(main_uri.as_str())
            || notification["params"]["diagnostics"]
                .as_array()
                .is_some_and(Vec::is_empty)
    }));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_routes_missing_import_to_import_declaration() {
    let root = temp_workspace("lsp_missing_import");
    let main_path = root.join("main.mimi");
    let main_text = "use missing_module\n\nfunc main() -> i32 { 0 }\n";
    fs::write(&main_path, main_text).expect("write LSP main");
    let root_uri = file_uri(&root);
    let main_uri = file_uri(&main_path);

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let notifications = server.compute_diagnostic_notifications(main_text, &main_uri);
    let main = notifications
        .iter()
        .find(|notification| notification["params"]["uri"].as_str() == Some(main_uri.as_str()))
        .expect("main publishDiagnostics notification");
    let diagnostic = main["params"]["diagnostics"]
        .as_array()
        .expect("main diagnostics")
        .iter()
        .find(|diagnostic| {
            diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("cannot find module 'missing_module'"))
        })
        .expect("structured missing-import diagnostic");
    assert_eq!(diagnostic["range"]["start"]["line"], 0);
    assert_eq!(diagnostic["range"]["start"]["character"], 0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_routes_dependency_parse_error_to_dependency_uri() {
    let root = temp_workspace("lsp_dependency_parse");
    let main_path = root.join("main.mimi");
    let dep_path = root.join("dep.mimi");
    let main_text = "use dep\n\nfunc main() -> i32 { 0 }\n";
    fs::write(&main_path, main_text).expect("write LSP main");
    fs::write(&dep_path, "pub func broken(value: i32 -> i32 { value }\n")
        .expect("write malformed dependency");
    let root_uri = file_uri(&root);
    let main_uri = file_uri(&main_path);
    let dep_uri = file_uri(&dep_path);

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let notifications = server.compute_diagnostic_notifications(main_text, &main_uri);
    let dependency = notifications
        .iter()
        .find(|notification| notification["params"]["uri"].as_str() == Some(dep_uri.as_str()))
        .expect("dependency publishDiagnostics notification");
    let diagnostics = dependency["params"]["diagnostics"]
        .as_array()
        .expect("dependency diagnostics");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["range"]["start"]["line"], 0);
    assert!(diagnostics[0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("expected")));
    assert!(notifications.iter().all(|notification| {
        notification["method"] != "textDocument/publishDiagnostics"
            || notification["params"]["uri"].as_str() != Some(main_uri.as_str())
            || notification["params"]["diagnostics"]
                .as_array()
                .is_some_and(Vec::is_empty)
    }));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_reports_unknown_source_loader_failures_as_global_messages() {
    let root = temp_workspace("lsp_global_loader_error");
    let outside = std::env::temp_dir().join(format!(
        "mimi_outside_workspace_{}_main.mimi",
        std::process::id()
    ));
    let text = "func main() -> i32 { 0 }\n";
    fs::write(&outside, text).expect("write outside source");
    let root_uri = file_uri(&root);
    let outside_uri = file_uri(&outside);

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let notifications = server.compute_diagnostic_notifications(text, &outside_uri);
    assert!(notifications.iter().any(|notification| {
        notification["method"] == "window/showMessage"
            && notification["params"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("outside the workspace"))
    }));
    let published = notifications
        .iter()
        .find(|notification| {
            notification["method"] == "textDocument/publishDiagnostics"
                && notification["params"]["uri"].as_str() == Some(outside_uri.as_str())
        })
        .expect("active document clear notification");
    assert!(published["params"]["diagnostics"]
        .as_array()
        .is_some_and(Vec::is_empty));

    let _ = fs::remove_file(outside);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_direct_notifications_match_pending_global_transport_order() {
    let root = temp_workspace("lsp_global_transport_order");
    let outside = std::env::temp_dir().join(format!(
        "mimi_global_transport_order_{}_main.mimi",
        std::process::id()
    ));
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n",
        "func main() -> i32 {\n",
        "    0\n",
        "}\n"
    );
    fs::write(&outside, text).expect("write outside source");
    let root_uri = file_uri(&root);
    let outside_uri = file_uri(&outside);

    let probe = crate::lsp::LspServer::new();
    let file = probe
        .parse_with_recovery_for_uri(text, Some(&outside_uri))
        .expect("parse outside source");
    let bad = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find bad function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let route = crate::diagnostic::mir_route_error_diagnostic(
        format!(
            "transport wrapper: {}: stale receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ),
        crate::span::Span::new(3, 5, 3, 12).with_source(crate::span::SourceId::new(1)),
    );

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    server.insert_verification_cache_with_diagnostic(
        crate::lsp::verification_cache_key(&outside_uri, "bad"),
        body_hash,
        crate::verifier::VerifStatus::Failed,
        route.message.clone(),
        route,
    );

    let direct = server.compute_diagnostic_notifications(text, &outside_uri);
    assert_eq!(
        direct[0]["method"], "textDocument/publishDiagnostics",
        "direct notifications must lead with the active document"
    );
    assert_eq!(direct[0]["params"]["uri"], outside_uri);
    assert!(direct[0]["params"]["diagnostics"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(direct[1]["method"], "window/showMessage");
    assert!(direct[1]["params"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("outside the workspace")));

    let response = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": outside_uri, "version": 1 },
            "contentChanges": [{ "text": text }]
        }
    }));
    let response = response.expect("didChange should publish primary diagnostics");
    assert_eq!(response["method"], "textDocument/publishDiagnostics");
    assert_eq!(
        response["params"]["diagnostics"][0]["code"],
        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
    );
    let pending = server.drain_pending_notifications();
    assert_eq!(pending, vec![direct[1].clone()]);

    let _ = fs::remove_file(outside);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_multi_uri_dependency_notifications_keep_batches_and_order() {
    let root = temp_workspace("lsp_multi_uri_transport_order");
    let main_path = root.join("main.mimi");
    let left_path = root.join("left.mimi");
    let right_path = root.join("right.mimi");
    let main_text = "use left\nuse right\nfunc main() -> i32 { 0 }\n";
    let left_text = "pub func left_value() -> i32 {\n    missing_value\n}\n";
    let right_text = "pub func right_value() -> i32 {\n    missing_value\n}\n";
    fs::write(&main_path, main_text).expect("write main source");
    fs::write(&left_path, left_text).expect("write left dependency");
    fs::write(&right_path, right_text).expect("write right dependency");
    let root_uri = file_uri(&root);
    let main_uri = file_uri(&main_path);
    let left_uri = file_uri(&left_path);
    let right_uri = file_uri(&right_path);

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));

    let direct = server.compute_diagnostic_notifications(main_text, &main_uri);
    let publish_uris = direct
        .iter()
        .filter(|notification| notification["method"] == "textDocument/publishDiagnostics")
        .filter_map(|notification| notification["params"]["uri"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        publish_uris,
        vec![main_uri.as_str(), left_uri.as_str(), right_uri.as_str()],
        "active document must lead, while dependency batches remain deterministically ordered"
    );
    for dependency_uri in [&left_uri, &right_uri] {
        let dependency = direct
            .iter()
            .find(|notification| notification["params"]["uri"] == *dependency_uri)
            .expect("dependency publishDiagnostics batch");
        let diagnostics = dependency["params"]["diagnostics"]
            .as_array()
            .expect("dependency diagnostics");
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic["message"] == "undefined variable 'missing_value'")
                .count(),
            1,
            "each dependency keeps its own identical checker diagnostic"
        );
    }

    let response = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": main_uri,
                "version": 1,
                "text": main_text
            }
        }
    }));
    let response = response.expect("didOpen should publish the active document");
    assert_eq!(response["method"], "textDocument/publishDiagnostics");
    assert_eq!(response["params"]["uri"], main_uri);
    let pending = server.drain_pending_notifications();
    assert_eq!(pending, direct[1..].to_vec());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_source_reset_cache_hit_keeps_route_primary_and_global_pending() {
    let root = temp_workspace("lsp_reset_cache_global_pending");
    let outside = std::env::temp_dir().join(format!(
        "mimi_reset_cache_global_pending_{}_main.mimi",
        std::process::id()
    ));
    let uri = file_uri(&outside);
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n",
        "func main() -> i32 {\n",
        "    0\n",
        "}\n"
    );

    let probe = crate::lsp::LspServer::new();
    let file = probe
        .parse_with_recovery_for_uri(text, Some(&uri))
        .expect("parse route source");
    let bad = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find bad function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let route = crate::diagnostic::mir_route_error_diagnostic(
        format!(
            "reset transport wrapper: {}: stale receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ),
        crate::span::Span::new(3, 5, 3, 12).with_source(crate::span::SourceId::new(1)),
    );

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": file_uri(&root) }
    }));
    for index in 0..crate::lsp::MAX_SOURCE_RECORDS {
        let seed = format!("func seed_{index}() -> i32 {{\n    {index}\n}}\n");
        server
            .parse_with_recovery_for_uri(&seed, None)
            .expect("fill source registry");
    }
    server.insert_verification_cache_with_diagnostic(
        crate::lsp::verification_cache_key(&uri, "bad"),
        body_hash,
        crate::verifier::VerifStatus::Failed,
        route.message.clone(),
        route,
    );

    let response = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 1 },
            "contentChanges": [{ "text": text }]
        }
    }));
    let response = response.expect("didChange should publish the active document");
    assert_eq!(response["method"], "textDocument/publishDiagnostics");
    assert_eq!(
        response["params"]["diagnostics"][0]["code"],
        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
        "source reset must not turn a route cache hit into a fresh unstructured verifier error"
    );
    let pending = server.drain_pending_notifications();
    assert_eq!(pending.len(), 1, "global loader failure should be pending");
    assert_eq!(pending[0]["method"], "window/showMessage");
    assert!(pending[0]["params"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("outside the workspace")));

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_for_same_disk_source_keeps_active_diagnostic_owner() {
    let root = temp_workspace("lsp_alias_uri_owner");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let text = "func broken(value: i32 -> i32 { value }\n";
    fs::write(&real_path, text).expect("write aliased source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create source alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_notifications = server.compute_diagnostic_notifications(text, &real_uri);
    assert!(
        real_notifications.iter().any(|notification| {
            notification["method"] == "textDocument/publishDiagnostics"
                && notification["params"]["uri"] == real_uri
                && !notification["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
        }),
        "the real URI should own its parse diagnostic before the alias is opened"
    );

    let alias_notifications = server.compute_diagnostic_notifications(text, &alias_uri);
    let alias_batch = alias_notifications
        .iter()
        .find(|notification| {
            notification["method"] == "textDocument/publishDiagnostics"
                && notification["params"]["uri"] == alias_uri
        })
        .expect("alias URI should receive a publishDiagnostics batch");
    assert!(
        !alias_batch["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "an aliased active document must retain its own parse diagnostic"
    );
    assert!(
        alias_notifications.iter().all(|notification| {
            notification["method"] != "textDocument/publishDiagnostics"
                || notification["params"]["uri"] != real_uri
        }),
        "diagnostics for the alias must not be redirected to the first URI"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_verification_cache_roundtrip_keeps_uri_keys_and_source_key() {
    let root = temp_workspace("lsp_alias_uri_cache_roundtrip");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n"
    );
    fs::write(&real_path, text).expect("write cache source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create cache source alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(text, Some(&real_uri))
        .expect("parse real cache source");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find cache function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("real source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Failed,
        "real URI cached failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            format!(
                "real cache wrapper: {}: real URI cached failure",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            ),
            crate::span::Span::new(3, 5, 3, 24).with_source(real_source),
        ),
    );

    let alias_file = writer
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("parse alias cache source");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("alias source id");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "alias cache snapshot must retain the stable SourceKey"
    );
    assert_eq!(real_source, alias_source, "alias must reuse the SourceId");
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Failed,
        "alias URI cached failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            format!(
                "alias cache wrapper: {}: alias URI cached failure",
                crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
            ),
            crate::span::Span::new(3, 5, 3, 25).with_source(alias_source),
        ),
    );
    writer.save_cache();

    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json")).expect("read alias cache"),
    )
    .expect("parse alias cache");
    for key in [&real_key, &alias_key] {
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"]["source_key"],
            source_key,
            "each URI cache entry must persist the same stable SourceKey"
        );
    }

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let alias_snapshot = reader
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("reader alias snapshot");
    let alias_diagnostics = reader.compute_verification_diagnostics(text, 0, &alias_uri);
    assert_eq!(
        alias_diagnostics.len(),
        1,
        "alias URI should replay its own persisted route diagnostic"
    );
    assert_eq!(
        alias_diagnostics[0]["message"],
        "alias cache wrapper: MIR-RECEIPT-001: alias URI cached failure"
    );
    assert_eq!(
        alias_snapshot
            .sources
            .record(
                alias_snapshot
                    .sources
                    .id_for_uri(&alias_uri)
                    .expect("alias id")
            )
            .and_then(|record| record.canonical_uri.as_deref()),
        Some(alias_uri.as_str()),
        "alias replay must keep the alias URI as the active snapshot target"
    );

    let real_snapshot = reader
        .parse_with_recovery_for_uri(text, Some(&real_uri))
        .expect("reader real snapshot");
    let real_diagnostics = reader.compute_verification_diagnostics(text, 0, &real_uri);
    assert_eq!(
        real_diagnostics.len(),
        1,
        "real URI should replay its independent persisted route diagnostic"
    );
    assert_eq!(
        real_diagnostics[0]["message"],
        "real cache wrapper: MIR-RECEIPT-001: real URI cached failure"
    );
    assert_eq!(
        real_snapshot
            .sources
            .record(
                real_snapshot
                    .sources
                    .id_for_uri(&real_uri)
                    .expect("real id")
            )
            .and_then(|record| record.canonical_uri.as_deref()),
        Some(real_uri.as_str()),
        "switching back must restore the real URI snapshot target"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_cache_hit_touches_active_key_before_eviction() {
    let root = temp_workspace("lsp_alias_uri_cache_lru");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n"
    );
    fs::write(&real_path, text).expect("write LRU source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create LRU source alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut server = crate::lsp::LspServer::new();
    let _ = server.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = server
        .parse_with_recovery_for_uri(text, Some(&real_uri))
        .expect("parse LRU real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find LRU function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("LRU real source id");
    let alias_file = server
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("parse LRU alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("LRU alias source id");
    assert_eq!(real_source, alias_source, "alias must share the SourceId");
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    server.cache_put_verification(
        real_key.clone(),
        crate::lsp::VerificationCacheEntry::new(
            body_hash,
            crate::verifier::VerifStatus::Disproven,
            "real LRU failure".to_string(),
            Some(crate::diagnostic::mir_route_error_diagnostic(
                "real LRU route: MIR-RECEIPT-001".to_string(),
                crate::span::Span::new(3, 5, 3, 24).with_source(real_source),
            )),
        ),
    );
    server.cache_put_verification(
        alias_key.clone(),
        crate::lsp::VerificationCacheEntry::new(
            body_hash,
            crate::verifier::VerifStatus::Disproven,
            "alias LRU failure".to_string(),
            Some(crate::diagnostic::mir_route_error_diagnostic(
                "alias LRU route: MIR-RECEIPT-001".to_string(),
                crate::span::Span::new(3, 5, 3, 24).with_source(alias_source),
            )),
        ),
    );
    for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE - 2) {
        server.cache_put_verification(
            format!("alias-lru-cold-{index}"),
            crate::lsp::VerificationCacheEntry::new(
                index as u64,
                crate::verifier::VerifStatus::Proven,
                "cold proof".to_string(),
                None,
            ),
        );
    }

    let diagnostics = server.compute_verification_diagnostics(text, 0, &alias_uri);
    assert_eq!(
        diagnostics
            .first()
            .and_then(|diagnostic| diagnostic["message"].as_str()),
        Some("alias LRU route: MIR-RECEIPT-001"),
        "the active alias key must replay its own cached diagnostic"
    );
    server.cache_put_verification(
        "alias-lru-new".to_string(),
        crate::lsp::VerificationCacheEntry::new(
            99,
            crate::verifier::VerifStatus::Proven,
            "new proof".to_string(),
            None,
        ),
    );
    assert!(
        server.verification_cache.contains_key(&alias_key),
        "the alias key touched by the active request must survive eviction"
    );
    assert!(
        !server.verification_cache.contains_key(&real_key),
        "the real URI key must remain an independent older LRU entry"
    );
    assert!(
        server.verification_cache.contains_key("alias-lru-cold-0"),
        "the oldest cold entry remains newer than the independent real key"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_cache_writeback_restarts_with_deterministic_lru_order() {
    let root = temp_workspace("lsp_alias_uri_cache_restart");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n"
    );
    fs::write(&real_path, text).expect("write restart source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create restart source alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(text, Some(&real_uri))
        .expect("parse restart real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find restart function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("restart real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("restart stable source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "restart real failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "restart real route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(3, 5, 3, 24).with_source(real_source),
        ),
    );

    let alias_file = writer
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("parse restart alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("restart alias source id");
    assert_eq!(real_source, alias_source, "aliases must share the SourceId");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "alias snapshot must retain the stable SourceKey"
    );
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "restart alias failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "restart alias route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(3, 5, 3, 25).with_source(alias_source),
        ),
    );
    writer.save_cache();

    // A cache hit has no verification side effect, so explicitly write the
    // touched alias state back to disk. The persistent schema intentionally
    // omits timestamps; a restart must reconstruct order from sorted keys.
    let mut hit_reader = crate::lsp::LspServer::new();
    let _ = hit_reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    hit_reader
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("parse hit-reader alias URI");
    let hit_diagnostics = hit_reader.compute_verification_diagnostics(text, 0, &alias_uri);
    assert_eq!(
        hit_diagnostics.first().and_then(|d| d["message"].as_str()),
        Some("restart alias route: MIR-RECEIPT-001"),
        "the writeback reader must replay the active alias diagnostic"
    );
    hit_reader.save_cache();

    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json"))
            .expect("read restart cache after writeback"),
    )
    .expect("parse restart cache after writeback");
    for key in [&real_key, &alias_key] {
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"]["source_key"],
            source_key,
            "writeback must preserve the shared stable SourceKey"
        );
    }

    // Two independent readers load the same file. Since persisted entries are
    // sorted before hydrating the LRU, the same alias hit and one fresh insert
    // must evict the real URI key in both readers.
    let mut readers = [crate::lsp::LspServer::new(), crate::lsp::LspServer::new()];
    for (reader_index, reader) in readers.iter_mut().enumerate() {
        let _ = reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": reader_index + 1,
            "method": "initialize",
            "params": { "rootUri": root_uri }
        }));
        let alias_snapshot = reader
            .parse_with_recovery_for_uri(text, Some(&alias_uri))
            .expect("parse restarted alias URI");
        let real_snapshot = reader
            .parse_with_recovery_for_uri(text, Some(&real_uri))
            .expect("parse restarted real URI");
        assert_eq!(
            alias_snapshot
                .sources
                .key(
                    alias_snapshot
                        .sources
                        .id_for_uri(&alias_uri)
                        .expect("alias id")
                )
                .map(|key| key.as_str()),
            Some(source_key.as_str()),
            "reader alias snapshot must resolve the persisted SourceKey"
        );
        assert_eq!(
            real_snapshot
                .sources
                .key(
                    real_snapshot
                        .sources
                        .id_for_uri(&real_uri)
                        .expect("real id")
                )
                .map(|key| key.as_str()),
            Some(source_key.as_str()),
            "reader real snapshot must resolve the persisted SourceKey"
        );
        assert!(reader.verification_cache.contains_key(&real_key));
        assert!(reader.verification_cache.contains_key(&alias_key));

        for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE - 2) {
            reader.cache_put_verification(
                format!("restart-cold-{index}"),
                crate::lsp::VerificationCacheEntry::new(
                    index as u64,
                    crate::verifier::VerifStatus::Proven,
                    "restart cold proof".to_string(),
                    None,
                ),
            );
        }
        let diagnostics = reader.compute_verification_diagnostics(text, 0, &alias_uri);
        assert_eq!(
            diagnostics.first().and_then(|d| d["message"].as_str()),
            Some("restart alias route: MIR-RECEIPT-001"),
            "reader must replay the alias entry after the cache is full"
        );
        reader.cache_put_verification(
            format!("restart-fresh-{reader_index}"),
            crate::lsp::VerificationCacheEntry::new(
                99,
                crate::verifier::VerifStatus::Proven,
                "restart fresh proof".to_string(),
                None,
            ),
        );
        assert!(
            reader.verification_cache.contains_key(&alias_key),
            "the touched alias entry must survive restart eviction"
        );
        assert!(
            !reader.verification_cache.contains_key(&real_key),
            "the independent real entry must be the deterministic oldest victim"
        );
        assert!(
            reader.verification_cache.contains_key("restart-cold-0"),
            "the oldest session entry must remain newer than the real entry"
        );
    }

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_transport_replay_keeps_primary_and_pending_order() {
    let root = temp_workspace("lsp_alias_uri_transport");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let dep_path = root.join("dep.mimi");
    let main_text = concat!(
        "use dep\n",
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n",
        "func main() -> i32 {\n",
        "    0\n",
        "}\n"
    );
    let dep_text = "pub func broken() -> i32 {\n    missing_dep\n}\n";
    fs::write(&real_path, main_text).expect("write transport source");
    fs::write(&dep_path, dep_text).expect("write transport dependency");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create transport source alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);
    let dep_uri = file_uri(&dep_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(main_text, Some(&real_uri))
        .expect("parse transport real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find transport function");
    let body_hash = crate::lsp::util::hash_func_body(main_text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("transport real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("transport stable source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "transport real failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "transport real route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 24).with_source(real_source),
        ),
    );
    let alias_file = writer
        .parse_with_recovery_for_uri(main_text, Some(&alias_uri))
        .expect("parse transport alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("transport alias source id");
    assert_eq!(real_source, alias_source, "aliases must share the SourceId");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "alias transport snapshot must retain the stable SourceKey"
    );
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "transport alias failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "transport alias route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 25).with_source(alias_source),
        ),
    );
    writer.save_cache();

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));
    // Keep the two persisted URI keys at the front of a full LRU. The alias
    // didChange hit below must touch only alias_key before the fresh insert.
    for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE - 2) {
        reader.cache_put_verification(
            format!("transport-cold-{index}"),
            crate::lsp::VerificationCacheEntry::new(
                index as u64,
                crate::verifier::VerifStatus::Proven,
                "transport cold proof".to_string(),
                None,
            ),
        );
    }

    let direct = reader.compute_diagnostic_notifications(main_text, &alias_uri);
    let direct_uris = direct
        .iter()
        .filter(|notification| notification["method"] == "textDocument/publishDiagnostics")
        .filter_map(|notification| notification["params"]["uri"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        direct_uris,
        vec![alias_uri.as_str(), dep_uri.as_str()],
        "direct alias diagnostics must lead and dependency batches must remain pending order"
    );

    let opened = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": alias_uri,
                "version": 1,
                "text": main_text
            }
        }
    }));
    let opened = opened.expect("alias didOpen should publish the primary document");
    assert_eq!(opened["method"], "textDocument/publishDiagnostics");
    assert_eq!(opened["params"]["uri"], alias_uri);
    assert!(
        opened["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "didOpen emits checker diagnostics only; verification is added on didChange"
    );
    let open_pending = reader.drain_pending_notifications();
    assert_eq!(open_pending, direct[1..].to_vec());

    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": alias_uri },
            "position": { "line": 1, "character": 0 }
        }
    }));
    let changed = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": alias_uri, "version": 2 },
            "contentChanges": [{ "text": main_text }]
        }
    }));
    let changed = changed.expect("alias didChange should publish the primary document");
    assert_eq!(changed["method"], "textDocument/publishDiagnostics");
    assert_eq!(changed["params"]["uri"], alias_uri);
    assert_eq!(
        changed["params"]["diagnostics"][0]["message"], "transport alias route: MIR-RECEIPT-001",
        "didChange must replay the alias route diagnostic in the primary batch"
    );
    let change_pending = reader.drain_pending_notifications();
    assert_eq!(
        change_pending, open_pending,
        "didOpen and didChange must preserve the same dependency pending batch"
    );

    // Persist immediately after the transport cache hit, before forcing LRU
    // eviction, so the writeback still contains both URI keys and their shared
    // source identity.
    reader.save_cache();
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json"))
            .expect("read transport cache after didChange hit"),
    )
    .expect("parse transport cache after didChange hit");
    for key in [&real_key, &alias_key] {
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"]["source_key"],
            source_key,
            "transport writeback must preserve the shared SourceKey"
        );
    }

    reader.cache_put_verification(
        "transport-fresh".to_string(),
        crate::lsp::VerificationCacheEntry::new(
            99,
            crate::verifier::VerifStatus::Proven,
            "transport fresh proof".to_string(),
            None,
        ),
    );
    assert!(
        reader.verification_cache.contains_key(&alias_key),
        "the active alias entry must survive transport eviction"
    );
    assert!(
        !reader.verification_cache.contains_key(&real_key),
        "the independent real URI entry must be the oldest transport victim"
    );
    assert!(
        reader.verification_cache.contains_key("transport-cold-0"),
        "the oldest session entry must remain newer than the independent real key"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_switch_rejects_stale_changes_without_cache_drift() {
    let root = temp_workspace("lsp_alias_uri_versions");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n"
    );
    let stale_text = text.replace("    0", "    missing_after_stale_change");
    fs::write(&real_path, text).expect("write version source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create version source alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(text, Some(&real_uri))
        .expect("parse version real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find version function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("version real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("version stable source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "version real failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "version real route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 24).with_source(real_source),
        ),
    );
    let alias_file = writer
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("parse version alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("version alias source id");
    assert_eq!(real_source, alias_source, "aliases must share the SourceId");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "alias version snapshot must retain the stable SourceKey"
    );
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "version alias failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "version alias route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 25).with_source(alias_source),
        ),
    );
    writer.save_cache();

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));
    for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE - 2) {
        reader.cache_put_verification(
            format!("version-cold-{index}"),
            crate::lsp::VerificationCacheEntry::new(
                index as u64,
                crate::verifier::VerifStatus::Proven,
                "version cold proof".to_string(),
                None,
            ),
        );
    }

    let opened_alias = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": { "uri": alias_uri, "version": 2, "text": text }
        }
    }));
    assert_eq!(
        opened_alias.expect("alias didOpen response")["params"]["uri"],
        alias_uri
    );
    assert_eq!(reader.document_version(&alias_uri), Some(2));
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": alias_uri },
            "position": { "line": 1, "character": 0 }
        }
    }));
    let changed_alias = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": alias_uri, "version": 3 },
            "contentChanges": [{ "text": text }]
        }
    }));
    assert_eq!(
        changed_alias.expect("alias didChange response")["params"]["diagnostics"][0]["message"],
        "version alias route: MIR-RECEIPT-001",
        "alias version hit must replay the alias route"
    );
    assert_eq!(reader.document_version(&alias_uri), Some(3));
    let alias_lru_after_hit = reader.verification_cache.contains_key(&alias_key);
    assert!(alias_lru_after_hit);

    // This lower version carries deliberately invalid text. It must be
    // rejected before applying contentChanges, so neither the open document
    // nor its version/cache state can drift from the accepted v3 snapshot.
    let stale = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": alias_uri, "version": 2 },
            "contentChanges": [{ "text": stale_text }]
        }
    }));
    assert!(stale.is_none(), "stale alias didChange must be ignored");
    assert_eq!(reader.document_version(&alias_uri), Some(3));
    assert_eq!(
        reader.documents.get(&alias_uri).map(String::as_str),
        Some(text)
    );
    assert!(
        reader.verification_cache.contains_key(&alias_key),
        "stale alias didChange must not evict the active cache key"
    );

    // Switch URI spelling while keeping the same disk source. Version state is
    // per URI, but SourceKey and route provenance remain shared.
    let opened_real = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": { "uri": real_uri, "version": 1, "text": text }
        }
    }));
    assert_eq!(
        opened_real.expect("real didOpen response")["params"]["uri"],
        real_uri
    );
    assert_eq!(reader.document_version(&real_uri), Some(1));
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": real_uri },
            "position": { "line": 1, "character": 0 }
        }
    }));
    let changed_real = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": real_uri, "version": 2 },
            "contentChanges": [{ "text": text }]
        }
    }));
    assert_eq!(
        changed_real.expect("real didChange response")["params"]["diagnostics"][0]["message"],
        "version real route: MIR-RECEIPT-001",
        "real version hit must replay the independent real route"
    );
    assert_eq!(reader.document_version(&real_uri), Some(2));
    let stale_real = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": real_uri, "version": 1 },
            "contentChanges": [{ "text": stale_text }]
        }
    }));
    assert!(stale_real.is_none(), "stale real didChange must be ignored");
    assert_eq!(reader.document_version(&real_uri), Some(2));
    assert_eq!(
        reader.documents.get(&real_uri).map(String::as_str),
        Some(text)
    );

    reader.save_cache();
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json"))
            .expect("read version cache after URI switch"),
    )
    .expect("parse version cache after URI switch");
    for key in [&real_key, &alias_key] {
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"]["source_key"],
            source_key,
            "URI switch writeback must preserve the shared SourceKey"
        );
    }
    reader.cache_put_verification(
        "version-fresh".to_string(),
        crate::lsp::VerificationCacheEntry::new(
            99,
            crate::verifier::VerifStatus::Proven,
            "version fresh proof".to_string(),
            None,
        ),
    );
    assert!(reader.verification_cache.contains_key(&alias_key));
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(
        !reader.verification_cache.contains_key("version-cold-0"),
        "both accepted URI hits should move behind the cold entries"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_close_reopen_preserves_cache_and_source_identity() {
    let root = temp_workspace("lsp_alias_uri_close_reopen");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n"
    );
    fs::write(&real_path, text).expect("write close/reopen source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create close/reopen alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(text, Some(&real_uri))
        .expect("parse close/reopen real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find close/reopen function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("close/reopen real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("close/reopen stable source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "close/reopen real failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "close/reopen real route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 24).with_source(real_source),
        ),
    );
    let alias_file = writer
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("parse close/reopen alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("close/reopen alias source id");
    assert_eq!(real_source, alias_source, "aliases must share the SourceId");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "alias close/reopen snapshot must retain the stable SourceKey"
    );
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "close/reopen alias failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "close/reopen alias route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 25).with_source(alias_source),
        ),
    );
    writer.save_cache();

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));
    for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE - 2) {
        reader.cache_put_verification(
            format!("close-reopen-cold-{index}"),
            crate::lsp::VerificationCacheEntry::new(
                index as u64,
                crate::verifier::VerifStatus::Proven,
                "close/reopen cold proof".to_string(),
                None,
            ),
        );
    }

    let open = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "version": version, "text": text }
            }
        }))
    };
    let hover = |reader: &mut crate::lsp::LspServer, uri: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 0 }
            }
        }))
    };
    let change = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }))
    };
    let close = |reader: &mut crate::lsp::LspServer, uri: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } }
        }))
    };

    assert_eq!(
        open(&mut reader, &alias_uri, 1).expect("alias first didOpen")["params"]["uri"],
        alias_uri
    );
    let _ = hover(&mut reader, &alias_uri);
    assert_eq!(
        change(&mut reader, &alias_uri, 2).expect("alias first didChange")["params"]["diagnostics"]
            [0]["message"],
        "close/reopen alias route: MIR-RECEIPT-001"
    );
    let closed_alias = close(&mut reader, &alias_uri).expect("alias didClose");
    assert_eq!(closed_alias["params"]["uri"], alias_uri);
    assert!(
        closed_alias["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "didClose must clear only the alias document diagnostics"
    );
    assert!(!reader.documents.contains_key(&alias_uri));
    assert_eq!(reader.document_version(&alias_uri), None);
    assert!(
        reader.verification_cache.contains_key(&alias_key),
        "closing an alias document must not delete its verification cache"
    );
    assert!(reader.verification_cache.contains_key(&real_key));

    assert_eq!(
        open(&mut reader, &real_uri, 1).expect("real didOpen")["params"]["uri"],
        real_uri
    );
    let _ = hover(&mut reader, &real_uri);
    assert_eq!(
        change(&mut reader, &real_uri, 2).expect("real didChange")["params"]["diagnostics"][0]
            ["message"],
        "close/reopen real route: MIR-RECEIPT-001"
    );
    let closed_real = close(&mut reader, &real_uri).expect("real didClose");
    assert_eq!(closed_real["params"]["uri"], real_uri);
    assert!(!reader.documents.contains_key(&real_uri));
    assert_eq!(reader.document_version(&real_uri), None);
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));

    // Reopen through the alias after the real URI was closed. The same source
    // identity must resolve a fresh active snapshot and replay alias route.
    assert_eq!(
        open(&mut reader, &alias_uri, 3).expect("alias reopen didOpen")["params"]["uri"],
        alias_uri
    );
    let _ = hover(&mut reader, &alias_uri);
    assert_eq!(
        change(&mut reader, &alias_uri, 4).expect("alias reopen didChange")["params"]
            ["diagnostics"][0]["message"],
        "close/reopen alias route: MIR-RECEIPT-001"
    );
    assert_eq!(reader.document_version(&alias_uri), Some(4));
    assert_eq!(
        reader.documents.get(&alias_uri).map(String::as_str),
        Some(text)
    );

    reader.save_cache();
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json")).expect("read close/reopen cache"),
    )
    .expect("parse close/reopen cache");
    for key in [&real_key, &alias_key] {
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"]["source_key"],
            source_key,
            "close/reopen writeback must preserve the shared SourceKey"
        );
    }
    reader.cache_put_verification(
        "close-reopen-fresh".to_string(),
        crate::lsp::VerificationCacheEntry::new(
            99,
            crate::verifier::VerifStatus::Proven,
            "close/reopen fresh proof".to_string(),
            None,
        ),
    );
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));
    assert!(
        !reader
            .verification_cache
            .contains_key("close-reopen-cold-0"),
        "valid hits across close/reopen must advance both URI keys"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_save_close_reopen_keeps_versions_and_lru_identity() {
    let root = temp_workspace("lsp_alias_uri_save_close");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n"
    );
    fs::write(&real_path, text).expect("write save source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create save source alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(text, Some(&real_uri))
        .expect("parse save real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find save function");
    let body_hash = crate::lsp::util::hash_func_body(text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("save real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("save stable source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "save real failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "save real route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 24).with_source(real_source),
        ),
    );
    let alias_file = writer
        .parse_with_recovery_for_uri(text, Some(&alias_uri))
        .expect("parse save alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("save alias source id");
    assert_eq!(real_source, alias_source, "aliases must share the SourceId");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "alias save snapshot must retain the stable SourceKey"
    );
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Disproven,
        "save alias failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "save alias route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 25).with_source(alias_source),
        ),
    );
    writer.save_cache();

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));

    let open = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "version": version, "text": text }
            }
        }))
    };
    let hover = |reader: &mut crate::lsp::LspServer, uri: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 0 }
            }
        }))
    };
    let change = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }))
    };
    let save = |reader: &mut crate::lsp::LspServer, uri: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": {
                "textDocument": { "uri": uri },
                "text": text
            }
        }))
    };
    let close = |reader: &mut crate::lsp::LspServer, uri: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": uri } }
        }))
    };

    assert_eq!(
        open(&mut reader, &alias_uri, 1).expect("alias save didOpen")["params"]["uri"],
        alias_uri
    );
    let _ = hover(&mut reader, &alias_uri);
    assert_eq!(
        change(&mut reader, &alias_uri, 2).expect("alias save didChange")["params"]["diagnostics"]
            [0]["message"],
        "save alias route: MIR-RECEIPT-001"
    );
    assert_eq!(reader.document_version(&alias_uri), Some(2));
    let saved_alias = save(&mut reader, &alias_uri).expect("alias didSave");
    assert_eq!(saved_alias["params"]["uri"], alias_uri);
    assert!(saved_alias["params"]["diagnostics"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(
        reader.document_version(&alias_uri),
        Some(2),
        "didSave must preserve the last accepted alias version"
    );
    assert_eq!(
        reader.documents.get(&alias_uri).map(String::as_str),
        Some(text)
    );
    let _ = close(&mut reader, &alias_uri).expect("alias didClose");
    assert_eq!(reader.document_version(&alias_uri), None);
    assert!(!reader.documents.contains_key(&alias_uri));
    assert!(reader.verification_cache.contains_key(&alias_key));
    assert!(reader.verification_cache.contains_key(&real_key));

    assert_eq!(
        open(&mut reader, &real_uri, 1).expect("real save didOpen")["params"]["uri"],
        real_uri
    );
    let _ = hover(&mut reader, &real_uri);
    assert_eq!(
        change(&mut reader, &real_uri, 2).expect("real save didChange")["params"]["diagnostics"][0]
            ["message"],
        "save real route: MIR-RECEIPT-001"
    );
    let saved_real = save(&mut reader, &real_uri).expect("real didSave");
    assert_eq!(saved_real["params"]["uri"], real_uri);
    assert_eq!(reader.document_version(&real_uri), Some(2));
    assert_eq!(
        reader.documents.get(&real_uri).map(String::as_str),
        Some(text)
    );
    let _ = close(&mut reader, &real_uri).expect("real didClose");
    assert_eq!(reader.document_version(&real_uri), None);
    assert!(!reader.documents.contains_key(&real_uri));
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));

    assert_eq!(
        open(&mut reader, &alias_uri, 3).expect("alias reopen save didOpen")["params"]["uri"],
        alias_uri
    );
    let _ = hover(&mut reader, &alias_uri);
    assert_eq!(
        change(&mut reader, &alias_uri, 4).expect("alias reopen save didChange")["params"]
            ["diagnostics"][0]["message"],
        "save alias route: MIR-RECEIPT-001"
    );
    let saved_alias_again = save(&mut reader, &alias_uri).expect("alias reopen didSave");
    assert_eq!(saved_alias_again["params"]["uri"], alias_uri);
    assert_eq!(reader.document_version(&alias_uri), Some(4));
    assert_eq!(
        reader.documents.get(&alias_uri).map(String::as_str),
        Some(text)
    );

    reader.save_cache();
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json")).expect("read save/close cache"),
    )
    .expect("parse save/close cache");
    for key in [&real_key, &alias_key] {
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"]["source_key"],
            source_key,
            "save/close writeback must preserve the shared SourceKey"
        );
    }

    // didSave/didClose and reopening are document-lifecycle operations. They
    // must not create verification-cache touches of their own: the final
    // alias didChange hit is the newest entry before capacity pressure.
    for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE - 2) {
        reader.cache_put_verification(
            format!("save-close-cold-{index}"),
            crate::lsp::VerificationCacheEntry::new(
                index as u64,
                crate::verifier::VerifStatus::Proven,
                "save/close cold proof".to_string(),
                None,
            ),
        );
    }
    reader.cache_put_verification(
        "save-close-fresh".to_string(),
        crate::lsp::VerificationCacheEntry::new(
            99,
            crate::verifier::VerifStatus::Proven,
            "save/close fresh proof".to_string(),
            None,
        ),
    );
    assert!(reader.verification_cache.contains_key(&alias_key));
    assert!(
        !reader.verification_cache.contains_key(&real_key),
        "the real URI key must remain the older independent entry"
    );
    assert!(reader.verification_cache.contains_key("save-close-cold-0"));

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_save_body_change_invalidates_stale_verdicts() {
    let root = temp_workspace("lsp_alias_uri_body_hash");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let old_text = concat!(
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n"
    );
    let new_text = old_text.replace("    0", "    1");
    fs::write(&real_path, old_text).expect("write body-hash source");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create body-hash alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(old_text, Some(&real_uri))
        .expect("parse body-hash real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find body-hash function");
    let old_hash = crate::lsp::util::hash_func_body(old_text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("body-hash real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("body-hash stable source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        old_hash,
        crate::verifier::VerifStatus::Disproven,
        "body-hash real stale failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "body-hash real stale route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 24).with_source(real_source),
        ),
    );
    let alias_file = writer
        .parse_with_recovery_for_uri(old_text, Some(&alias_uri))
        .expect("parse body-hash alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("body-hash alias source id");
    assert_eq!(real_source, alias_source, "aliases must share the SourceId");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "alias body-hash snapshot must retain the stable SourceKey"
    );
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        old_hash,
        crate::verifier::VerifStatus::Disproven,
        "body-hash alias stale failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "body-hash alias stale route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(4, 5, 4, 25).with_source(alias_source),
        ),
    );
    writer.save_cache();

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let open = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64, text: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "version": version, "text": text }
            }
        }))
    };
    let hover = |reader: &mut crate::lsp::LspServer, uri: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 0 }
            }
        }))
    };
    let change = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64, text: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }))
    };
    let save = |reader: &mut crate::lsp::LspServer, uri: &str, text: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": { "textDocument": { "uri": uri }, "text": text }
        }))
    };

    assert_eq!(
        open(&mut reader, &alias_uri, 1, old_text).expect("body-hash alias open")["params"]["uri"],
        alias_uri
    );
    let _ = hover(&mut reader, &alias_uri);
    assert_eq!(
        change(&mut reader, &alias_uri, 2, old_text).expect("body-hash alias old change")["params"]
            ["diagnostics"][0]["message"],
        "body-hash alias stale route: MIR-RECEIPT-001"
    );
    assert_eq!(reader.document_version(&alias_uri), Some(2));
    let saved_alias = save(&mut reader, &alias_uri, &new_text).expect("body-hash alias save");
    assert_eq!(saved_alias["params"]["uri"], alias_uri);
    assert_eq!(reader.document_version(&alias_uri), Some(2));
    assert_eq!(
        reader.documents.get(&alias_uri).map(String::as_str),
        Some(new_text.as_str())
    );
    let _ = hover(&mut reader, &alias_uri);
    let changed_alias =
        change(&mut reader, &alias_uri, 3, &new_text).expect("body-hash alias new change");
    assert!(
        changed_alias["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "body hash mismatch must invalidate the old disproven alias verdict"
    );
    assert_eq!(reader.document_version(&alias_uri), Some(3));
    assert_eq!(
        reader
            .verification_cache
            .get(&alias_key)
            .map(|entry| entry.body_hash),
        Some({
            let file = reader
                .parse_with_recovery_for_uri(&new_text, Some(&alias_uri))
                .expect("parse new alias body");
            let func = file
                .items
                .iter()
                .find_map(|item| match item {
                    crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
                    _ => None,
                })
                .expect("find new alias body");
            crate::lsp::util::hash_func_body(&new_text, func)
        })
    );
    assert!(matches!(
        reader
            .verification_cache
            .get(&alias_key)
            .map(|entry| &entry.status),
        Some(crate::verifier::VerifStatus::Proven)
    ));

    assert_eq!(
        open(&mut reader, &real_uri, 1, old_text).expect("body-hash real open")["params"]["uri"],
        real_uri
    );
    let _ = hover(&mut reader, &real_uri);
    assert_eq!(
        change(&mut reader, &real_uri, 2, old_text).expect("body-hash real old change")["params"]
            ["diagnostics"][0]["message"],
        "body-hash real stale route: MIR-RECEIPT-001"
    );
    let _ = save(&mut reader, &real_uri, &new_text).expect("body-hash real save");
    assert_eq!(reader.document_version(&real_uri), Some(2));
    let _ = hover(&mut reader, &real_uri);
    let changed_real =
        change(&mut reader, &real_uri, 3, &new_text).expect("body-hash real new change");
    assert!(
        changed_real["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "body hash mismatch must invalidate the old disproven real verdict"
    );
    assert!(matches!(
        reader
            .verification_cache
            .get(&real_key)
            .map(|entry| &entry.status),
        Some(crate::verifier::VerifStatus::Proven)
    ));

    reader.save_cache();
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json")).expect("read body-hash cache"),
    )
    .expect("parse body-hash cache");
    for key in [&real_key, &alias_key] {
        assert_eq!(
            persisted["entries"][key.as_str()]["status"],
            "Verified",
            "body-hash recheck must persist the new proven verdict"
        );
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"],
            serde_json::Value::Null,
            "a proven recheck must not persist the old disproven diagnostic"
        );
    }
    let alias_source_after = reader
        .parse_with_recovery_for_uri(&new_text, Some(&alias_uri))
        .expect("parse final alias snapshot")
        .sources;
    assert_eq!(
        alias_source_after
            .key(
                alias_source_after
                    .id_for_uri(&alias_uri)
                    .expect("final alias id")
            )
            .map(|key| key.as_str()),
        Some(source_key.as_str()),
        "body-hash recheck must keep the shared SourceKey"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn lsp_alias_uri_save_body_change_after_source_reset_keeps_pending_order() {
    let root = temp_workspace("lsp_alias_uri_body_hash_reset_pending");
    let real_path = root.join("real.mimi");
    let alias_path = root.join("alias.mimi");
    let dep_path = root.join("dep.mimi");
    let old_text = concat!(
        "use dep\n",
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n",
        "func main() -> i32 {\n",
        "    0\n",
        "}\n"
    );
    let new_text = old_text.replace("    0\n}\nfunc main", "    1\n}\nfunc main");
    let dep_text = "pub func broken() -> i32 {\n    missing_dep\n}\n";
    fs::write(&real_path, old_text).expect("write reset body-hash source");
    fs::write(&dep_path, dep_text).expect("write reset body-hash dependency");
    std::os::unix::fs::symlink(&real_path, &alias_path).expect("create reset body-hash alias");
    let root_uri = file_uri(&root);
    let real_uri = file_uri(&real_path);
    let alias_uri = file_uri(&alias_path);
    let dep_uri = file_uri(&dep_path);

    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let real_file = writer
        .parse_with_recovery_for_uri(old_text, Some(&real_uri))
        .expect("parse reset body-hash real URI");
    let bad = real_file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find reset body-hash function");
    let old_hash = crate::lsp::util::hash_func_body(old_text, bad);
    let real_source = real_file
        .sources
        .id_for_uri(&real_uri)
        .expect("reset body-hash real source id");
    let source_key = real_file
        .sources
        .key(real_source)
        .expect("reset body-hash stable source key")
        .as_str()
        .to_string();
    let real_key = crate::lsp::verification_cache_key(&real_uri, "bad");
    let alias_key = crate::lsp::verification_cache_key(&alias_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        real_key.clone(),
        old_hash,
        crate::verifier::VerifStatus::Disproven,
        "reset real stale failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "reset real stale route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(5, 5, 5, 12).with_source(real_source),
        ),
    );
    let alias_file = writer
        .parse_with_recovery_for_uri(old_text, Some(&alias_uri))
        .expect("parse reset body-hash alias URI");
    let alias_source = alias_file
        .sources
        .id_for_uri(&alias_uri)
        .expect("reset body-hash alias source id");
    assert_eq!(real_source, alias_source, "aliases must share the SourceId");
    assert_eq!(
        alias_file.sources.key(alias_source).map(|key| key.as_str()),
        Some(source_key.as_str()),
        "reset alias snapshot must retain the stable SourceKey"
    );
    writer.insert_verification_cache_with_diagnostic(
        alias_key.clone(),
        old_hash,
        crate::verifier::VerifStatus::Disproven,
        "reset alias stale failure".to_string(),
        crate::diagnostic::mir_route_error_diagnostic(
            "reset alias stale route: MIR-RECEIPT-001".to_string(),
            crate::span::Span::new(5, 5, 5, 13).with_source(alias_source),
        ),
    );
    writer.save_cache();

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    assert!(reader.verification_cache.contains_key(&real_key));
    assert!(reader.verification_cache.contains_key(&alias_key));

    // Leave the session registry exactly at its cap. The next diagnostic
    // request must cross the reset boundary while preserving its private
    // SourceKey snapshot for both the checker and verifier paths.
    for index in 0..crate::lsp::MAX_SOURCE_RECORDS {
        let seed = format!("func reset_pending_seed_{index}() -> i32 {{\n    {index}\n}}\n");
        reader
            .parse_with_recovery_for_uri(&seed, None)
            .expect("fill reset pending source registry");
    }
    assert_eq!(
        reader.source_registry.borrow().records().len(),
        crate::lsp::MAX_SOURCE_RECORDS,
        "test must fill source registry before the alias transport reset"
    );

    let open = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64, text: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "version": version, "text": text }
            }
        }))
    };
    let hover = |reader: &mut crate::lsp::LspServer, uri: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 0 }
            }
        }))
    };
    let change = |reader: &mut crate::lsp::LspServer, uri: &str, version: i64, text: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }
        }))
    };
    let save = |reader: &mut crate::lsp::LspServer, uri: &str, text: &str| {
        reader.handle_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": { "textDocument": { "uri": uri }, "text": text }
        }))
    };

    let opened_alias = open(&mut reader, &alias_uri, 1, old_text).expect("reset alias open");
    assert_eq!(opened_alias["params"]["uri"], alias_uri);
    assert!(
        opened_alias["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "didOpen should leave verification diagnostics for didChange"
    );
    let open_pending = reader.drain_pending_notifications();
    assert_eq!(
        open_pending.len(),
        1,
        "alias didOpen should queue one dependency batch"
    );
    assert_eq!(open_pending[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(open_pending[0]["params"]["uri"], dep_uri);

    let _ = hover(&mut reader, &alias_uri);
    let changed_old = change(&mut reader, &alias_uri, 2, old_text).expect("reset alias old change");
    assert_eq!(
        changed_old["params"]["diagnostics"][0]["message"],
        "reset alias stale route: MIR-RECEIPT-001"
    );
    let old_change_pending = reader.drain_pending_notifications();
    assert_eq!(old_change_pending, open_pending);

    // Refill the registry before didSave so that the provided new body travels
    // through another reset, then didChange must still produce the same single
    // dependency batch after the body-hash recheck.
    let current_records = reader.source_registry.borrow().records().len();
    for index in current_records..crate::lsp::MAX_SOURCE_RECORDS {
        let seed = format!("func save_reset_pending_seed_{index}() -> i32 {{\n    {index}\n}}\n");
        reader
            .parse_with_recovery_for_uri(&seed, None)
            .expect("refill reset pending source registry");
    }
    assert_eq!(
        reader.source_registry.borrow().records().len(),
        crate::lsp::MAX_SOURCE_RECORDS,
        "test must refill source registry before didSave reset"
    );
    let saved_alias = save(&mut reader, &alias_uri, &new_text).expect("reset alias save");
    assert_eq!(saved_alias["params"]["uri"], alias_uri);
    assert_eq!(reader.document_version(&alias_uri), Some(2));
    assert_eq!(
        reader.documents.get(&alias_uri).map(String::as_str),
        Some(new_text.as_str())
    );
    let save_pending = reader.drain_pending_notifications();
    assert_eq!(
        save_pending, open_pending,
        "didSave reset must preserve pending order"
    );

    let _ = hover(&mut reader, &alias_uri);
    let changed_new =
        change(&mut reader, &alias_uri, 3, &new_text).expect("reset alias new change");
    assert_eq!(changed_new["params"]["uri"], alias_uri);
    assert!(
        changed_new["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "new alias body must not replay its old route diagnostic after reset"
    );
    assert_eq!(reader.drain_pending_notifications(), open_pending);
    assert!(matches!(
        reader
            .verification_cache
            .get(&alias_key)
            .map(|entry| &entry.status),
        Some(crate::verifier::VerifStatus::Proven)
    ));
    assert!(
        matches!(
            reader
                .verification_cache
                .get(&real_key)
                .map(|entry| &entry.status),
            Some(crate::verifier::VerifStatus::Disproven)
        ),
        "the untouched real URI key must remain independent"
    );

    // Process the real spelling through the same reset/recheck path. This
    // proves that the two URI keys clean their own diagnostics independently
    // while retaining one SourceKey and one pending dependency order.
    let opened_real = open(&mut reader, &real_uri, 1, old_text).expect("reset real open");
    assert_eq!(opened_real["params"]["uri"], real_uri);
    assert!(opened_real["params"]["diagnostics"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(reader.drain_pending_notifications(), open_pending);
    let _ = hover(&mut reader, &real_uri);
    let changed_real_old =
        change(&mut reader, &real_uri, 2, old_text).expect("reset real old change");
    assert_eq!(
        changed_real_old["params"]["diagnostics"][0]["message"],
        "reset real stale route: MIR-RECEIPT-001"
    );
    assert_eq!(reader.drain_pending_notifications(), open_pending);

    let current_records = reader.source_registry.borrow().records().len();
    for index in current_records..crate::lsp::MAX_SOURCE_RECORDS {
        let seed =
            format!("func real_save_reset_pending_seed_{index}() -> i32 {{\n    {index}\n}}\n");
        reader
            .parse_with_recovery_for_uri(&seed, None)
            .expect("refill real reset pending source registry");
    }
    let _ = save(&mut reader, &real_uri, &new_text).expect("reset real save");
    assert_eq!(reader.document_version(&real_uri), Some(2));
    assert_eq!(
        reader.documents.get(&real_uri).map(String::as_str),
        Some(new_text.as_str())
    );
    assert_eq!(reader.drain_pending_notifications(), open_pending);
    let _ = hover(&mut reader, &real_uri);
    let changed_real_new =
        change(&mut reader, &real_uri, 3, &new_text).expect("reset real new change");
    assert_eq!(changed_real_new["params"]["uri"], real_uri);
    assert!(changed_real_new["params"]["diagnostics"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert_eq!(reader.drain_pending_notifications(), open_pending);
    for key in [&real_key, &alias_key] {
        assert!(matches!(
            reader
                .verification_cache
                .get(key)
                .map(|entry| &entry.status),
            Some(crate::verifier::VerifStatus::Proven)
        ));
    }
    reader.save_cache();
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json"))
            .expect("read reset body-hash cache"),
    )
    .expect("parse reset body-hash cache");
    for key in [&real_key, &alias_key] {
        assert_eq!(persisted["entries"][key.as_str()]["status"], "Verified");
        assert_eq!(
            persisted["entries"][key.as_str()]["diagnostic"],
            serde_json::Value::Null
        );
    }
    let final_alias = reader
        .parse_with_recovery_for_uri(&new_text, Some(&alias_uri))
        .expect("parse final reset alias snapshot");
    assert_eq!(
        final_alias
            .sources
            .key(
                final_alias
                    .sources
                    .id_for_uri(&alias_uri)
                    .expect("final reset alias id")
            )
            .map(|key| key.as_str()),
        Some(source_key.as_str())
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn lsp_persisted_route_cache_replay_precedes_multi_uri_pending_dependency() {
    let root = temp_workspace("lsp_persisted_route_multi_uri");
    let main_path = root.join("main.mimi");
    let dep_path = root.join("dep.mimi");
    let main_text = concat!(
        "use dep\n",
        "func bad(x: i32) -> i32 {\n",
        "    requires: x > 0\n",
        "    ensures: result > 0\n",
        "    0\n",
        "}\n",
        "func main() -> i32 {\n",
        "    0\n",
        "}\n"
    );
    let dep_text = "pub func broken() -> i32 {\n    missing_dep\n}\n";
    fs::write(&main_path, main_text).expect("write persisted-route main");
    fs::write(&dep_path, dep_text).expect("write persisted-route dependency");
    let root_uri = file_uri(&root);
    let main_uri = file_uri(&main_path);
    let dep_uri = file_uri(&dep_path);

    // Persist a sourceful route diagnostic in a separate server instance. The
    // reader below must recover it by SourceKey, not by the writer's numeric
    // SourceId, before it emits dependency notifications.
    let mut writer = crate::lsp::LspServer::new();
    let _ = writer.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    let file = writer
        .parse_with_recovery_for_uri(main_text, Some(&main_uri))
        .expect("parse persisted-route main");
    let bad = file
        .items
        .iter()
        .find_map(|item| match item {
            crate::ast::Item::Func(func) if func.name == "bad" => Some(func),
            _ => None,
        })
        .expect("find persisted-route function");
    let body_hash = crate::lsp::util::hash_func_body(main_text, bad);
    let source_id = file
        .sources
        .id_for_uri(&main_uri)
        .expect("persisted-route source id");
    let source_key = file
        .sources
        .key(source_id)
        .expect("persisted-route source key")
        .as_str()
        .to_string();
    let route = crate::diagnostic::mir_route_error_diagnostic(
        format!(
            "persisted multi-uri wrapper: {}: stale receipt",
            crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE
        ),
        crate::span::Span::new(3, 5, 3, 12).with_source(source_id),
    );
    let cache_key = crate::lsp::verification_cache_key(&main_uri, "bad");
    writer.insert_verification_cache_with_diagnostic(
        cache_key.clone(),
        body_hash,
        crate::verifier::VerifStatus::Failed,
        route.message.clone(),
        route,
    );
    writer.save_cache();
    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".mimi/verify_cache.json")).expect("read persisted route"),
    )
    .expect("parse persisted route cache");
    assert_eq!(
        persisted["entries"][cache_key.as_str()]["diagnostic"]["code"],
        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
        "writer must persist the structured route code"
    );
    assert_eq!(
        persisted["entries"][cache_key.as_str()]["diagnostic"]["source_key"],
        source_key,
        "writer must persist the stable source key alongside the route code"
    );

    let mut reader = crate::lsp::LspServer::new();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "rootUri": root_uri }
    }));
    assert!(
        reader.verification_cache.contains_key(&cache_key),
        "reader must load the engine-qualified cache entry"
    );
    // Put the restored route entry behind a full LRU of session verdicts, then
    // fill the source registry so didOpen crosses the reset boundary. The
    // subsequent didChange cache hit must refresh the route entry after reset.
    for index in 0..crate::lsp::MAX_SOURCE_RECORDS {
        let seed = format!("func reset_seed_{index}() -> i32 {{\n    {index}\n}}\n");
        reader
            .parse_with_recovery_for_uri(&seed, None)
            .expect("fill reader source registry");
    }
    for index in 0..(crate::lsp::MAX_VERIFICATION_CACHE - 1) {
        reader.cache_put_verification(
            format!("session-cold-{index}"),
            crate::lsp::VerificationCacheEntry::new(
                index as u64,
                crate::verifier::VerifStatus::Proven,
                "session proof".to_string(),
                None,
            ),
        );
    }
    assert_eq!(
        reader.verification_cache.len(),
        crate::lsp::MAX_VERIFICATION_CACHE,
        "restored route and session verdicts must share one bounded LRU"
    );
    // didChange verifies the function at the editor's most recent cursor. Use
    // the real hover request to set that 0-indexed cursor to `bad`, which starts
    // on the second source line after the import.
    let opened = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": main_uri,
                "version": 1,
                "text": main_text
            }
        }
    }));
    assert_eq!(
        opened.expect("didOpen should publish the main document")["method"],
        "textDocument/publishDiagnostics"
    );
    let _ = reader.drain_pending_notifications();
    let _ = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": main_uri },
            "position": { "line": 1, "character": 0 }
        }
    }));
    let response = reader.handle_message(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": main_uri, "version": 1 },
            "contentChanges": [{ "text": main_text }]
        }
    }));
    let response = response.expect("didChange should publish the main document");
    assert_eq!(response["method"], "textDocument/publishDiagnostics");
    assert_eq!(response["params"]["uri"], main_uri);
    assert_eq!(
        response["params"]["diagnostics"][0]["code"],
        crate::core::mir::MIR_ROUTE_RECEIPT_ERROR_CODE,
        "persisted SourceKey replay must remain the primary route diagnostic"
    );

    let pending = reader.drain_pending_notifications();
    assert_eq!(
        pending.len(),
        1,
        "dependency checker batch should be pending"
    );
    assert_eq!(pending[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(pending[0]["params"]["uri"], dep_uri);
    assert!(pending[0]["params"]["diagnostics"]
        .as_array()
        .is_some_and(|items| {
            items
                .iter()
                .any(|diagnostic| diagnostic["message"] == "undefined variable 'missing_dep'")
        }));
    reader.cache_put_verification(
        "post-pending-fresh".to_string(),
        crate::lsp::VerificationCacheEntry::new(
            99,
            crate::verifier::VerifStatus::Proven,
            "fresh proof".to_string(),
            None,
        ),
    );
    assert!(
        reader.verification_cache.contains_key(&cache_key),
        "a cache hit before pending delivery must refresh the persisted route entry"
    );
    assert!(
        !reader.verification_cache.contains_key("session-cold-0"),
        "the oldest cold entry should be evicted after the touched route survives"
    );

    let _ = fs::remove_dir_all(root);
}
