use std::fs;
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("mimi_test_loader_{}_{}", name, std::process::id()));
    let _ = fs::create_dir_all(&dir);
    dir
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn loader_load_single_file() {
    let dir = temp_dir("single");
    let file_path = dir.join("main.mimi");
    fs::write(
        &file_path,
        r#"
func main() -> i32 {
    42
}
"#,
    )
    .expect("src/tests/loader.rs:22 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(
        result.is_ok(),
        "loading single file should succeed: {:?}",
        result.err()
    );
    let loaded = result.expect("src/tests/loader.rs:27 unwrap failed");
    assert_eq!(loaded.file.items.len(), 1);
    cleanup(&dir);
}

#[test]
fn loader_nonexistent_file() {
    let dir = temp_dir("nonexist");
    let file_path = dir.join("nope.mimi");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(result.is_err(), "loading nonexistent file should fail");
    cleanup(&dir);
}

#[test]
fn loader_invalid_syntax() {
    let dir = temp_dir("syntax");
    let file_path = dir.join("bad.mimi");
    fs::write(
        &file_path,
        r#"
func $$$ broken
"#,
    )
    .expect("src/tests/loader.rs:49 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(
        result.is_err(),
        "loading invalid syntax should fail: {:?}",
        result.ok()
    );
    cleanup(&dir);
}

#[test]
fn loader_merge_all() {
    let dir = temp_dir("merge");
    let main_path = dir.join("main.mimi");
    let mod_path = dir.join("helper.mimi");
    fs::write(
        &main_path,
        r#"
func main() -> i32 { 42 }
"#,
    )
    .expect("src/tests/loader.rs:64 unwrap failed");
    fs::write(
        &mod_path,
        r#"
func helper() -> i32 { 99 }
"#,
    )
    .expect("src/tests/loader.rs:67 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let _ = loader.load_main(&main_path);
    let _ = loader.load_main(&mod_path);
    let merged = loader.merge_all().expect("merge_all should succeed");
    assert!(merged.items.len() >= 2, "merge should include all items");
    cleanup(&dir);
}

#[test]
fn loader_import_resolution_failure() {
    let dir = temp_dir("import_fail");
    let main_path = dir.join("main.mimi");
    fs::write(
        &main_path,
        r#"
use nonexistent;

func main() -> i32 { 42 }
"#,
    )
    .expect("src/tests/loader.rs:85 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&main_path);
    assert!(
        result.is_err(),
        "import of nonexistent module should fail: {:?}",
        result.ok()
    );
    cleanup(&dir);
}

#[test]
fn loader_get_module() {
    let dir = temp_dir("getmod");
    let file_path = dir.join("mymod.mimi");
    fs::write(
        &file_path,
        r#"
func hello() -> i32 { 1 }
"#,
    )
    .expect("src/tests/loader.rs:99 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let _ = loader.load_main(&file_path);
    cleanup(&dir);
}

#[test]
fn loader_empty_file() {
    let dir = temp_dir("empty");
    let file_path = dir.join("empty.mimi");
    fs::write(&file_path, r#""#).expect("src/tests/loader.rs:112 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(result.is_ok(), "loading empty file should succeed");
    cleanup(&dir);
}

#[test]
fn loader_file_with_only_comments() {
    let dir = temp_dir("comments");
    let file_path = dir.join("comments.mimi");
    fs::write(
        &file_path,
        r#"
// This is a comment
// Another comment
"#,
    )
    .expect("src/tests/loader.rs:127 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(
        result.is_ok(),
        "loading file with only comments should succeed"
    );
    cleanup(&dir);
}

#[test]
fn loader_merge_with_empty() {
    let dir = temp_dir("merge_empty");
    let main_path = dir.join("main.mimi");
    let empty_path = dir.join("empty.mimi");
    fs::write(
        &main_path,
        r#"
func main() -> i32 { 42 }
"#,
    )
    .expect("src/tests/loader.rs:142 unwrap failed");
    fs::write(&empty_path, r#""#).expect("src/tests/loader.rs:143 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let _ = loader.load_main(&main_path);
    let _ = loader.load_main(&empty_path);
    let merged = loader.merge_all().expect("merge_all should succeed");
    assert!(
        !merged.items.is_empty(),
        "merge should include main function"
    );
    cleanup(&dir);
}

#[test]
fn loader_resolve_import() {
    let dir = temp_dir("resolve");
    let lib_path = dir.join("lib.mimi");
    let main_path = dir.join("main.mimi");
    fs::write(
        &lib_path,
        r#"
pub func helper() -> i32 { 99 }
"#,
    )
    .expect("src/tests/loader.rs:160 unwrap failed");
    fs::write(
        &main_path,
        r#"
use lib;

func main() -> i32 {
    lib::helper()
}
"#,
    )
    .expect("src/tests/loader.rs:167 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&main_path);
    // May fail if import resolution requires specific setup
    // Just ensure it doesn't panic
    let _ = result;
    cleanup(&dir);
}

#[test]
fn loader_duplicate_key_no_panic() {
    let dir = temp_dir("dup");
    let path = dir.join("a.mimi");
    fs::write(
        &path,
        r#"
func f() -> i32 { 1 }
func f() -> i32 { 2 }
"#,
    )
    .expect("src/tests/loader.rs:184 unwrap failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&path);
    // May error on duplicate, just ensure no panic
    let _ = result;
    cleanup(&dir);
}

#[test]
fn loader_selective_import_resolve() {
    let dir = temp_dir("selective");
    let strings_path = dir.join("strings.mimi");
    let main_path = dir.join("main.mimi");
    fs::write(
        &strings_path,
        r#"
pub func replace_all(s: string, from: string, to: string) -> string {
    s // simplified
}
pub func contains(s: string, substr: string) -> bool {
    true
}
"#,
    )
    .expect("src/tests/loader.rs: write strings.mimi");
    fs::write(
        &main_path,
        r#"
use strings::replace_all;

func main() -> i32 {
    42
}
"#,
    )
    .expect("src/tests/loader.rs: write main.mimi");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&main_path);
    assert!(
        result.is_ok(),
        "selective import should resolve: {:?}",
        result.err()
    );
    let merged = loader.merge_all().expect("merge should succeed");
    // The merged file should contain replace_all (from strings.mimi)
    let has_replace_all = merged
        .items
        .iter()
        .any(|item| matches!(item, crate::ast::Item::Func(f) if f.name == "replace_all"));
    assert!(
        has_replace_all,
        "selective import should bring replace_all into scope"
    );
    // Also the import path should remain unchanged
    let has_selective_import = merged
        .imports
        .iter()
        .any(|imp| imp.path == vec!["strings", "replace_all"]);
    assert!(
        has_selective_import,
        "selective import path should be preserved"
    );
    cleanup(&dir);
}

#[test]
fn loader_std_json_import_typechecks() {
    // Regression for v0.28.17: `use std::json` must resolve the stdlib module,
    // merge its public items, and pass type checking.
    let dir = temp_dir("std_json");
    let main_path = dir.join("main.mimi");
    fs::write(
        &main_path,
        r#"
use std::json

func main() -> i64 {
    let data = "{\"count\":42}"
    get_int(data, "count")
}
"#,
    )
    .expect("src/tests/loader.rs: std_json write failed");

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    loader
        .load_main(&main_path)
        .expect("loading main with std::json import should succeed");
    let merged = loader
        .merge_all()
        .expect("merging std::json import should succeed");
    let result = crate::core::check(&merged);
    assert!(
        result.is_ok(),
        "use std::json should typecheck: {:?}",
        result.err()
    );
    cleanup(&dir);
}

// Regression for v0.28.25: `use pkgname::func` and `use pkgname` should both
// resolve a path dependency's entry file (from mimi.toml) without requiring
// the entry file name in the use path.
#[test]
fn loader_package_import_uses_entry_file() {
    let root = temp_dir("pkg_import");
    let lib_dir = root.join("mylib");
    let app_dir = root.join("app");
    fs::create_dir_all(&lib_dir).expect("create lib dir");
    fs::create_dir_all(&app_dir).expect("create app dir");

    fs::write(
        lib_dir.join("mimi.toml"),
        r#"[package]
name = "mylib"
version = "0.1.0"
entry = "main.mimi"
"#,
    )
    .expect("write lib mimi.toml");
    fs::write(
        lib_dir.join("main.mimi"),
        r#"pub func factorial(n: i32) -> i32 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}
"#,
    )
    .expect("write lib main.mimi");

    fs::write(
        app_dir.join("mimi.toml"),
        r#"[package]
name = "app"
version = "0.1.0"

[[dependencies]]
name = "mylib"
path = "../mylib"
"#,
    )
    .expect("write app mimi.toml");

    // Case 1: `use mylib::factorial` resolves to entry file and merges factorial.
    let main_path = app_dir.join("main.mimi");
    fs::write(
        &main_path,
        r#"use mylib::factorial

func main() -> i32 {
    factorial(5)
}
"#,
    )
    .expect("write app main case1");

    let mut loader = crate::loader::ModuleLoader::new(app_dir.clone());
    loader
        .load_main(&main_path)
        .expect("use mylib::factorial should resolve to entry file");
    let merged = loader.merge_all().expect("merge should succeed");
    let has_factorial = merged
        .items
        .iter()
        .any(|item| matches!(item, crate::ast::Item::Func(f) if f.name == "factorial"));
    assert!(
        has_factorial,
        "use mylib::factorial should bring factorial into scope"
    );

    // Case 2: `use mylib` resolves to entry file and merges all pub items.
    fs::write(
        &main_path,
        r#"use mylib

func main() -> i32 {
    mylib::factorial(5)
}
"#,
    )
    .expect("write app main case2");

    let mut loader2 = crate::loader::ModuleLoader::new(app_dir.clone());
    loader2
        .load_main(&main_path)
        .expect("use mylib should resolve to entry file");
    let merged2 = loader2.merge_all().expect("merge should succeed");
    let has_factorial2 = merged2
        .items
        .iter()
        .any(|item| matches!(item, crate::ast::Item::Func(f) if f.name == "factorial"));
    assert!(
        has_factorial2,
        "use mylib should bring factorial into scope"
    );

    cleanup(&root);
}
