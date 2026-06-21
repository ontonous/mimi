use super::*;
#[test]
fn test_framework_finds_test_functions() {
    use crate::ast;
    let src = r#"
func test_addition() -> i32 {
    assert_eq(1 + 1, 2);
    1
}

func test_subtraction() -> i32 {
    assert_eq(5 - 3, 2);
    1
}

func not_a_test() -> i32 {
    42
}

func main() -> i32 {
    0
}
"#;
    let file = parse(src);
    let test_funcs: Vec<String> = file.items.iter().filter_map(|item| {
        match item {
            ast::Item::Func(f) if f.name.starts_with("test_") => Some(f.name.clone()),
            _ => None,
        }
    }).collect();
    assert_eq!(test_funcs.len(), 2);
    assert!(test_funcs.contains(&"test_addition".to_string()));
    assert!(test_funcs.contains(&"test_subtraction".to_string()));
}


#[test]
fn test_framework_run_test_function() {
    let src = r#"
func test_assert_eq_works() -> i32 {
    assert_eq(2 + 2, 4);
    1
}

func main() -> i32 {
    0
}
"#;
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    let result = interp.call_named("test_assert_eq_works", vec![]);
    assert!(result.is_ok());
}


#[test]
fn test_framework_test_failure() {
    let src = r#"
func test_failing() -> i32 {
    assert_eq(1, 2);
    1
}

func main() -> i32 {
    0
}
"#;
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    let result = interp.call_named("test_failing", vec![]);
    assert!(result.is_err());
}


#[test]
fn test_framework_no_tests() {
    use crate::ast;
    let src = r#"
func main() -> i32 {
    42
}
"#;
    let file = parse(src);
    let test_funcs: Vec<String> = file.items.iter().filter_map(|item| {
        match item {
            ast::Item::Func(f) if f.name.starts_with("test_") => Some(f.name.clone()),
            _ => None,
        }
    }).collect();
    assert!(test_funcs.is_empty());
}

// === T503: Package Management Tests ===


#[test]
fn manifest_new() {
    let manifest = crate::manifest::Manifest::new("test-pkg");
    assert!(manifest.package.is_some());
    let pkg = manifest.package.expect("src/tests/v1_2_infra.rs:100 unwrap failed");
    assert_eq!(pkg.name, "test-pkg");
    assert_eq!(pkg.version, Some("0.1.0".to_string()));
    assert!(manifest.dependencies.is_none());
}


#[test]
fn manifest_add_dependency() {
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    manifest.add_dependency("serde", Some("1.0"), None);
    let deps = manifest.dependencies.expect("src/tests/v1_2_infra.rs:111 unwrap failed");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "serde");
    assert_eq!(deps[0].version, Some("1.0".to_string()));
}


#[test]
fn manifest_add_dependency_replace() {
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    manifest.add_dependency("serde", Some("1.0"), None);
    manifest.add_dependency("serde", Some("2.0"), None);
    let deps = manifest.dependencies.expect("src/tests/v1_2_infra.rs:123 unwrap failed");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].version, Some("2.0".to_string()));
}


#[test]
fn manifest_remove_dependency() {
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    manifest.add_dependency("serde", Some("1.0"), None);
    manifest.add_dependency("tokio", Some("1.0"), None);
    assert!(manifest.remove_dependency("serde"));
    let deps = manifest.dependencies.expect("src/tests/v1_2_infra.rs:135 unwrap failed");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "tokio");
}


#[test]
fn manifest_remove_nonexistent() {
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    assert!(!manifest.remove_dependency("nonexistent"));
}


#[test]
fn manifest_save_and_load() {
    let dir = std::env::temp_dir().join("mimi_test_manifest");
    let _ = std::fs::create_dir_all(&dir);
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    manifest.add_dependency("serde", Some("1.0"), None);
    manifest.save(&dir).expect("src/tests/v1_2_infra.rs:154 unwrap failed");
    let loaded = crate::manifest::Manifest::load(&dir).expect("src/tests/v1_2_infra.rs:155 unwrap failed").expect("src/tests/v1_2_infra.rs:155 unwrap failed");
    assert_eq!(loaded.package.expect("src/tests/v1_2_infra.rs:156 unwrap failed").name, "test-pkg");
    let deps = loaded.dependencies.expect("src/tests/v1_2_infra.rs:157 unwrap failed");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "serde");
    let _ = std::fs::remove_dir_all(&dir);
}

// === T500: LSP Tests ===


#[test]
fn lsp_diagnostics_no_errors() {
    use crate::lsp::LspServer;
    let server = LspServer::new();
    let text = r#"
func main() -> i32 {
    42
}
"#;
    let diagnostics = server.compute_diagnostics(text);
    assert!(diagnostics.is_empty());
}


#[test]
fn lsp_diagnostics_parse_error() {
    use crate::lsp::LspServer;
    let server = LspServer::new();
    let text = "func main() -> i32 {";
    let diagnostics = server.compute_diagnostics(text);
    assert!(!diagnostics.is_empty());
}


#[test]
fn lsp_diagnostics_type_error() {
    use crate::lsp::LspServer;
    let server = LspServer::new();
    let text = r#"
func main() -> i32 {
    let x: string = 42;
    x
}
"#;
    let diagnostics = server.compute_diagnostics(text);
    assert!(!diagnostics.is_empty());
}


#[test]
fn lsp_completion_keywords() {
    use crate::lsp::LspServer;
    let server = LspServer::new();
    let text = "";
    let items = server.compute_completion(text, 0, 0);
    let labels: Vec<&str> = items.iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(labels.contains(&"func"));
    assert!(labels.contains(&"type"));
    assert!(labels.contains(&"if"));
}


#[test]
fn lsp_completion_functions() {
    use crate::lsp::LspServer;
    let server = LspServer::new();
    let text = r#"
func my_function() -> i32 {
    42
}

func main() -> i32 {
    my_function()
}
"#;
    let items = server.compute_completion(text, 0, 0);
    let labels: Vec<&str> = items.iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(labels.contains(&"my_function"));
    assert!(labels.contains(&"main"));
}


#[test]
fn lsp_completion_builtins() {
    use crate::lsp::LspServer;
    let server = LspServer::new();
    let text = "";
    let items = server.compute_completion(text, 0, 0);
    let labels: Vec<&str> = items.iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(labels.contains(&"println"));
    assert!(labels.contains(&"len"));
    assert!(labels.contains(&"map"));
    assert!(labels.contains(&"filter"));
}

