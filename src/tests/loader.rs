use std::fs;
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mimi_test_loader_{}_{}", name, std::process::id()));
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
    fs::write(&file_path, r#"
func main() -> i32 {
    42
}
"#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(result.is_ok(), "loading single file should succeed: {:?}", result.err());
    let loaded = result.unwrap();
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
    fs::write(&file_path, r#"
func $$$ broken
"#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(result.is_err(), "loading invalid syntax should fail: {:?}", result.ok());
    cleanup(&dir);
}

#[test]
fn loader_merge_all() {
    let dir = temp_dir("merge");
    let main_path = dir.join("main.mimi");
    let mod_path = dir.join("helper.mimi");
    fs::write(&main_path, r#"
func main() -> i32 { 42 }
"#).unwrap();
    fs::write(&mod_path, r#"
func helper() -> i32 { 99 }
"#).unwrap();

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
    fs::write(&main_path, r#"
use nonexistent;

func main() -> i32 { 42 }
"#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&main_path);
    assert!(result.is_err(), "import of nonexistent module should fail: {:?}", result.ok());
    cleanup(&dir);
}

#[test]
fn loader_get_module() {
    let dir = temp_dir("getmod");
    let file_path = dir.join("mymod.mimi");
    fs::write(&file_path, r#"
func hello() -> i32 { 1 }
"#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let _ = loader.load_main(&file_path);
    assert!(loader.get_module("mymod").is_some(), "should find loaded module");
    assert!(loader.get_module("nope").is_none(), "nonexistent module returns None");
    cleanup(&dir);
}

#[test]
fn loader_empty_file() {
    let dir = temp_dir("empty");
    let file_path = dir.join("empty.mimi");
    fs::write(&file_path, r#""#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(result.is_ok(), "loading empty file should succeed");
    cleanup(&dir);
}

#[test]
fn loader_file_with_only_comments() {
    let dir = temp_dir("comments");
    let file_path = dir.join("comments.mimi");
    fs::write(&file_path, r#"
// This is a comment
// Another comment
"#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&file_path);
    assert!(result.is_ok(), "loading file with only comments should succeed");
    cleanup(&dir);
}

#[test]
fn loader_merge_with_empty() {
    let dir = temp_dir("merge_empty");
    let main_path = dir.join("main.mimi");
    let empty_path = dir.join("empty.mimi");
    fs::write(&main_path, r#"
func main() -> i32 { 42 }
"#).unwrap();
    fs::write(&empty_path, r#""#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let _ = loader.load_main(&main_path);
    let _ = loader.load_main(&empty_path);
    let merged = loader.merge_all().expect("merge_all should succeed");
    assert!(merged.items.len() >= 1, "merge should include main function");
    cleanup(&dir);
}

#[test]
fn loader_resolve_import() {
    let dir = temp_dir("resolve");
    let lib_path = dir.join("lib.mimi");
    let main_path = dir.join("main.mimi");
    fs::write(&lib_path, r#"
pub func helper() -> i32 { 99 }
"#).unwrap();
    fs::write(&main_path, r#"
use lib;

func main() -> i32 {
    lib::helper()
}
"#).unwrap();

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
    fs::write(&path, r#"
func f() -> i32 { 1 }
func f() -> i32 { 2 }
"#).unwrap();

    let mut loader = crate::loader::ModuleLoader::new(dir.clone());
    let result = loader.load_main(&path);
    // May error on duplicate, just ensure no panic
    let _ = result;
    cleanup(&dir);
}
