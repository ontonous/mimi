use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        .expect("Cargo must provide the mimi binary for SetResult CLI evidence")
}

fn set_result_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owned_any_v2_set_result_lineage.mimi")
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

struct TempSource(PathBuf);

impl TempSource {
    fn new(label: &str, source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mimi-{label}-{}-{}.mimi",
            std::process::id(),
            std::thread::current().name().unwrap_or("cli")
        ));
        std::fs::write(&path, source).expect("write temporary SetResult near miss");
        Self(path)
    }
}

impl Drop for TempSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RouteIdentity {
    profile: String,
    mir_digest: String,
    receipt_identity_digest: String,
}

fn census_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(key))
}

fn assert_set_result_route(stderr: &str, mode: &str, profile: &str) -> RouteIdentity {
    assert!(
        stderr.contains(profile),
        "{mode} did not report the SetResult route: {stderr}"
    );
    let line = stderr
        .lines()
        .find(|line| {
            line.contains(profile)
                && line.contains("mir_digest=")
                && line.contains("receipt_identity_digest=")
        })
        .unwrap_or_else(|| panic!("{mode} did not report a SetResult MIR receipt: {stderr}"));
    let mir_digest = census_value(line, "mir_digest=")
        .filter(|digest| !digest.is_empty())
        .unwrap_or_else(|| panic!("{mode} did not report a SetResult MIR digest: {stderr}"));
    let receipt_identity_digest = census_value(line, "receipt_identity_digest=")
        .filter(|digest| !digest.is_empty())
        .unwrap_or_else(|| {
            panic!("{mode} did not report a SetResult receipt identity digest: {stderr}")
        });
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} fell back to a legacy route: {stderr}"
    );
    RouteIdentity {
        profile: profile.to_owned(),
        mir_digest: mir_digest.to_owned(),
        receipt_identity_digest: receipt_identity_digest.to_owned(),
    }
}

fn run_command(mimi: &Path, explicit_mir: bool, source: &Path) -> Output {
    let mut command = Command::new(mimi);
    command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg(source)
        .output()
        .expect("run the ordinary Map SetResult fixture")
}

fn verify_command(mimi: &Path, explicit_mir: bool, source: &Path) -> Output {
    let mut command = Command::new(mimi);
    command.env("MIMI_ROUTE_CENSUS", "1").arg("verify");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg(source)
        .output()
        .expect("verify the ordinary Map SetResult fixture")
}

fn build_command(mimi: &Path, explicit_mir: bool, source: &Path, output: &Path) -> Output {
    let mut command = Command::new(mimi);
    command
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("build")
        .arg(source);
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg("--output")
        .arg(output)
        .output()
        .expect("build the ordinary Map SetResult fixture")
}

#[test]
fn ordinary_map_set_result_uses_the_same_default_and_explicit_cli_route() {
    let mimi = mimi_bin();
    let source = set_result_fixture();
    let mut run_route_digests = Vec::new();
    let mut build_route_digests = Vec::new();
    let mut verify_route_digests = Vec::new();

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit --mir"
        } else {
            "default"
        };

        let run = run_command(&mimi, explicit_mir, &source);
        let run_stderr = String::from_utf8_lossy(&run.stderr);
        assert_eq!(
            run.status.code(),
            Some(1),
            "{mode} mimi run should return the fixture's map size (1), not a route error:\nstdout:\n{}\nstderr:\n{run_stderr}",
            String::from_utf8_lossy(&run.stdout)
        );
        run_route_digests.push(assert_set_result_route(
            &run_stderr,
            &format!("{mode} run"),
            "ordinary-map-set-result-v1",
        ));

        let binary = TempBinary::new(&format!("ordinary-map-set-result-{mode}"));
        let build = build_command(&mimi, explicit_mir, &source, &binary.0);
        let build_stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "{mode} mimi build failed:\nstdout:\n{}\nstderr:\n{build_stderr}",
            String::from_utf8_lossy(&build.stdout)
        );
        build_route_digests.push(assert_set_result_route(
            &build_stderr,
            &format!("{mode} build"),
            "ordinary-map-set-result-v1",
        ));

        let native = Command::new(&binary.0)
            .output()
            .expect("run the native SetResult build");
        assert_eq!(
            native.status.code(),
            Some(1),
            "{mode} native binary should return the fixture's map size (1): {}",
            String::from_utf8_lossy(&native.stderr)
        );

        let verify = verify_command(&mimi, explicit_mir, &source);
        let verify_stdout = String::from_utf8_lossy(&verify.stdout);
        let verify_stderr = String::from_utf8_lossy(&verify.stderr);
        assert!(
            verify.status.success(),
            "{mode} mimi verify failed:\nstdout:\n{verify_stdout}\nstderr:\n{verify_stderr}"
        );
        assert!(
            verify_stdout.contains("Result status/tag semantics were not proven")
                || verify_stdout.contains("no proof obligations"),
            "the contract-free SetResult fixture must retain its explicit no-obligation disposition: {verify_stdout}"
        );
        assert!(
            !verify_stdout.contains("Proven") && !verify_stdout.contains("✓ pass"),
            "a contract-free SetResult route must not report a proof: {verify_stdout}"
        );
        verify_route_digests.push(assert_set_result_route(
            &verify_stderr,
            &format!("{mode} verify"),
            "ordinary-map-set-result-l2-verifier-v1",
        ));
    }

    for (consumer, digests) in [
        ("run", &run_route_digests),
        ("build", &build_route_digests),
        ("verify", &verify_route_digests),
    ] {
        assert_eq!(
            digests.len(),
            2,
            "both default and explicit --mir {consumer} must report the route digest"
        );
        assert_eq!(digests[0], digests[1], "default and explicit --mir {consumer} must bind the same SetResult profile, MIR digest, and receipt identity");
    }
}

#[test]
fn ordinary_map_set_result_near_misses_fail_closed_in_the_cli() {
    let mimi = mimi_bin();
    let exact =
        std::fs::read_to_string(set_result_fixture()).expect("read the exact SetResult fixture");
    let tampered_status = exact.replace(
        "                    status\n",
        "                    status + 1\n",
    );
    assert_ne!(
        tampered_status, exact,
        "status mutation must change the source"
    );
    let status_source = TempSource::new("set-result-status-tamper", &tampered_status);

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let binary = TempBinary::new(&format!("set-result-status-tamper-{mode}"));
        let outputs = [
            ("run", run_command(&mimi, explicit_mir, &status_source.0)),
            (
                "build",
                build_command(&mimi, explicit_mir, &status_source.0, &binary.0),
            ),
            (
                "verify",
                verify_command(&mimi, explicit_mir, &status_source.0),
            ),
        ];
        for (consumer, output) in outputs {
            let transcript = format!(
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !output.status.success(),
                "a SetResult status mutation must be rejected by {mode} {consumer}: {transcript}"
            );
            assert!(
                transcript.contains("MIR-COVERAGE-001"),
                "a status mutation must remain a surfaced canonical coverage failure in {mode} {consumer}: {transcript}"
            );
            assert!(
                !transcript.contains("canonical route disposition: legacy"),
                "a SetResult status mutation must not reach legacy dispatch in {mode} {consumer}: {transcript}"
            );
        }
    }

    let missing_root_drop = exact.replacen("                    drop(root)\n", "", 1);
    assert_ne!(
        missing_root_drop, exact,
        "drop mutation must change the source"
    );
    let drop_source = TempSource::new("set-result-missing-root-drop", &missing_root_drop);
    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let output = run_command(&mimi, explicit_mir, &drop_source.0);
        let transcript = format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.status.success(),
            "a missing linear-root Drop must be rejected before {mode} execution: {transcript}"
        );
        assert!(
            transcript.contains("E0256")
                || transcript.contains("E0304")
                || transcript.contains("MIR-COVERAGE-001"),
            "the missing root settlement must fail through linear checking or canonical coverage: {transcript}"
        );
        assert!(
            !transcript.contains("canonical route disposition: legacy"),
            "a dropped-root near miss must not be reported as a legacy fallback in {mode}: {transcript}"
        );
    }
}
