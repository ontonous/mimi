use super::*;

// ===================== Lockfile Tests =====================

#[test]
fn lockfile_create_and_save() {
    let dir = std::env::temp_dir().join("mimi_lockfile_test");
    std::fs::create_dir_all(&dir).expect("src/tests/package_management.rs:8 unwrap failed");

    let mut lf = crate::lockfile::Lockfile::new();
    lf.add_package("foo", "1.0.0", Some("git+https://example.com"), None);
    lf.add_package("bar", "2.5.0", None, Some("sha256:abc123"));

    lf.save(&dir).expect("src/tests/package_management.rs:14 unwrap failed");

    let loaded = crate::lockfile::Lockfile::load(&dir).expect("src/tests/package_management.rs:16 unwrap failed").expect("src/tests/package_management.rs:16 unwrap failed");
    assert_eq!(loaded.package.len(), 2);
    assert_eq!(loaded.package[0].name, "foo");
    assert_eq!(loaded.package[0].version, "1.0.0");
    assert_eq!(loaded.package[1].name, "bar");
    assert_eq!(loaded.package[1].version, "2.5.0");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lockfile_resolve_version_caret() {
    let available = ["0.1.0", "0.2.0", "1.0.0", "1.1.0", "2.0.0"];
    assert_eq!(
        crate::lockfile::Lockfile::resolve_version("^1.0", &available),
        Some("1.1.0".into())
    );
}

#[test]
fn lockfile_resolve_version_exact() {
    let available = ["0.1.0", "1.0.0", "2.0.0"];
    assert_eq!(
        crate::lockfile::Lockfile::resolve_version("1.0.0", &available),
        Some("1.0.0".into())
    );
}

#[test]
fn lockfile_resolve_version_wildcard() {
    let available = ["0.1.0", "1.0.0"];
    assert_eq!(
        crate::lockfile::Lockfile::resolve_version("*", &available),
        Some("1.0.0".into())
    );
}

#[test]
fn lockfile_resolve_version_tilde() {
    let available = ["1.0.0", "1.0.1", "1.0.2", "1.1.0", "2.0.0"];
    assert_eq!(
        crate::lockfile::Lockfile::resolve_version("~1.0", &available),
        Some("1.0.2".into())
    );
}

#[test]
fn lockfile_resolve_version_range() {
    let available = ["0.1.0", "0.5.0", "1.0.0", "1.5.0", "2.0.0"];
    assert_eq!(
        crate::lockfile::Lockfile::resolve_version(">=0.5, <2.0", &available),
        Some("1.5.0".into())
    );
}

// ===================== Manifest Tests =====================

#[test]
fn manifest_add_dependency() {
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    manifest.add_dependency("foo", Some("^1.0"), None);
    manifest.add_dependency("bar", Some("2.0.0"), Some("./local"));

    let deps = manifest.dependencies.expect("src/tests/package_management.rs:79 unwrap failed");
    assert_eq!(deps.len(), 2);
    assert_eq!(deps[0].name, "foo");
    assert_eq!(deps[0].version, Some("^1.0".into()));
    assert_eq!(deps[1].name, "bar");
    assert_eq!(deps[1].path, Some("./local".into()));
}

#[test]
fn manifest_remove_dependency() {
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    manifest.add_dependency("foo", Some("1.0"), None);
    manifest.add_dependency("bar", Some("2.0"), None);

    assert!(manifest.remove_dependency("foo"));
    assert!(!manifest.remove_dependency("foo"));

    let deps = manifest.dependencies.expect("src/tests/package_management.rs:96 unwrap failed");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "bar");
}

#[test]
fn manifest_replace_dependency() {
    let mut manifest = crate::manifest::Manifest::new("test-pkg");
    manifest.add_dependency("foo", Some("1.0"), None);
    manifest.add_dependency("foo", Some("2.0"), None);

    let deps = manifest.dependencies.expect("src/tests/package_management.rs:107 unwrap failed");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].version, Some("2.0".into()));
}

// ===================== Expanded Package Management Tests =====================

#[test]
fn lockfile_resolve_version_no_match() {
    let available = ["1.0.0", "2.0.0", "3.0.0"];
    let result = crate::lockfile::Lockfile::resolve_version("^4.0", &available);
    assert_eq!(result, None, "should return None when no match");
}

#[test]
fn lockfile_resolve_version_invalid_constraint() {
    let available = ["1.0.0", "2.0.0"];
    let result = crate::lockfile::Lockfile::resolve_version("invalid", &available);
    assert_eq!(result, None, "should return None for invalid constraint");
}

#[test]
fn lockfile_resolve_version_empty_available() {
    let available: [&str; 0] = [];
    let result = crate::lockfile::Lockfile::resolve_version("*", &available);
    assert_eq!(result, None, "should return None with empty available list");
}

#[test]
fn lockfile_resolve_version_caret_no_prerelease() {
    let available = ["1.0.0-alpha", "1.0.0", "1.1.0"];
    let result = crate::lockfile::Lockfile::resolve_version("^1.0", &available);
    assert_eq!(result, Some("1.1.0".into()), "should pick latest non-prerelease");
}

#[test]
fn lockfile_resolve_version_tilde_minor() {
    let available = ["1.0.0", "1.0.1", "1.0.2", "1.1.0"];
    let result = crate::lockfile::Lockfile::resolve_version("~1.0", &available);
    assert_eq!(result, Some("1.0.2".into()), "tilde should prefer highest patch");
}

#[test]
fn lockfile_save_and_load_nonexistent() {
    let dir = std::env::temp_dir().join("mimi_lockfile_nonexistent");
    std::fs::create_dir_all(&dir).expect("src/tests/package_management.rs:152 unwrap failed");

    let loaded = crate::lockfile::Lockfile::load(&dir).expect("src/tests/package_management.rs:154 unwrap failed");
    assert!(loaded.is_none(), "no lockfile should return None");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lockfile_add_package_without_source() {
    let mut lf = crate::lockfile::Lockfile::new();
    lf.add_package("foo", "1.0.0", None, None);

    assert_eq!(lf.package.len(), 1);
    assert_eq!(lf.package[0].name, "foo");
}

#[test]
fn manifest_invalid_dependency_path() {
    let mut manifest = crate::manifest::Manifest::new("test");
    manifest.add_dependency("local-dep", None, Some("/nonexistent/path"));

    let deps = manifest.dependencies.as_ref().expect("src/tests/package_management.rs:174 unwrap failed");
    assert_eq!(deps[0].path.as_deref(), Some("/nonexistent/path"));
}

#[test]
fn manifest_find_nonexistent() {
    let result = crate::manifest::Manifest::find(std::path::Path::new("/tmp/nonexistent_path_for_test"));
    // Should not panic, may return Err or Ok(None)
    let _ = result;
}

#[test]
fn test_framework_assert_eq_fails() {
    let src = r#"
        func test_fail() {
            assert_eq(1, 2)
        }
    "#;
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    let result = interp.call_named("test_fail", vec![]);
    assert!(result.is_err(), "assert_eq should fail on unequal values");
}

#[test]
fn codegen_multi_file_with_deps() {
    let src = r#"
        func helper() -> i32 { 42 }
        func main() -> i32 { helper() }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "multi");
    let result = codegen.compile_file(&file);
    assert!(result.is_ok(), "multi-file codegen should succeed: {:?}", result.err());
    let ir = codegen.emit_ir();
    assert!(ir.contains("helper"), "IR should contain helper");
    assert!(ir.contains("main"), "IR should contain main");
}

// ===================== Multi-file Build Tests =====================

#[test]
fn codegen_multi_file_build() {
    let src = r#"
        func add(a: i32, b: i32) -> i32 {
            a + b
        }
        func main() -> i32 {
            add(1, 2)
        }
    "#;
    let file = parse(src);
    let context = inkwell::context::Context::create();
    let mut codegen = crate::codegen::CodeGenerator::new(&context, "test");
    codegen.compile_file(&file).expect("src/tests/package_management.rs:230 unwrap failed");
    let ir = codegen.emit_ir();
    assert!(ir.contains("add"), "IR should contain add function");
    assert!(ir.contains("main"), "IR should contain main function");
}

// ===================== Test Framework Enhancement Tests =====================

#[test]
fn test_framework_assert_eq() {
    let src = r#"
        func test_addition() {
            assert_eq(1 + 1, 2)
        }
    "#;
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    let result = interp.call_named("test_addition", vec![]);
    assert!(result.is_ok());
}

#[test]
fn test_framework_assert_ne() {
    let src = r#"
        func test_not_equal() {
            assert_ne(1, 2)
        }
    "#;
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    let result = interp.call_named("test_not_equal", vec![]);
    assert!(result.is_ok());
}

#[test]
fn test_framework_assert_ne_fails() {
    let src = r#"
        func test_equal() {
            assert_ne(1, 1)
        }
    "#;
    let file = parse(src);
    let mut interp = interp::Interpreter::new(&file);
    interp.verify_contracts = true;
    let result = interp.call_named("test_equal", vec![]);
    assert!(result.is_err());
}
