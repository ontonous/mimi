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
