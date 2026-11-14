use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const PROFILE: &str = "ordinary-map-owned-get-i32-string-v1";
const EXPECTED_STDOUT: &[u8] = b"snapshot survives\n";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owned_get_string_detached_result_match.mimi")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn route_identity(
    output: &Output,
    consumer: &str,
    mode: &str,
    label: &str,
) -> (String, String, String) {
    assert_success(output, label);
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} fell back to Legacy:\n{stderr}"
    );
    if mode == "default" {
        assert!(
            stderr.contains(&format!(
                "canonical route disposition: canonical ({PROFILE})"
            )),
            "{label} did not report the exact default typed owned-get V1 route:\n{stderr}"
        );
    } else if consumer == "run" {
        assert!(
            stderr.contains(&format!(
                "canonical route disposition: explicit canonical ({PROFILE})"
            )),
            "{label} did not report the exact explicit typed owned-get V1 route:\n{stderr}"
        );
    } else if stderr.contains("canonical route disposition:") {
        assert!(
            stderr.contains(&format!("canonical ({PROFILE})"))
                || stderr.contains(&format!("explicit canonical ({PROFILE})")),
            "{label} reported a different canonical route:\n{stderr}"
        );
    }
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{label} must report one route-admission line: {stderr}"
    );
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route-admission prefix")
        .split_whitespace()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{label} has malformed route field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&consumer), "{label} consumer");
    assert_eq!(fields.get("mode"), Some(&mode), "{label} mode");
    assert_eq!(
        fields.get("checker_profile"),
        Some(&PROFILE),
        "{label} Checker profile"
    );
    let digest = fields.get("mir_digest").expect("MIR digest");
    let receipt = fields
        .get("receipt_identity_digest")
        .expect("route receipt digest");
    for (name, value) in [
        ("mir_digest", *digest),
        ("receipt_identity_digest", *receipt),
    ] {
        assert!(
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{label} has invalid {name}: {value}"
        );
    }
    (PROFILE.into(), (*digest).into(), (*receipt).into())
}

fn command_for_mode(mimi: &Path, subcommand: &str, explicit_mir: bool) -> Command {
    let mut command = Command::new(mimi);
    command.env("MIMI_ROUTE_CENSUS", "1").arg(subcommand);
    if explicit_mir {
        command.arg("--mir");
    }
    command
}

fn assert_native_build_receipt(
    output: &Output,
    identity: &(String, String, String),
    mode: &str,
    label: &str,
) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{label} must report one native route receipt: {stderr}"
    );
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS canonical-native-build ")
        .expect("native build receipt prefix")
        .split_whitespace()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{label} has malformed native receipt field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&mode), "{label} consumer");
    assert_eq!(fields.get("route_id"), Some(&PROFILE), "{label} route id");
    assert_eq!(fields.get("profile"), Some(&PROFILE), "{label} profile");
    assert_eq!(
        fields.get("mir_digest"),
        Some(&identity.1.as_str()),
        "{label} MIR digest"
    );
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&identity.2.as_str()),
        "{label} route identity digest"
    );
}

fn temp_dir(label: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "mimi-owned-get-string-match-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir(&directory).expect("create isolated test directory");
    directory
}

#[test]
fn detached_string_result_match_uses_exact_default_and_explicit_mir_routes() {
    const {
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let mimi = mimi_bin();
    let source = fixture();
    let directory = temp_dir("routes");
    let mut identities = Vec::new();

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let mut mode_identities = Vec::new();

        let run = command_for_mode(&mimi, "run", explicit_mir)
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("start {mode} run: {error}"));
        assert_eq!(run.stdout, EXPECTED_STDOUT, "{mode} run stdout");
        mode_identities.push(route_identity(&run, "run", mode, &format!("{mode} run")));

        let binary = directory.join(format!("{mode}.native"));
        let build = command_for_mode(&mimi, "build", explicit_mir)
            .arg(&source)
            .arg("--output")
            .arg(&binary)
            .output()
            .unwrap_or_else(|error| panic!("start {mode} build: {error}"));
        let build_identity = route_identity(&build, "build", mode, &format!("{mode} build"));
        assert_native_build_receipt(&build, &build_identity, mode, &format!("{mode} build"));
        mode_identities.push(build_identity);
        assert!(
            binary.is_file(),
            "{mode} build produced no native executable"
        );
        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("execute {mode} native executable: {error}"));
        assert_success(&native, &format!("{mode} native execution"));
        assert_eq!(native.stdout, EXPECTED_STDOUT, "{mode} native stdout");
        assert!(
            native.stderr.is_empty(),
            "{mode} native stderr: {}",
            text(&native.stderr)
        );

        let verify = command_for_mode(&mimi, "verify", explicit_mir)
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("start {mode} verify: {error}"));
        assert!(
            text(&verify.stdout).contains("No contracts to verify"),
            "{mode} verifier output changed: {}",
            text(&verify.stdout)
        );
        assert!(
            !text(&verify.stdout).contains("Proven")
                && !text(&verify.stderr).contains("canonical MIR verifier provenance:"),
            "{mode} no-contract verifier fabricated a proof result or solver provenance:\nstdout:\n{}\nstderr:\n{}",
            text(&verify.stdout),
            text(&verify.stderr)
        );
        let verify_identity = route_identity(&verify, "verify", mode, &format!("{mode} verify"));
        mode_identities.push(verify_identity);

        assert_eq!(mode_identities.len(), 3);
        assert!(
            mode_identities
                .iter()
                .all(|identity| identity == &mode_identities[0]),
            "{mode} run/build/verify consumed different route identities: {mode_identities:?}"
        );
        identities.push(mode_identities[0].clone());
    }

    assert_eq!(
        identities[0], identities[1],
        "default and explicit MIR route identities differ"
    );
    fs::remove_dir_all(directory).expect("remove isolated test directory");
}

#[test]
fn source_near_misses_fail_closed_without_a_legacy_route() {
    let mimi = mimi_bin();
    let source_text = fs::read_to_string(fixture()).expect("read positive fixture");
    let near_misses = [
        (
            "wildcard-error-payload",
            source_text.replace("Err(status) => status", "Err(_) => 1"),
        ),
        (
            "aliased-result-local",
            source_text
                .replace(
                    "let snapshot = map_get_string(source_copy, \"payload\")",
                    "let snapshot = map_get_string(source_copy, \"payload\")\n    let alias = snapshot",
                )
                .replace("match snapshot {", "match alias {"),
        ),
        (
            "missing-ok-string-drop",
            source_text.replace("            drop(text)\n", ""),
        ),
    ];
    let directory = temp_dir("near-misses");
    for (label, source) in near_misses {
        let source_path = directory.join(format!("{label}.mimi"));
        fs::write(&source_path, source).expect("write near-miss fixture");
        for explicit_mir in [false, true] {
            let mode = if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            };
            let output = command_for_mode(&mimi, "run", explicit_mir)
                .arg(&source_path)
                .output()
                .unwrap_or_else(|error| panic!("start {label} {mode} run: {error}"));
            let transcript = format!("{}{}", text(&output.stdout), text(&output.stderr));
            assert!(
                !output.status.success(),
                "{label} {mode} unexpectedly ran:\n{transcript}"
            );
            assert!(
                !transcript.contains("canonical route disposition: legacy"),
                "{label} {mode} fell back to Legacy:\n{transcript}"
            );
            let route_rejection = transcript.contains("MIR-COVERAGE-001")
                || transcript.contains("MIR-MATERIALIZATION-001")
                || transcript.contains("MIR-RECEIPT-001");
            let checker_rejection =
                label == "missing-ok-string-drop" && transcript.contains("error[E");
            assert!(
                route_rejection || checker_rejection,
                "{label} {mode} did not fail closed in Checker or at a canonical route boundary:\n{transcript}"
            );
        }
    }
    fs::remove_dir_all(directory).expect("remove isolated test directory");
}
