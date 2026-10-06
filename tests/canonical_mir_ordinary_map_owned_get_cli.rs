use std::path::{Path, PathBuf};
use std::process::Command;

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide the mimi binary for typed owned-Get CLI evidence")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn assert_owned_get_route(stderr: &str, mode: &str) {
    assert!(
        stderr.contains(
            "canonical route disposition: canonical (ordinary-map-owned-get-i32-string-v1) mir_digest="
        ),
        "{mode} did not report the typed owned-Get route: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} fell back to a legacy Map owner: {stderr}"
    );
}

#[test]
fn ordinary_map_typed_owned_get_uses_default_run_build_verify_route() {
    let mimi = mimi_bin();
    let source = fixture("mir_ordinary_map_owned_get_default.mimi");
    let expected_stdout = "";

    let run = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("run")
        .arg(&source)
        .output()
        .expect("run typed ordinary Map owned-Get fixture");
    assert!(
        run.status.success(),
        "mimi run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), expected_stdout);
    assert_owned_get_route(&String::from_utf8_lossy(&run.stderr), "mimi run");

    let binary = std::env::temp_dir().join(format!(
        "mimi-ordinary-map-owned-get-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("cli")
    ));
    let build = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&binary)
        .output()
        .expect("build typed ordinary Map owned-Get fixture");
    assert!(
        build.status.success(),
        "mimi build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert_owned_get_route(&String::from_utf8_lossy(&build.stderr), "mimi build");

    let native = Command::new(&binary)
        .output()
        .expect("run built typed ordinary Map owned-Get binary");
    let _ = std::fs::remove_file(&binary);
    assert!(
        native.status.success(),
        "native binary failed:\nstderr:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected_stdout);

    let verify = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("verify")
        .arg(&source)
        .output()
        .expect("verify typed ordinary Map owned-Get fixture");
    let verify_stdout = String::from_utf8_lossy(&verify.stdout);
    let verify_stderr = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify.status.success(),
        "mimi verify failed:\nstdout:\n{verify_stdout}\nstderr:\n{verify_stderr}"
    );
    assert!(
        verify_stdout.contains("No contracts to verify"),
        "the no-contract fixture should retain the empty verification disposition: {verify_stdout}"
    );
    assert_owned_get_route(&verify_stderr, "mimi verify");
}

#[test]
fn ordinary_map_owned_get_rejects_bare_list_composition_before_backend() {
    let mimi = mimi_bin();
    let source = fixture("mir_ordinary_map_owned_get_mixed_list_reject.mimi");
    let run = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("run")
        .arg(&source)
        .output()
        .expect("run mixed ordinary Map/List fixture");
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        !run.status.success(),
        "ordinary Map owned-get must reject a bare List composition: {transcript}"
    );
    assert!(
        transcript.contains("MIR-COVERAGE-001")
            && transcript.contains("mixed scalar-collection admission"),
        "bare List composition should fail closed at the route gate: {transcript}"
    );
    assert!(
        !transcript.contains("canonical route disposition: legacy"),
        "bare List composition must not fall back to legacy: {transcript}"
    );
}

#[test]
fn ordinary_map_values_snapshot_uses_default_run_build_verify_route() {
    let mimi = mimi_bin();
    let source = fixture("mir_ordinary_map_values.mimi");

    let run = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("run")
        .arg(&source)
        .output()
        .expect("run ordinary Map values fixture");
    assert!(
        run.status.success(),
        "mimi run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "");
    assert!(
        String::from_utf8_lossy(&run.stderr).contains(
            "canonical route disposition: canonical (ordinary-map-owned-get-i32-string-v1)"
        ),
        "ordinary Map values did not report the owned-get route: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-ordinary-map-values-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("cli")
    ));
    let build = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("build")
        .arg(&source)
        .arg("--output")
        .arg(&binary)
        .output()
        .expect("build ordinary Map values fixture");
    assert!(
        build.status.success(),
        "mimi build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("run built ordinary Map values binary");
    let _ = std::fs::remove_file(&binary);
    assert!(
        native.status.success(),
        "native ordinary Map values binary failed:\nstderr:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&native.stdout), "");

    let receipt = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("mir")
        .arg("--receipt")
        .arg(&source)
        .output()
        .expect("inspect ordinary Map values MIR receipt");
    assert!(
        receipt.status.success(),
        "MIR receipt inspection failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&receipt.stdout),
        String::from_utf8_lossy(&receipt.stderr)
    );
    let receipt_stdout = String::from_utf8_lossy(&receipt.stdout);
    let receipt_stderr = String::from_utf8_lossy(&receipt.stderr);
    let receipt_transcript = format!("{receipt_stdout}{receipt_stderr}");
    assert!(
        receipt_transcript.contains("lowered 1 callable(s) to canonical MIR")
            && !receipt_transcript.contains("canonical route disposition: legacy")
            && !receipt_stderr.contains("canonical route disposition: legacy"),
        "MIR receipt inspection did not report a canonical owned-get route:\nstdout:\n{receipt_stdout}\nstderr:\n{receipt_stderr}"
    );
}

#[test]
fn explicit_mir_verifier_rejects_incomplete_typed_owned_get_coverage() {
    let mimi = mimi_bin();
    let dir = std::env::temp_dir().join(format!(
        "mimi-ordinary-map-owned-get-incomplete-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("cli")
    ));
    std::fs::create_dir_all(&dir).expect("create isolated incomplete owned-get directory");
    let source = dir.join("incomplete.mimi");
    std::fs::write(
        &source,
        r#"
            func main() -> i32 {
                let root = map_new()
                let result = map_get_i32(root, "answer")
                drop(root)
                match result {
                    Ok(value) => value,
                    Err(code) => code,
                }
            }
        "#,
    )
    .expect("write incomplete typed owned-get fixture");

    let verify = Command::new(&mimi)
        .arg("verify")
        .arg("--mir")
        .arg(&source)
        .output()
        .expect("verify incomplete owned-get fixture through explicit MIR");
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !verify.status.success(),
        "explicit MIR must reject an incomplete typed owned-get receipt: {transcript}"
    );
    assert!(
        transcript.contains("MIR-COVERAGE-001")
            && transcript.contains(
                "typed owned-get Map candidate without complete operation-attempt coverage"
            ),
        "explicit MIR should report the Checker coverage boundary: {transcript}"
    );
}
