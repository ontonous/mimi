use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "map-any-owner-v2-branch-local-set-diamond-v1";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const VERIFIER_PROVENANCE_PREFIX: &str = "canonical MIR verifier provenance: ";
const FIXTURES: [(&str, &str); 2] = [
    (
        "true-branch",
        "mir_branch_local_persistent_map_set_diamond_true.mimi",
    ),
    (
        "false-branch",
        "mir_branch_local_persistent_map_set_diamond_false.mimi",
    ),
];

#[derive(Debug, PartialEq, Eq)]
struct RouteIdentity {
    mir_digest: String,
    receipt_identity_digest: String,
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn one_line<'a>(stderr: &'a str, prefix: &str, mode: &str) -> &'a str {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must emit exactly one {prefix:?} record:\n{stderr}"
    );
    lines[0]
}

fn fields<'a>(line: &'a str, prefix: &str, mode: &str) -> BTreeMap<&'a str, &'a str> {
    let payload = line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{mode} record is missing prefix {prefix:?}"));
    let mut result = BTreeMap::new();
    for token in payload.split_whitespace() {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{mode} has malformed field {token:?}"));
        assert!(
            result.insert(key, value).is_none(),
            "{mode} repeats field {key}"
        );
    }
    result
}

fn assert_digest(value: &str, label: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} must be a lowercase 256-bit digest, got {value:?}"
    );
}

fn verify_route(fixture: &Path, explicit_mir: bool, label: &str) -> RouteIdentity {
    let mut command = Command::new(mimi_bin());
    command
        .env("MIMI_ROUTE_CENSUS", "1")
        .env("MIMI_VERBOSE", "1")
        .arg("verify")
        .arg(fixture);
    if explicit_mir {
        command.arg("--mir");
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{label} could not start mimi verify: {error}"));
    assert_success(&output, label);

    assert_eq!(
        text(&output.stdout),
        format!("No contracts to verify in {}\n", fixture.display()),
        "{label} must retain the fixture's empty proof request"
    );

    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} silently selected a legacy route:\n{stderr}"
    );
    assert!(
        !stderr.contains("MIR-COVERAGE-001"),
        "{label} lost dedicated route coverage:\n{stderr}"
    );

    let admission = one_line(&stderr, ROUTE_ADMISSION_PREFIX, label);
    let admission_fields = fields(admission, ROUTE_ADMISSION_PREFIX, label);
    let route_field_order = admission
        .strip_prefix(ROUTE_ADMISSION_PREFIX)
        .expect("route record has its stable prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("field has key=value").0)
        .collect::<Vec<_>>();
    assert_eq!(
        route_field_order,
        [
            "consumer",
            "mode",
            "checker_profile",
            "mir_digest",
            "receipt_identity_digest",
        ]
        .to_vec(),
        "{label} route-admission identity shape changed"
    );
    assert_eq!(
        admission_fields.get("consumer").copied(),
        Some("verify"),
        "{label} route consumer"
    );
    assert_eq!(
        admission_fields.get("mode").copied(),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{label} route mode"
    );
    assert_eq!(
        admission_fields.get("checker_profile").copied(),
        Some(ROUTE_PROFILE),
        "{label} must select the dedicated BranchLocalSetDiamond route"
    );
    let mir_digest = admission_fields
        .get("mir_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its MIR digest"));
    let receipt_identity_digest = admission_fields
        .get("receipt_identity_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its receipt identity digest"));
    assert_digest(mir_digest, &format!("{label} MIR digest"));
    assert_digest(
        receipt_identity_digest,
        &format!("{label} receipt identity digest"),
    );

    let provenance = one_line(&stderr, VERIFIER_PROVENANCE_PREFIX, label);
    let provenance_fields = fields(provenance, VERIFIER_PROVENANCE_PREFIX, label);
    assert_eq!(
        provenance_fields.get("profile").copied(),
        Some(ROUTE_PROFILE),
        "{label} verifier must consume the dedicated route receipt"
    );
    assert_eq!(
        provenance_fields.get("mir_digest").copied(),
        Some(mir_digest),
        "{label} verifier provenance must match the admitted MIR graph"
    );
    for field in [
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
        "source_hash",
    ] {
        let value = provenance_fields
            .get(field)
            .unwrap_or_else(|| panic!("{label} verifier provenance omitted {field}"));
        assert_digest(value, &format!("{label} verifier {field}"));
    }

    RouteIdentity {
        mir_digest: mir_digest.to_owned(),
        receipt_identity_digest: receipt_identity_digest.to_owned(),
    }
}

fn assert_success(output: &Output, mode: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{mode} must exit successfully:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

#[test]
fn branch_local_set_diamond_verify_routes_fail_closed() {
    for (branch, fixture_name) in FIXTURES {
        let fixture = fixture_path(fixture_name);
        let default = verify_route(&fixture, false, &format!("{branch} default verify"));
        let explicit = verify_route(&fixture, true, &format!("{branch} explicit MIR verify"));
        assert_eq!(
            default, explicit,
            "{branch} default and explicit --mir verification must consume the same route identity"
        );
    }
}
