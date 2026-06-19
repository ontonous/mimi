use std::fs;
use std::path::PathBuf;

fn temp_dir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("mimi_test_{}_{}", std::process::id(), nanos));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn promote_clean_file() {
    let dir = temp_dir();
    let src_path = dir.join("test.mms");
    fs::write(&src_path, "func add(a: i32, b: i32) -> i32 { a + b }").unwrap();

    let output_path = dir.join("test.mimi");
    let result = crate::main_promote(&src_path, Some(&output_path));
    assert!(result.is_ok(), "promote should succeed: {:?}", result.err());
    assert!(output_path.exists(), "output file should exist");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn promote_rejects_placeholders() {
    let dir = temp_dir();
    let src_path = dir.join("test.mms");
    fs::write(&src_path, "func add(a: i32, b: i32) -> i32 { ... }").unwrap();

    let result = crate::main_promote(&src_path, None);
    assert!(result.is_err(), "promote should fail with ...");
    assert!(result.unwrap_err().contains("..."), "error should mention ...");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn promote_default_output() {
    let dir = temp_dir();
    let src_path = dir.join("test.mms");
    fs::write(&src_path, "func main() { }").unwrap();

    let result = crate::main_promote(&src_path, None);
    assert!(result.is_ok(), "promote should succeed");

    let output_path = dir.join("test.mimi");
    assert!(output_path.exists(), "default output should be .mimi");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_markdown() {
    let dir = temp_dir();
    let src_path = dir.join("test.mimi");
    fs::write(&src_path, "func add(a: i32, b: i32) -> i32 { a + b }\ntype Point { x: i32, y: i32 }").unwrap();

    let result = crate::main_doc(&src_path, "markdown");
    assert!(result.is_ok(), "doc should succeed: {:?}", result.err());

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_empty_file() {
    let dir = temp_dir();
    let src_path = dir.join("empty.mimi");
    fs::write(&src_path, "").unwrap();

    let result = crate::main_doc(&src_path, "markdown");
    assert!(result.is_ok(), "doc should succeed on empty file");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn promote_file_with_type_def() {
    let dir = temp_dir();
    let src_path = dir.join("test.mms");
    fs::write(&src_path, "type Point { x: i32, y: i32 }\nfunc main() { }").unwrap();

    let result = crate::main_promote(&src_path, None);
    assert!(result.is_ok(), "promote should succeed with type def");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_markdown_with_type_and_func() {
    let dir = temp_dir();
    let src_path = dir.join("test.mimi");
    fs::write(&src_path, "type Point { x: i32, y: i32 }\n\nfunc distance(p: Point) -> f64 { sqrt(p.x * p.x + p.y * p.y) }").unwrap();

    let result = crate::main_doc(&src_path, "markdown");
    assert!(result.is_ok(), "doc should succeed with type and func");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_unsupported_format() {
    let dir = temp_dir();
    let src_path = dir.join("test.mimi");
    fs::write(&src_path, "func main() { }").unwrap();

    let result = crate::main_doc(&src_path, "html");
    let _ = result;

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn promote_nonexistent_file() {
    let dir = temp_dir();
    let src_path = dir.join("nonexistent.mms");

    let result = crate::main_promote(&src_path, None);
    assert!(result.is_err(), "promote should fail on nonexistent file");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_nonexistent_file() {
    let dir = temp_dir();
    let src_path = dir.join("nonexistent.mimi");

    let result = crate::main_doc(&src_path, "markdown");
    assert!(result.is_err(), "doc should fail on nonexistent file");

    // Cleanup
    fs::remove_dir_all(&dir).ok();
}
