use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-map-values-snapshot-alias-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create isolated Map Values alias directory");
        Self(path)
    }

    fn main_file(&self) -> PathBuf {
        self.0.join("main.mimi")
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn run_check(source: &str, main_file: &Path) -> Output {
    run_check_with_stdlib(
        source,
        main_file,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("std"),
    )
}

fn run_check_with_stdlib(source: &str, main_file: &Path, stdlib: &Path) -> Output {
    std::fs::write(main_file, source).expect("write Map Values alias source");
    Command::new(mimi_bin())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("MIMI_STDLIB", stdlib)
        .arg("check")
        .arg(main_file)
        .output()
        .expect("run Mimi Checker against Map Values alias source")
}

#[test]
fn imported_map_values_alias_is_order_independent_and_keeps_any_source_gate() {
    let directory = TemporaryDirectory::new();
    let main_file = directory.main_file();
    let accepted = run_check(
        r#"
use std::maps

func snapshot_values(root: Record) -> MapValuesSnapshot {
    values(root)
}

func caller(root: Record) -> i32 {
    let snapshot: MapValuesSnapshot = snapshot_values(root)
    let first = snapshot[0] as i32
    drop(snapshot)
    first
}

func main() -> i32 {
    let root = map_new()
    let result = caller(root)
    drop(root)
    result
}
"#,
        &main_file,
    );
    assert!(
        accepted.status.success(),
        "the root module must resolve the imported public alias even though main.mimi sorts before std/maps.mimi:\n{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let rejected = run_check(
        r#"
use std::maps

func raw_any() -> List<Any> {
    []
}

func main() -> i32 { 0 }
"#,
        &main_file,
    );
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(!rejected.status.success(), "user code must not spell Any");
    assert!(
        stderr.contains("E0407") && stderr.contains("Any"),
        "direct user-authored Any must retain the source restriction, got:\n{stderr}"
    );
}

#[test]
fn generic_imported_alias_preserves_user_any_provenance() {
    let directory = TemporaryDirectory::new();
    let stdlib = directory.0.join("stdlib");
    std::fs::create_dir(&stdlib).expect("create isolated stdlib directory");
    std::fs::write(stdlib.join("generic.mimi"), "pub type Wrap<T> = List<T>\n")
        .expect("write stdlib generic alias");
    let early_main = directory.main_file();
    let accepted = run_check_with_stdlib(
        r#"
use std::generic

func valid_scalar_alias() -> Wrap<i64> { [1] }

func main() -> i32 { 0 }
"#,
        &early_main,
        &stdlib,
    );
    assert!(
        accepted.status.success(),
        "an imported generic alias should be available when main.mimi sorts before the imported module:\n{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let predeclared_any = run_check_with_stdlib(
        r#"
use std::generic

func invalid_user_any() -> Wrap<Any> { [] }

func main() -> i32 { 0 }
"#,
        &early_main,
        &stdlib,
    );
    let predeclared_stderr = String::from_utf8_lossy(&predeclared_any.stderr);
    assert!(
        !predeclared_any.status.success()
            && predeclared_stderr.contains("E0407")
            && predeclared_stderr.contains("Any"),
        "the predeclared generic alias must retain the caller's Any provenance, got:\n{predeclared_stderr}"
    );

    let main_file = directory.0.join("zmain.mimi");
    let result = run_check_with_stdlib(
        r#"
use std::generic

func valid_scalar_alias() -> Wrap<i64> { [1] }
func invalid_user_any() -> Wrap<Any> { [] }

func main() -> i32 { 0 }
"#,
        &main_file,
        &stdlib,
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success(),
        "an imported generic alias must not make user-authored Any legal"
    );
    assert!(
        stderr.contains("E0407") && stderr.contains("Any"),
        "user Any must retain its own provenance through generic alias substitution, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("unknown type 'Wrap'") && !stderr.contains("type 'Wrap' not found"),
        "the scalar generic alias should resolve before rejecting Any, got:\n{stderr}"
    );
}
