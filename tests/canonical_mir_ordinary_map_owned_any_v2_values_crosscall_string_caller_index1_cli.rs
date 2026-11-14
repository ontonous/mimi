use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "ordinary-map-owned-any-v2-values-crosscall-string-caller-v2";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const NATIVE_BUILD_PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";
const VERIFIER_PROVENANCE_PREFIX: &str = "canonical MIR verifier provenance: ";
const EXPECTED_RUN_STDOUT: &[u8] = b"true\n";

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mimi-values-crosscall-string-caller-index1-cli-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated Values cross-call index-one CLI directory {}: {error}",
                path.display()
            )
        });
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn cli(mode: &str, explicit_mir: bool, source: &Path, output: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mimi"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("MIMI_") {
            command.env_remove(key);
        }
    }
    command
        .arg(mode)
        .arg(source)
        .env("MIMI_ROUTE_CENSUS", "1")
        .env("MIMI_VERBOSE", "1")
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if explicit_mir {
        command.arg("--mir");
    }
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("invoke mimi {mode}: {error}"))
}

fn assert_success(output: &Output, label: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} must succeed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn route_admission(output: &Output, consumer: &str, explicit_mir: bool, label: &str) -> String {
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} must not fall back to the legacy route:\n{stderr}"
    );
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with(ROUTE_ADMISSION_PREFIX))
        .collect::<Vec<_>>();
    let [line] = lines.as_slice() else {
        panic!(
            "{label} must emit one canonical route admission; got {}:\n{stderr}",
            lines.len()
        );
    };
    let route = line
        .strip_prefix(ROUTE_ADMISSION_PREFIX)
        .expect("admission prefix");
    let expected_consumer = format!("consumer={consumer}");
    assert!(
        route
            .split_whitespace()
            .any(|field| field == expected_consumer.as_str()),
        "{label} route admission must identify {consumer}: {route}"
    );
    let expected_mode = format!(
        "mode={}",
        if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }
    );
    assert!(
        route
            .split_whitespace()
            .any(|field| field == expected_mode.as_str()),
        "{label} route admission must identify the request mode: {route}"
    );
    let expected_profile = format!("checker_profile={ROUTE_PROFILE}");
    assert!(
        route
            .split_whitespace()
            .any(|field| field == expected_profile.as_str()),
        "{label} must use the exact V2 index-one checker profile: {route}"
    );
    let identity = route
        .split_whitespace()
        .find_map(|field| field.strip_prefix("receipt_identity_digest="))
        .unwrap_or_else(|| panic!("{label} omitted route receipt identity: {route}"));
    assert!(
        identity.len() == 64 && identity.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} receipt identity must be a SHA-256 digest: {identity}"
    );
    if consumer == "build" {
        assert!(
            stderr.lines().any(|native| {
                native.starts_with(NATIVE_BUILD_PREFIX)
                    && native.contains(&format!("route_id={ROUTE_PROFILE}"))
                    && native.contains(&format!("profile={ROUTE_PROFILE}"))
                    && native.contains(&format!("route_identity_digest={identity}"))
            }),
            "{label} native adapter must consume the same V2 route identity:\n{stderr}"
        );
    }
    if consumer == "verify" {
        let provenance = stderr
            .lines()
            .find(|line| line.starts_with(VERIFIER_PROVENANCE_PREFIX))
            .unwrap_or_else(|| panic!("{label} omitted verifier provenance:\n{stderr}"));
        assert!(
            provenance.contains(&format!("profile={ROUTE_PROFILE}")),
            "{label} verifier must use the exact V2 route profile: {provenance}"
        );
        let mir_digest = route
            .split_whitespace()
            .find_map(|field| field.strip_prefix("mir_digest="))
            .expect("route admission MIR digest");
        assert!(
            provenance.contains(&format!("mir_digest={mir_digest}")),
            "{label} verifier must use the same canonical MIR graph: {provenance}"
        );
    }
    identity.to_owned()
}

fn assert_near_miss_rejected(output: &Output, label: &str) {
    assert_ne!(
        output.status.code(),
        Some(0),
        "{label} must be rejected:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let stderr = text(&output.stderr);
    assert!(
        ["E0218", "E0256", "E0304", "MIR-COVERAGE-001"]
            .iter()
            .any(|code| stderr.contains(code)),
        "{label} should fail at the Checker ownership or canonical coverage gate:\n{stderr}"
    );
    assert!(
        !stderr.contains(ROUTE_ADMISSION_PREFIX)
            && !stderr.contains(NATIVE_BUILD_PREFIX)
            && !stderr.contains("canonical route disposition: legacy"),
        "{label} must fail closed without V1/V2 route admission or legacy fallback:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{label} must not execute user code"
    );
}

#[test]
fn values_crosscall_string_caller_index1_uses_its_exact_cli_route() {
    let source = fixture();
    let checker = cli("check", false, &source, None);
    assert_success(&checker, "Checker-only check");
    assert!(
        text(&checker.stdout).contains("checked successfully"),
        "check must report successful type and ownership checking: {}",
        text(&checker.stdout)
    );
    assert!(
        !text(&checker.stderr).contains(ROUTE_ADMISSION_PREFIX),
        "check is a Checker-only command and does not claim a MIR route identity"
    );

    let temporary = TemporaryDirectory::new();
    let mut identities = Vec::new();
    for (consumer, explicit_mir) in [
        ("build", false),
        ("build", true),
        ("run", false),
        ("run", true),
        ("verify", false),
        ("verify", true),
    ] {
        let mode = if explicit_mir {
            "explicit --mir"
        } else {
            "default"
        };
        let label = format!("{consumer} {mode}");
        let native = (consumer == "build").then(|| {
            temporary
                .path()
                .join(format!("native-{}", mode.replace(' ', "-")))
        });
        let output = cli(consumer, explicit_mir, &source, native.as_deref());
        assert_success(&output, &label);
        identities.push(route_admission(&output, consumer, explicit_mir, &label));

        if consumer == "run" {
            assert_eq!(
                output.stdout, EXPECTED_RUN_STDOUT,
                "{label} must print the expected selected String comparison"
            );
        } else if consumer == "build" {
            let binary = native.expect("build output path");
            assert!(binary.is_file(), "{label} must create a native executable");
            let native_run = Command::new(&binary)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap_or_else(|error| panic!("execute {label} native artifact: {error}"));
            assert_eq!(
                native_run.status.code(),
                Some(0),
                "{label} native artifact must execute:\nstdout:\n{}\nstderr:\n{}",
                text(&native_run.stdout),
                text(&native_run.stderr)
            );
            assert_eq!(
                native_run.stdout, EXPECTED_RUN_STDOUT,
                "{label} native output must match VM output"
            );
            assert!(native_run.stderr.is_empty());
        } else {
            assert_eq!(
                text(&output.stdout),
                format!("No contracts to verify in {}\n", source.display()),
                "{label} must complete through the dedicated V2 verifier route"
            );
        }
    }
    assert!(
        identities.windows(2).all(|pair| pair[0] == pair[1]),
        "default and explicit build/run routes must share one V2 MIR receipt identity: {identities:?}"
    );
}

#[test]
fn values_crosscall_string_caller_index1_near_misses_fail_closed() {
    let positive = fs::read_to_string(fixture()).expect("read V2 index-one fixture");
    let cases = [
        (
            "zero-index",
            positive.replacen("snapshot[1]", "snapshot[0]", 1),
        ),
        (
            "dynamic-index",
            positive.replacen(
                "    let selected = snapshot[1] as string",
                "    let index = 1\n    let selected = snapshot[index] as string",
                1,
            ),
        ),
        (
            "missing-selected-drop",
            positive.replacen("    drop(selected)\n", "", 1),
        ),
    ];
    let temporary = TemporaryDirectory::new();
    for (case, source_text) in cases {
        assert_ne!(
            source_text, positive,
            "{case} must modify the positive source"
        );
        let source = temporary.path().join(format!("{case}.mimi"));
        fs::write(&source, source_text).unwrap_or_else(|error| {
            panic!(
                "write {case} near-miss source {}: {error}",
                source.display()
            )
        });
        assert_near_miss_rejected(
            &cli("check", false, &source, None),
            &format!("{case} check"),
        );
        assert_near_miss_rejected(
            &cli("run", false, &source, None),
            &format!("{case} default run"),
        );
        assert_near_miss_rejected(
            &cli(
                "build",
                true,
                &source,
                Some(&temporary.path().join(format!("{case}-out"))),
            ),
            &format!("{case} explicit MIR build"),
        );
    }
}
