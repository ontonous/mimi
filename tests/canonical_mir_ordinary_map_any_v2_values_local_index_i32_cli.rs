use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const STALE_FIXTURE: &str = "mir_ordinary_map_any_owner_v2_values_local_index_i32.mimi";
const STALE_FIXTURE_SHA256: &str =
    "1ac48995706de2790a73148e449faf6495fdbe9dca301d9fc1fe3d57e8510bfa";
const RESULT_BRANCH_FIXTURE: &str =
    "mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi";
const RESULT_BRANCH_PROFILE: &str = "map-any-owner-v2-values-local-index-i32-result-branch-v2";
const RESULT_BRANCH_FIXTURE_SHA256: &str =
    "3083314a0fd518bd8365e61522e46f1a77b6e2dd78d0e9dc086bd745d8009baa";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide mimi for ValuesLocalIndexI32 CLI coverage")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(STALE_FIXTURE)
}

fn result_branch_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(RESULT_BRANCH_FIXTURE)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg("--")
        .arg(path)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "hash ValuesLocalIndexI32 fixture {}: {error}",
                path.display()
            )
        });
    assert!(
        output.status.success(),
        "sha256sum failed for ValuesLocalIndexI32 fixture {}: {}",
        path.display(),
        text(&output.stderr)
    );
    text(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum must print a fixture digest")
        .to_owned()
}

fn route_identity_for_profile(
    output: &Output,
    consumer: &str,
    mode: &str,
    expected_status: i32,
    expected_profile: &str,
    label: &str,
) -> (String, String, String) {
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{label} exit status; stderr:\n{}",
        text(&output.stderr)
    );
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{label} must report exactly one route-admission record:\n{stderr}"
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
        Some(&expected_profile),
        "{label} profile"
    );
    for key in ["mir_digest", "receipt_identity_digest"] {
        let digest = fields
            .get(key)
            .unwrap_or_else(|| panic!("{label} omitted {key}"));
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{label} has malformed {key}: {digest}"
        );
    }
    assert_no_legacy_or_canonical_fallback_for_profile(&stderr, expected_profile, label);
    (
        fields["checker_profile"].to_string(),
        fields["mir_digest"].to_string(),
        fields["receipt_identity_digest"].to_string(),
    )
}

fn assert_no_legacy_or_canonical_fallback_for_profile(
    stderr: &str,
    expected_profile: &str,
    label: &str,
) {
    assert!(
        !stderr.to_ascii_lowercase().contains("legacy"),
        "{label} selected a legacy fallback:\n{stderr}"
    );
    assert!(
        stderr
            .lines()
            .filter(|line| line.starts_with("canonical route disposition:"))
            .all(|line| line.contains(&format!("({expected_profile})"))),
        "{label} selected a different canonical profile:\n{stderr}"
    );
    assert!(
        !stderr.to_ascii_lowercase().contains("verifier-only"),
        "{label} selected a verifier-only fallback:\n{stderr}"
    );
}

fn assert_native_build_receipt(
    output: &Output,
    expected: &(String, String, String),
    consumer: &str,
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
        "{label} must report one native build receipt:\n{stderr}"
    );
    let fields = lines[0]
        .split_whitespace()
        .skip(2)
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{label} has malformed native field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&consumer), "{label} consumer");
    assert_eq!(
        fields.get("route_id"),
        Some(&expected.0.as_str()),
        "{label} route id"
    );
    assert_eq!(
        fields.get("profile"),
        Some(&expected.0.as_str()),
        "{label} profile"
    );
    assert_eq!(
        fields.get("mir_digest"),
        Some(&expected.1.as_str()),
        "{label} MIR digest"
    );
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&expected.2.as_str()),
        "{label} route identity digest"
    );
}

fn assert_verifier_receipt(output: &Output, expected: &(String, String, String), label: &str) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("canonical MIR verifier provenance: "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{label} must report the consumed MIR verifier receipt:\n{stderr}"
    );
    let fields = lines[0]
        .strip_prefix("canonical MIR verifier provenance: ")
        .expect("verifier provenance prefix")
        .split_whitespace()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{label} has malformed verifier field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("profile"),
        Some(&expected.0.as_str()),
        "{label} profile"
    );
    assert_eq!(
        fields.get("mir_digest"),
        Some(&expected.1.as_str()),
        "{label} MIR digest"
    );
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&expected.2.as_str()),
        "{label} route identity digest"
    );
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(label: &str) -> Self {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mimi-values-local-index-i32-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated ValuesLocalIndexI32 directory");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn values_local_index_i32_result_branch_keeps_its_own_default_and_explicit_route() {
    let source = result_branch_fixture();
    assert_eq!(
        sha256_file(&source),
        RESULT_BRANCH_FIXTURE_SHA256,
        "the nested Result-branch route fixture changed"
    );
    let mimi = mimi_bin();
    let directory = TemporaryDirectory::new("result-branch");
    let mut shared_identity = None;

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let output = command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("Result-branch {mode} run: {error}"));
        assert!(
            text(&output.stdout).is_empty(),
            "Result-branch {mode} stdout"
        );
        let identity = route_identity_for_profile(
            &output,
            "run",
            mode,
            41,
            RESULT_BRANCH_PROFILE,
            &format!("Result-branch {mode} run"),
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(
                &identity, expected,
                "{mode} changed the branch route identity"
            );
        } else {
            shared_identity = Some(identity);
        }
    }

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let binary = directory.0.join(mode);
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("Result-branch {mode} build: {error}"));
        assert!(
            output.status.success(),
            "Result-branch {mode} build failed:\nstdout:\n{}\nstderr:\n{}",
            text(&output.stdout),
            text(&output.stderr)
        );
        assert!(binary.is_file(), "Result-branch {mode} emitted no binary");
        let identity = route_identity_for_profile(
            &output,
            "build",
            mode,
            0,
            RESULT_BRANCH_PROFILE,
            &format!("Result-branch {mode} build"),
        );
        assert_native_build_receipt(
            &output,
            &identity,
            mode,
            &format!("Result-branch {mode} build"),
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(
                &identity, expected,
                "{mode} changed the branch route identity"
            );
        }
        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("Result-branch {mode} artifact: {error}"));
        assert_eq!(native.status.code(), Some(41), "Result-branch {mode} exit");
        assert!(
            text(&native.stdout).is_empty(),
            "Result-branch {mode} output"
        );
    }

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let output = command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("Result-branch {mode} verify: {error}"));
        assert!(
            output.status.success(),
            "Result-branch {mode} verify failed:\nstdout:\n{}\nstderr:\n{}",
            text(&output.stdout),
            text(&output.stderr)
        );
        assert!(
            text(&output.stdout).contains("No contracts to verify in"),
            "Result-branch {mode} must report no contracts: {}",
            text(&output.stdout)
        );
        let identity = route_identity_for_profile(
            &output,
            "verify",
            mode,
            0,
            RESULT_BRANCH_PROFILE,
            &format!("Result-branch {mode} verify"),
        );
        assert_verifier_receipt(&output, &identity, &format!("Result-branch {mode} verify"));
        if let Some(expected) = &shared_identity {
            assert_eq!(
                &identity, expected,
                "{mode} changed the branch route identity"
            );
        }
    }

    let original = fs::read_to_string(&source).expect("read branch fixture for L2 contracts");
    for (case, ensures) in [
        (
            "complete-result-domain",
            "result == 41 || result == 2 || result == 3",
        ),
        ("omit-set-err-2", "result == 41 || result == 3"),
        ("omit-new-err-3", "result == 41 || result == 2"),
    ] {
        let source_text = replace_once(
            &original,
            "func main() -> i32 {\n",
            &format!("func main() -> i32 {{\n    ensures: {ensures}\n"),
            &format!("Result-branch {case} contract"),
        );
        let contract_source = directory.0.join(format!("{case}.mimi"));
        fs::write(&contract_source, source_text).expect("write branch contract source");
        let mut identities = Vec::new();
        for direct in [false, true] {
            let mode = if direct { "explicit-mir" } else { "default" };
            let mut command = Command::new(&mimi);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .env("MIMI_VERBOSE", "1")
                .arg("verify")
                .arg("--stats");
            if direct {
                command.arg("--mir");
            }
            let output = command
                .arg(&contract_source)
                .output()
                .unwrap_or_else(|error| panic!("Result-branch {case} {mode}: {error}"));
            let transcript = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
            assert_eq!(
                output.status.code(),
                Some(1),
                "Result-branch {case} {mode} must fail L2:\n{transcript}"
            );
            assert!(
                transcript.contains("canonical MIR body can reach trap 'E0800' under requires"),
                "Result-branch {case} {mode} must preserve reachable E0800:\n{transcript}"
            );
            assert!(
                !transcript.contains("No contracts to verify")
                    && !transcript.contains("canonical MIR ensures contract proven"),
                "Result-branch {case} {mode} lost or waived the Ensures obligation:\n{transcript}"
            );
            assert!(
                text(&output.stdout).contains("0/1 verified"),
                "Result-branch {case} {mode} must report the failed proof:\n{transcript}"
            );
            let identity = route_identity_for_profile(
                &output,
                "verify",
                mode,
                1,
                RESULT_BRANCH_PROFILE,
                &format!("Result-branch {case} {mode} verify"),
            );
            assert_verifier_receipt(
                &output,
                &identity,
                &format!("Result-branch {case} {mode} verify"),
            );
            identities.push(identity);
        }
        assert_eq!(
            identities[0], identities[1],
            "Result-branch {case} default and explicit verification must share identity"
        );
    }

    let near_miss = replace_once(
        &original,
        "let index: i32 = 0",
        "let index: i32 = 1",
        "Result-branch nonzero index",
    );
    let rejected = directory.0.join("nonzero-index.mimi");
    fs::write(&rejected, near_miss).expect("write nonzero-index branch near-miss");
    assert_near_miss_rejected(
        &mimi,
        &rejected,
        &directory.0,
        "result-branch-nonzero-index",
    );
}

#[test]
fn stale_straightline_values_fixture_is_a_source_diagnostic_on_both_routes() {
    let mimi = mimi_bin();
    let source = fixture();
    assert_eq!(
        sha256_file(&source),
        STALE_FIXTURE_SHA256,
        "the retired straightline fixture changed"
    );
    let directory = TemporaryDirectory::new("stale-straightline-source-diagnostic");

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let output = command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("stale fixture {mode} run: {error}"));
        assert_stale_map_result_diagnostic(&output, &format!("stale fixture {mode} run"));
    }

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let binary = directory.0.join(format!("stale-{mode}"));
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("stale fixture {mode} build: {error}"));
        assert_stale_map_result_diagnostic(&output, &format!("stale fixture {mode} build"));
        assert!(
            !binary.exists(),
            "stale fixture {mode} build left a native artifact"
        );
    }

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let output = command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("stale fixture {mode} verify: {error}"));
        assert_stale_map_result_diagnostic(&output, &format!("stale fixture {mode} verify"));
    }
}

fn assert_stale_map_result_diagnostic(output: &Output, label: &str) {
    assert!(
        !output.status.success(),
        "{label} must reject the bare Result source:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains("E0242"),
        "{label} must preserve the source-level Map argument diagnostic:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("MIMI_ROUTE_CENSUS route-admission ")
            && !diagnostic.contains("canonical route disposition: canonical")
            && !diagnostic.contains("canonical route disposition: legacy"),
        "{label} must stop at source checking without claiming either backend route:\n{diagnostic}"
    );
}

fn replace_once(source: &str, needle: &str, replacement: &str, label: &str) -> String {
    let occurrences = source.matches(needle).count();
    assert_eq!(occurrences, 1, "{label} source anchor count");
    source.replacen(needle, replacement, 1)
}

fn assert_near_miss_rejected(mimi: &Path, source: &Path, directory: &Path, label: &str) {
    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let output = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{label} {mode} run: {error}"));
        assert_fail_closed(&output, &format!("{label} {mode} run"));
    }

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let binary = directory.join(format!("{label}-{mode}"));
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("build")
            .arg(source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("{label} {mode} build: {error}"));
        assert_fail_closed(&output, &format!("{label} {mode} build"));
        assert!(
            !binary.exists(),
            "{label} {mode} build left a binary after rejecting the route"
        );
    }

    for direct in [false, true] {
        let mode = if direct { "explicit-mir" } else { "default" };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let output = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{label} {mode} verify: {error}"));
        assert_fail_closed(&output, &format!("{label} {mode} verify"));
    }
}

fn assert_fail_closed(output: &Output, label: &str) {
    assert!(
        !output.status.success(),
        "{label} unexpectedly accepted a ValuesLocalIndexI32 near-miss:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains("MIR-COVERAGE-001")
            || (diagnostic.contains("MIR-MATERIALIZATION-001")
                && diagnostic.contains("Canonical MIR route rejected:")
                && diagnostic.contains("MIR lowering failed"))
            || (diagnostic.contains("canonical MIR build error: MIR lowering failed")
                && diagnostic.contains("index projection has no canonical ownership receipt"))
            || (diagnostic.contains("E0200")
                && diagnostic.contains(
                    "direct List<Any> indexed cast requires one exact Values unpack receipt",
                ))
            || (diagnostic.contains("typecheck failed before verify")
                && diagnostic.contains("error[E0200]"))
            || diagnostic.contains("type checking failed")
            || diagnostic.contains("E0256")
            || diagnostic.contains("E0304"),
        "{label} failed without an expected closed-route or ownership diagnostic:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: canonical"),
        "{label} announced a canonical route before rejecting:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: legacy"),
        "{label} fell back to the compatibility route:\n{diagnostic}"
    );
}

#[test]
fn values_local_index_i32_near_misses_reject_all_default_and_explicit_consumers() {
    const {
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let mimi = mimi_bin();
    let branch_source = result_branch_fixture();
    assert_eq!(
        sha256_file(&branch_source),
        RESULT_BRANCH_FIXTURE_SHA256,
        "near-miss cases must derive from the pinned Result-branch source"
    );
    let original = fs::read_to_string(&branch_source)
        .expect("read the Result-branch ValuesLocalIndexI32 fixture");
    let directory = TemporaryDirectory::new("negative");
    let cases = [
        (
            "nonzero-index",
            replace_once(
                &original,
                "let index: i32 = 0",
                "let index: i32 = 1",
                "nonzero",
            ),
        ),
        (
            "mutable-index",
            replace_once(
                &original,
                "let index: i32 = 0",
                "let mut index: i32 = 0",
                "mutable",
            ),
        ),
        (
            "reassigned-index",
            replace_once(
                &original,
                "let index: i32 = 0",
                "let mut index: i32 = 0\n    index = 0",
                "reassigned",
            ),
        ),
        (
            "computed-initializer",
            replace_once(
                &original,
                "let index: i32 = 0",
                "let index: i32 = 0 + 0",
                "computed initializer",
            ),
        ),
        (
            "computed-projection",
            replace_once(
                &original,
                "snapshot[index]",
                "snapshot[index + 0]",
                "computed projection",
            ),
        ),
        (
            "extra-index",
            replace_once(
                &original,
                "    let item = snapshot[index] as i32\n",
                "    let item = snapshot[index] as i32\n    let extra = snapshot[index] as i32\n",
                "extra unpack",
            ),
        ),
        (
            "unsettled-snapshot",
            replace_once(&original, "    drop(snapshot)\n", "", "unsettled snapshot"),
        ),
        (
            "unsettled-root",
            replace_once(&original, "    drop(root)\n", "", "unsettled root"),
        ),
    ];

    for (label, source_text) in cases {
        let source = directory.0.join(format!("{label}.mimi"));
        fs::write(&source, source_text).expect("write exact near-miss source");
        assert_near_miss_rejected(&mimi, &source, &directory.0, label);
    }
}
