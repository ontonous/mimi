use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROFILE: &str = "ordinary-map-owned-any-v2-direct-print-result-l2-verifier-v1";
const EXECUTION_PROFILE: &str = "map-any-owner-v2-direct-print-result-v1";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const ROUTE_DISPOSITION_PREFIX: &str = "canonical route disposition: canonical (";
const VERIFIER_PREFIX: &str = "canonical MIR verifier provenance: ";

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixtures() -> [PathBuf; 4] {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    [
        root.join("mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi"),
        root.join("mir_ordinary_map_any_owner_v2_direct_print_hit_string.mimi"),
        root.join("mir_ordinary_map_any_owner_v2_direct_print_miss_i32.mimi"),
        root.join("mir_ordinary_map_any_owner_v2_direct_print_same_root_map_size_i32.mimi"),
    ]
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_success(output: &Output, mode: &str) {
    assert!(
        output.status.success(),
        "{mode} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn parse_fields<'a>(line: &'a str, prefix: &str, mode: &str) -> BTreeMap<&'a str, &'a str> {
    let mut fields = BTreeMap::new();
    for token in line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{mode} record has an unexpected prefix: {line:?}"))
        .split_whitespace()
    {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{mode} has malformed field {token:?}"));
        assert!(
            fields.insert(key, value).is_none(),
            "{mode} repeats field {key}"
        );
    }
    fields
}

fn assert_sha256(value: &str, field: &str, mode: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{mode} has malformed {field} digest {value}"
    );
}

fn assert_no_legacy(stderr: &str, mode: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy")
            && !stderr.contains("compatibility fallback"),
        "{mode} selected or mentioned a legacy fallback: {stderr}"
    );
}

fn verify_identity(mimi: &Path, source: &Path, direct: bool) -> (String, String, String) {
    let mode = format!(
        "{} verify {}",
        source.file_name().unwrap().to_string_lossy(),
        if direct { "explicit --mir" } else { "default" }
    );
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
        .unwrap_or_else(|error| panic!("{mode}: {error}"));
    assert_success(&output, &mode);

    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("no proof obligations were present")
            && stdout.contains("Result tag/status and stdout semantics were not proven"),
        "{mode} must state a neutral no-obligation outcome and its proof boundary:\n{stdout}"
    );
    assert!(
        !stdout.contains("✓ pass") && !stdout.contains("Proven"),
        "{mode} must not present the no-obligation route as a proof:\n{stdout}"
    );

    let stderr = text(&output.stderr);
    assert_no_legacy(&stderr, &mode);
    let dispositions = stderr
        .lines()
        .filter(|line| line.starts_with(ROUTE_DISPOSITION_PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(
        dispositions.len(),
        1,
        "{mode} must pass the shared canonical route selector and its execution-consumer preflight: {stderr}"
    );
    assert!(
        dispositions[0].starts_with(&format!("{ROUTE_DISPOSITION_PREFIX}{PROFILE}) mir_digest=")),
        "{mode} selected another canonical verification profile: {stderr}"
    );
    let admissions = stderr
        .lines()
        .filter(|line| line.starts_with(ROUTE_ADMISSION_PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(
        admissions.len(),
        1,
        "{mode} must report one Checker-bound L2 route admission: {stderr}"
    );
    let admission = parse_fields(admissions[0], ROUTE_ADMISSION_PREFIX, &mode);
    assert_eq!(
        admission.keys().copied().collect::<Vec<_>>(),
        [
            "checker_profile",
            "consumer",
            "mir_digest",
            "mode",
            "receipt_identity_digest"
        ],
        "{mode} route-admission field set"
    );
    assert_eq!(
        admission.get("consumer"),
        Some(&"verify"),
        "{mode} consumer"
    );
    assert_eq!(
        admission.get("mode"),
        Some(&if direct { "explicit-mir" } else { "default" }),
        "{mode} route mode"
    );
    assert_eq!(
        admission.get("checker_profile"),
        Some(&PROFILE),
        "{mode} L2 profile"
    );

    let provenance = stderr
        .lines()
        .filter(|line| line.starts_with(VERIFIER_PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(
        provenance.len(),
        1,
        "{mode} must report the exact route receipt consumed by L2: {stderr}"
    );
    let receipt = parse_fields(provenance[0], VERIFIER_PREFIX, &mode);
    assert_eq!(
        receipt.get("profile"),
        Some(&PROFILE),
        "{mode} verifier profile"
    );

    let mir_digest = *admission.get("mir_digest").expect("MIR digest");
    let route_identity = *admission
        .get("receipt_identity_digest")
        .expect("route receipt identity");
    assert_eq!(
        receipt.get("mir_digest"),
        Some(&mir_digest),
        "{mode} MIR identity"
    );
    assert_eq!(
        receipt.get("route_identity_digest"),
        Some(&route_identity),
        "{mode} verifier receipt identity"
    );
    assert_sha256(mir_digest, "MIR", &mode);
    assert_sha256(route_identity, "route", &mode);
    (
        PROFILE.to_string(),
        mir_digest.to_string(),
        route_identity.to_string(),
    )
}

fn run_identity(
    mimi: &Path,
    source: &Path,
    direct: bool,
    expected_stdout: &str,
    expected_exit: i32,
) -> (String, String, String) {
    let mode = format!(
        "{} run {}",
        source.file_name().unwrap().to_string_lossy(),
        if direct { "explicit --mir" } else { "default" }
    );
    let mut command = Command::new(mimi);
    command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
    if direct {
        command.arg("--mir");
    }
    let output = command
        .arg(source)
        .output()
        .unwrap_or_else(|error| panic!("{mode}: {error}"));
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "{mode} returned an unexpected process status; stdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(text(&output.stdout), expected_stdout, "{mode} output");

    let stderr = text(&output.stderr);
    assert_no_legacy(&stderr, &mode);
    let disposition_prefix = if direct {
        "canonical route disposition: explicit canonical ("
    } else {
        "canonical route disposition: canonical ("
    };
    let dispositions = stderr
        .lines()
        .filter(|line| line.starts_with(disposition_prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        dispositions.len(),
        1,
        "{mode} must select the canonical Bytecode route exactly once: {stderr}"
    );
    assert!(
        dispositions[0].starts_with(&format!(
            "{disposition_prefix}{EXECUTION_PROFILE}) mir_digest="
        )),
        "{mode} selected another canonical execution profile: {stderr}"
    );

    let admissions = stderr
        .lines()
        .filter(|line| line.starts_with(ROUTE_ADMISSION_PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(
        admissions.len(),
        1,
        "{mode} must report one Checker-bound Bytecode route admission: {stderr}"
    );
    let admission = parse_fields(admissions[0], ROUTE_ADMISSION_PREFIX, &mode);
    assert_eq!(
        admission.keys().copied().collect::<Vec<_>>(),
        [
            "checker_profile",
            "consumer",
            "mir_digest",
            "mode",
            "receipt_identity_digest"
        ],
        "{mode} route-admission field set"
    );
    assert_eq!(admission.get("consumer"), Some(&"run"), "{mode} consumer");
    assert_eq!(
        admission.get("mode"),
        Some(&if direct { "explicit-mir" } else { "default" }),
        "{mode} route mode"
    );
    assert_eq!(
        admission.get("checker_profile"),
        Some(&EXECUTION_PROFILE),
        "{mode} execution profile"
    );
    assert_sha256(admission["mir_digest"], "MIR", &mode);
    assert_sha256(admission["receipt_identity_digest"], "route", &mode);
    (
        EXECUTION_PROFILE.to_string(),
        admission["mir_digest"].to_string(),
        admission["receipt_identity_digest"].to_string(),
    )
}

#[test]
fn direct_print_result_default_and_explicit_verify_share_the_exact_l2_route() {
    let mimi = mimi_bin();
    for source in fixtures() {
        let default = verify_identity(&mimi, &source, false);
        let explicit = verify_identity(&mimi, &source, true);
        assert_eq!(
            default,
            explicit,
            "default and explicit --mir must verify the same Checker-bound route for {}",
            source.display()
        );
    }
}

#[test]
fn direct_print_result_default_and_explicit_run_share_the_exact_bytecode_route() {
    let mimi = mimi_bin();
    let expectations = [
        ("42\n", 0),
        ("borrowed String λ\n", 0),
        ("0\n", 0),
        ("42\n", 1),
    ];
    for (source, (expected_stdout, expected_exit)) in fixtures().into_iter().zip(expectations) {
        let default = run_identity(&mimi, &source, false, expected_stdout, expected_exit);
        let explicit = run_identity(&mimi, &source, true, expected_stdout, expected_exit);
        assert_eq!(
            default,
            explicit,
            "default and explicit --mir must run through one Checker-bound Bytecode route for {}",
            source.display()
        );
    }
}
