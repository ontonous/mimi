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
            "        do { return Start {} }\n",
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
    let mut checker = crate::core::Checker::new(&file);
    checker.check().expect("check ownership source");
    let ledger = checker
        .ownership_ledgers
        .get(&crate::core::NodeId("function:close".into()))
        .expect("close ownership ledger");
    assert!(ledger.actions.iter().all(|action| {
        action.span.source_id.is_known() && action.span.start_line > 0 && action.span.start_col > 0
    }));
    assert!(ledger.branch_merges.iter().all(|merge| {
        merge.span.source_id.is_known() && merge.span.start_line > 0 && merge.span.start_col > 0
    }));
    assert!(ledger
        .actions
        .iter()
        .filter(|action| action.kind == crate::core::ResourceActionKind::Drop)
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
