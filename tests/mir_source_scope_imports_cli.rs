use std::path::PathBuf;
use std::process::Command;

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return path.into();
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/mimi")
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn mir_source_scope_rejects_excluded_import_edge_and_all_includes_import_graph() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let directory = TemporaryDirectory(std::env::temp_dir().join(format!(
        "mimi-mir-source-scope-imports-{}-{unique}",
        std::process::id()
    )));
    std::fs::create_dir_all(&directory.0).expect("create source-scope import directory");

    std::fs::write(
        directory.0.join("right.mimi"),
        "pub func right() -> i32 { 41 }\n",
    )
    .expect("write right module");
    std::fs::write(
        directory.0.join("left.mimi"),
        "use right;\npub func left() -> i32 { right() }\n",
    )
    .expect("write left module");
    let main = directory.0.join("main.mimi");
    std::fs::write(&main, "use left;\nfunc main() -> i32 { left() }\n")
        .expect("write entry module");

    let source_scope = Command::new(mimi_bin())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("mir")
        .arg(&main)
        .output()
        .expect("run source-scoped MIR inspection");
    assert!(
        !source_scope.status.success(),
        "source-scoped MIR must reject a direct call into an excluded import"
    );
    assert!(
        source_scope.stdout.is_empty(),
        "source-scope rejection must not emit a partial MIR graph: {}",
        String::from_utf8_lossy(&source_scope.stdout)
    );
    let source_stderr = String::from_utf8_lossy(&source_scope.stderr);
    assert!(
        source_stderr.contains("MIR-COVERAGE-001")
            && source_stderr.contains("source-scoped MIR excludes imported callable")
            && source_stderr.contains("function:left")
            && source_stderr.contains("left.mimi")
            && source_stderr.contains("function:main")
            && source_stderr.contains("main.mimi"),
        "source-scope failure must identify the excluded callable/source and retained caller/source: {source_stderr}"
    );
    assert!(
        !source_stderr.contains("has no complete Checker call census"),
        "source-scope failure leaked the downstream ManagedAny census error: {source_stderr}"
    );

    let all_scope = Command::new(mimi_bin())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["mir", "--all", "--receipt"])
        .arg(&main)
        .output()
        .expect("run complete-graph MIR inspection");
    assert!(
        all_scope.status.success(),
        "--all must include the imported callable graph:\n{}\n{}",
        String::from_utf8_lossy(&all_scope.stdout),
        String::from_utf8_lossy(&all_scope.stderr)
    );
    let all_stdout = String::from_utf8_lossy(&all_scope.stdout);
    assert!(
        all_stdout.contains("root_owners=")
            && all_stdout.contains("function:left")
            && all_stdout.contains("function:right"),
        "--all receipt must contain both imported parse owners: {all_stdout}"
    );
}

#[test]
fn mir_source_scope_rejects_alias_qualified_same_name_import_edge() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let directory = TemporaryDirectory(std::env::temp_dir().join(format!(
        "mimi-mir-source-scope-alias-{}-{unique}",
        std::process::id()
    )));
    let apps = directory.0.join("apps");
    std::fs::create_dir_all(&apps).expect("create source-scope alias modules");
    std::fs::write(
        directory.0.join("mimi.toml"),
        "[package]\nname = \"source_scope_alias_fixture\"\nversion = \"0.1.0\"\nentry = \"main.mimi\"\n",
    )
    .expect("write source-scope package manifest");
    std::fs::write(
        apps.join("left.mimi"),
        "pub func parse(value: i32) -> i32 { value + 10 }\n",
    )
    .expect("write left same-name export");
    std::fs::write(
        apps.join("right.mimi"),
        "pub func parse(left: i32, right: i32) -> i32 { left + right + 20 }\n",
    )
    .expect("write right same-name export");
    let main = directory.0.join("main.mimi");
    std::fs::write(
        &main,
        "use apps::left as left;\nuse apps::right as right;\nfunc main() -> i32 { println(left.parse(1)); println(right.parse(2, 3)); 0 }\n",
    )
    .expect("write alias-qualified entry");

    let source_scope = Command::new(mimi_bin())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("mir")
        .arg(&main)
        .output()
        .expect("run source-scoped MIR inspection for aliases");
    assert!(
        !source_scope.status.success(),
        "source-scoped MIR must reject alias calls into excluded imports"
    );
    assert!(
        source_scope.stdout.is_empty(),
        "alias source-scope rejection must not emit a partial MIR graph: {}",
        String::from_utf8_lossy(&source_scope.stdout)
    );
    let stderr = String::from_utf8_lossy(&source_scope.stderr);
    assert!(
        stderr.contains("MIR-COVERAGE-001")
            && stderr.contains("source-scoped MIR excludes imported callable")
            && stderr.contains("function:main")
            && stderr.contains("main.mimi")
            && (stderr.contains("function:apps::left::parse")
                || stderr.contains("function:apps::right::parse")),
        "source-scope diagnostic must use the resolved aliased module identity: {stderr}"
    );
    assert!(
        !stderr.contains("has no complete Checker call census"),
        "alias source-scope rejection leaked a downstream implementation error: {stderr}"
    );
}
