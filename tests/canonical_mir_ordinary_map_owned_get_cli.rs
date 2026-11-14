use std::path::{Path, PathBuf};
use std::process::Command;

const OWNED_GET_DEFAULT_SHA256: &str =
    "6ab9d1d9a397914d0069a2b8ca39a9f51a470643a7aa90d9c7d07795cc8f482a";

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

fn assert_fixture_sha256(path: &Path, expected: &str) {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("hash owned-Get fixture {}: {error}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let output_text = String::from_utf8_lossy(&output.stdout);
    let digest = output_text
        .split_whitespace()
        .next()
        .expect("sha256sum output includes a digest");
    assert_eq!(digest, expected, "owned-Get fixture source changed");
}

struct TempBinary(PathBuf);

impl TempBinary {
    fn new(label: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "mimi-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("cli")
        )))
    }
}

impl Drop for TempBinary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn assert_new_result_route(stderr: &str, mode: &str) -> String {
    assert!(
        stderr.contains("ordinary-map-new-result-v1"),
        "{mode} did not report the NewResult route: {stderr}"
    );
    let digest = stderr
        .lines()
        .filter(|line| line.contains("ordinary-map-new-result-v1"))
        .find_map(|line| {
            line.split_once("mir_digest=")
                .map(|(_, digest)| digest.split_whitespace().next().unwrap_or_default())
        })
        .filter(|digest| !digest.is_empty())
        .unwrap_or_else(|| panic!("{mode} did not report a NewResult MIR digest: {stderr}"));
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} fell back to a legacy Map owner: {stderr}"
    );
    digest.to_owned()
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
    assert_fixture_sha256(&source, OWNED_GET_DEFAULT_SHA256);
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
                match map_new() {
                    Ok(root) => {
                        let result = map_get_i32(root, "answer")
                        drop(root)
                        match result {
                            Ok(value) => value,
                            Err(code) => code,
                        }
                    },
                    Err(status) => { status },
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

#[test]
fn ordinary_map_new_result_core_shape_uses_default_and_explicit_routes() {
    let mimi = mimi_bin();
    let source = fixture("mir_ordinary_map_new_result_core.mimi");

    let mut route_digests = Vec::new();
    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit --mir"
        } else {
            "default"
        };
        let mut run = Command::new(&mimi);
        run.env("MIMI_ROUTE_CENSUS", "1").arg("run");
        if explicit_mir {
            run.arg("--mir");
        }
        let run = run
            .arg(&source)
            .output()
            .expect("run ordinary Map NewResult fixture");
        let run_stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "{mode} mimi run failed:\nstdout:\n{}\nstderr:\n{run_stderr}",
            String::from_utf8_lossy(&run.stdout)
        );
        route_digests.push(assert_new_result_route(&run_stderr, &format!("{mode} run")));

        let binary = TempBinary::new(&format!("ordinary-map-new-result-{mode}"));
        let mut build = Command::new(&mimi);
        build
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(&source);
        if explicit_mir {
            build.arg("--mir");
        }
        let build = build
            .arg("--output")
            .arg(&binary.0)
            .output()
            .expect("build ordinary Map NewResult fixture");
        let build_stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "{mode} mimi build failed:\nstdout:\n{}\nstderr:\n{build_stderr}",
            String::from_utf8_lossy(&build.stdout)
        );
        route_digests.push(assert_new_result_route(
            &build_stderr,
            &format!("{mode} build"),
        ));
        let native = Command::new(&binary.0)
            .output()
            .expect("run built ordinary Map NewResult binary");
        assert!(
            native.status.success(),
            "{mode} native binary failed with {:?}: {}",
            native.status.code(),
            String::from_utf8_lossy(&native.stderr)
        );

        let mut verify = Command::new(&mimi);
        verify.env("MIMI_ROUTE_CENSUS", "1").arg("verify");
        if explicit_mir {
            verify.arg("--mir");
        }
        let verify = verify
            .arg(&source)
            .output()
            .expect("verify ordinary Map NewResult fixture");
        let verify_stdout = String::from_utf8_lossy(&verify.stdout);
        let verify_stderr = String::from_utf8_lossy(&verify.stderr);
        assert!(
            verify.status.success(),
            "{mode} mimi verify failed:\nstdout:\n{verify_stdout}\nstderr:\n{verify_stderr}"
        );
        assert!(
            verify_stdout.contains("No contracts to verify"),
            "the contract-free NewResult fixture should retain the empty verification disposition: {verify_stdout}"
        );
        assert!(
            verify_stderr.contains("checker_profile=ordinary-map-new-result-v1"),
            "{mode} verify did not report the NewResult route admission: {verify_stderr}"
        );
    }
    assert!(
        route_digests
            .iter()
            .all(|digest| digest == &route_digests[0]),
        "default/direct run/build routes must compile the same NewResult graph: {route_digests:?}"
    );
}
