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
    let merged = loader.merge_all();
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
