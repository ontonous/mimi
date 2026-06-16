use crate::manifest::Manifest;
use std::fs;
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mimi_test_manifest_{}_{}", name, std::process::id()));
    let _ = fs::create_dir_all(&dir);
    dir
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn manifest_new() {
    let m = Manifest::new("test-project");
    assert!(m.package.is_some());
    let pkg = m.package.unwrap();
    assert_eq!(pkg.name, "test-project");
    assert_eq!(pkg.version.as_deref(), Some("0.1.0"));
    assert_eq!(pkg.entry.as_deref(), Some("main.mimi"));
    assert!(m.dependencies.is_none());
}

#[test]
fn manifest_save_and_load() {
    let dir = temp_dir("save_load");
    let m = Manifest::new("myproject");
    m.save(&dir).unwrap();

    let loaded = Manifest::load(&dir).unwrap();
    assert!(loaded.is_some(), "should find mimi.toml after save");
    let loaded = loaded.unwrap();
    assert_eq!(loaded.package.as_ref().unwrap().name, "myproject");
    cleanup(&dir);
}

#[test]
fn manifest_load_nonexistent() {
    let dir = temp_dir("load_none");
    let result = Manifest::load(&dir).unwrap();
    assert!(result.is_none(), "no mimi.toml should return None");
    cleanup(&dir);
}

#[test]
fn manifest_add_dependency() {
    let mut m = Manifest::new("test");
    m.add_dependency("foo", Some("1.0"), None);
    m.add_dependency("bar", None, Some("./bar"));

    let deps = m.dependencies.as_ref().unwrap();
    assert_eq!(deps.len(), 2);
    assert_eq!(deps[0].name, "foo");
    assert_eq!(deps[0].version.as_deref(), Some("1.0"));
    assert_eq!(deps[1].name, "bar");
    assert_eq!(deps[1].path.as_deref(), Some("./bar"));
}

#[test]
fn manifest_add_duplicate_replaces() {
    let mut m = Manifest::new("test");
    m.add_dependency("foo", Some("1.0"), None);
    m.add_dependency("foo", Some("2.0"), None);

    let deps = m.dependencies.as_ref().unwrap();
    assert_eq!(deps.len(), 1, "duplicate should be replaced");
    assert_eq!(deps[0].version.as_deref(), Some("2.0"));
}

#[test]
fn manifest_remove_dependency() {
    let mut m = Manifest::new("test");
    m.add_dependency("foo", Some("1.0"), None);
    m.add_dependency("bar", Some("2.0"), None);

    let removed = m.remove_dependency("foo");
    assert!(removed, "should return true when removing existing dep");

    let deps = m.dependencies.as_ref().unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "bar");
}

#[test]
fn manifest_remove_nonexistent() {
    let mut m = Manifest::new("test");
    let removed = m.remove_dependency("nope");
    assert!(!removed, "removing nonexistent dep should return false");
}

#[test]
fn manifest_entry_path() {
    let m = Manifest::new("test");
    let entry = m.entry_path(std::path::Path::new("/project"));
    assert_eq!(entry, PathBuf::from("/project/main.mimi"));
}

#[test]
fn manifest_find_up() {
    let dir = temp_dir("find_up");
    let subdir = dir.join("deep").join("nested");
    fs::create_dir_all(&subdir).unwrap();

    let m = Manifest::new("found");
    m.save(&dir).unwrap();

    let result = Manifest::find(&subdir);
    assert!(result.is_ok());
    let found = result.unwrap();
    assert!(found.is_some(), "should find mimi.toml in parent directory");
    let (_found_dir, manifest) = found.unwrap();
    assert_eq!(manifest.package.as_ref().unwrap().name, "found");
    cleanup(&dir);
}

#[test]
fn manifest_invalid_toml() {
    let dir = temp_dir("invalid_toml");
    fs::write(dir.join("mimi.toml"), "this is not [valid toml {{{{").unwrap();
    let result = Manifest::load(&dir);
    assert!(result.is_err(), "invalid TOML should return error");
    cleanup(&dir);
}

#[test]
fn manifest_dependency_serialization() {
    let mut m = Manifest::new("test");
    m.add_dependency("dep1", Some("0.5.0"), Some("./local"));

    let toml_str = toml::to_string_pretty(&m).unwrap();
    assert!(toml_str.contains("dep1"));
    assert!(toml_str.contains("0.5.0"));

    let deserialized: Manifest = toml::from_str(&toml_str).unwrap();
    let deps = deserialized.dependencies.as_ref().unwrap();
    assert_eq!(deps[0].name, "dep1");
}
