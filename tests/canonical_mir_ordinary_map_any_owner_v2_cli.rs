use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MIR_RECEIPT_FIELDS: &[&str] = &[
    "schema",
    "profile",
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
    "root_owners",
];
const MIR_RECEIPT_HEADER: &str = "mimi-mir-route-manifest-v1";
const MIR_RECEIPT_SCHEMA: &str = "mimi-mir-route-receipt-v1";
const CHECKER_INSPECTION_HEADER: &str = "mimi-checker-inspection-manifest-v1";

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
        .expect("Cargo must provide mimi for MapAnyOwner v2 CLI evidence")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_record.mimi")
}

fn values_unpack_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_unpack.mimi")
}

fn keys_snapshot_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_keys_snapshot.mimi")
}

fn values_field_move_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_field_move.mimi")
}

fn values_snapshot_replacement_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_snapshot_replacement.mimi")
}

fn values_mixed_snapshot_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_mixed_snapshot.mimi")
}

fn values_snapshot_remove_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_snapshot_remove.mimi")
}

fn computed_key_get_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_computed_key_get.mimi")
}

fn computed_key_string_get_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_computed_key_get_string.mimi")
}

fn checked_get_settled_branch_tail_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_checked_get_settled_branch_tail.mimi")
}

fn checked_get_bool_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_checked_get_hit_bool.mimi")
}

fn checked_get_multi_lookup_fixture(reversed: bool) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(if reversed {
            "mir_ordinary_map_any_owner_v2_checked_get_multi_lookup_reversed.mimi"
        } else {
            "mir_ordinary_map_any_owner_v2_checked_get_multi_lookup.mimi"
        })
}

fn values_bool_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_bool_fail_closed.mimi")
}

fn incomplete_keys_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_incomplete_keys.mimi")
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

fn assert_no_legacy_fallback(stderr: &str, mode: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} selected a legacy route: {stderr}"
    );
}

fn assert_no_canonical_route(output: &Output, mode: &str) {
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        !diagnostic.contains("canonical route disposition: canonical"),
        "{mode} reported an executable canonical route despite rejecting owner-v2:\n{diagnostic}"
    );
}

fn assert_v2_route_fail_closed(output: &Output, mode: &str) {
    assert!(
        !output.status.success(),
        "{mode} unexpectedly accepted the not-ready MapAnyOwner v2 route:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains("not production-ready") || diagnostic.contains("MIR-COVERAGE-001"),
        "{mode} did not report the V2 readiness boundary:\n{diagnostic}"
    );
    assert_no_legacy_fallback(&text(&output.stderr), mode);
    assert_no_canonical_route(output, mode);
}

fn assert_complete_coverage_candidate_reported(output: &Output, mode: &str) {
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains("CompleteCoverage"),
        "{mode} did not identify the ValuesUnpack CompleteCoverage candidate:\n{diagnostic}"
    );
}

fn parse_manifest(output: &Output) -> BTreeMap<String, String> {
    assert_success(output, "mimi mir --receipt");
    let receipt_text = text(&output.stdout);
    let mut lines = receipt_text.lines();
    assert_eq!(
        lines.next(),
        Some(MIR_RECEIPT_HEADER),
        "the CLI MIR receipt header changed"
    );
    let mut fields = BTreeMap::new();
    for (row, expected_field) in MIR_RECEIPT_FIELDS.iter().enumerate() {
        let line = lines
            .next()
            .unwrap_or_else(|| panic!("CLI MIR receipt omitted {expected_field} at row {row}"));
        let (field, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("CLI MIR receipt row {row} is missing '=': {line:?}"));
        assert_eq!(
            field, *expected_field,
            "CLI MIR receipt order changed at row {row}"
        );
        assert!(
            fields
                .insert(field.to_string(), value.to_string())
                .is_none(),
            "CLI MIR receipt repeats {field} at row {row}"
        );
    }
    assert!(
        lines.next().is_none(),
        "CLI MIR receipt has rows after its exact field set: {receipt_text}"
    );
    assert_eq!(fields.len(), MIR_RECEIPT_FIELDS.len());
    assert_eq!(
        fields.get("schema").map(String::as_str),
        Some(MIR_RECEIPT_SCHEMA),
        "the CLI MIR receipt schema changed"
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some("cli-mir-v1"),
        "the CLI receipt schema changed"
    );
    for field in [
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
    ] {
        let digest = fields
            .get(field)
            .unwrap_or_else(|| panic!("MIR receipt omitted {field}"));
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "MIR receipt has malformed {field}: {digest}"
        );
    }
    assert!(
        fields.contains_key("root_owners"),
        "MIR receipt omitted its ownership root identity"
    );
    fields
}

fn assert_read_only_receipt(mimi: &Path, source: &Path) -> String {
    let receipt = Command::new(mimi)
        .arg("mir")
        .arg("--all")
        .arg("--receipt")
        .arg(source)
        .output()
        .expect("materialize read-only MapAnyOwner v2 MIR receipt");
    let manifest = parse_manifest(&receipt);
    assert_eq!(
        manifest.get("profile").map(String::as_str),
        Some("cli-mir-v1")
    );
    assert!(
        manifest
            .get("root_owners")
            .is_some_and(|owners| owners.contains("function:main")),
        "MIR inspection receipt omitted the candidate's main owner: {manifest:?}"
    );
    assert!(
        !manifest.contains_key("execution_route") && !manifest.contains_key("production_ready"),
        "read-only MIR receipt claimed production route admission: {manifest:?}"
    );
    assert_no_canonical_route(&receipt, "mimi mir --all --receipt");
    manifest["mir_digest"].clone()
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mimi-map-any-owner-v2-cli-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("integration")
        ));
        std::fs::create_dir_all(&path).expect("create isolated MapAnyOwner v2 CLI directory");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_cli_production_routes_fail_closed(mimi: &Path, source: &Path, prefix: &str) {
    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR run")
        } else {
            format!("{prefix} default run")
        };
        let mut command = Command::new(mimi);
        command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_v2_route_fail_closed(&run, &mode);
        if prefix == "ValuesUnpack" {
            assert_complete_coverage_candidate_reported(&run, &mode);
        }
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR native build")
        } else {
            format!("{prefix} default native build")
        };
        let binary = directory.0.join(if direct {
            format!("{prefix}-direct")
        } else {
            format!("{prefix}-default")
        });
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_v2_route_fail_closed(&build, &mode);
        if prefix == "ValuesUnpack" {
            assert_complete_coverage_candidate_reported(&build, &mode);
        }
        assert!(
            !binary.exists(),
            "{mode} left behind an executable after rejecting the V2 route"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR verify")
        } else {
            format!("{prefix} default verify")
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify")
            .arg(source);
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_v2_route_fail_closed(&verify, &mode);
        if prefix == "ValuesUnpack" {
            assert_complete_coverage_candidate_reported(&verify, &mode);
        }
    }
}

fn assert_cli_checker_rejects_production_routes(
    mimi: &Path,
    source: &Path,
    prefix: &str,
    diagnostic_code: &str,
) {
    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR run")
        } else {
            format!("{prefix} default run")
        };
        let mut command = Command::new(mimi);
        command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_checker_rejection_fail_closed(&run, &mode, diagnostic_code);
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR native build")
        } else {
            format!("{prefix} default native build")
        };
        let binary = directory.0.join(if direct {
            format!("{prefix}-direct")
        } else {
            format!("{prefix}-default")
        });
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_checker_rejection_fail_closed(&build, &mode, diagnostic_code);
        assert!(
            !binary.exists(),
            "{mode} left behind an executable after Checker rejected the source"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR verify")
        } else {
            format!("{prefix} default verify")
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify")
            .arg(source);
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_checker_rejection_fail_closed(&verify, &mode, diagnostic_code);
    }
}

fn assert_checker_rejection_fail_closed(output: &Output, mode: &str, diagnostic_code: &str) {
    assert!(
        !output.status.success(),
        "{mode} unexpectedly accepted a source rejected by Checker:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains(diagnostic_code),
        "{mode} did not report Checker diagnostic {diagnostic_code}:\n{diagnostic}"
    );
    assert_no_legacy_fallback(&text(&output.stderr), mode);
    assert_no_canonical_route(output, mode);
}

fn assert_cli_values_field_move_routes(mimi: &Path, source: &Path) {
    const PROFILE: &str = "map-any-owner-v2-values-field-move-v1";
    let expected_output = "true\ntrue\n";

    for direct in [false, true] {
        let mode = if direct {
            "ValuesFieldMove explicit MIR run"
        } else {
            "ValuesFieldMove default run"
        };
        let mut command = Command::new(mimi);
        command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&run, mode);
        assert_eq!(text(&run.stdout), expected_output, "{mode} output");
        assert!(
            text(&run.stderr).contains(PROFILE),
            "{mode} lost its dedicated route identity: {}",
            text(&run.stderr)
        );
        assert_no_legacy_fallback(&text(&run.stderr), mode);
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            "ValuesFieldMove explicit MIR native build"
        } else {
            "ValuesFieldMove default native build"
        };
        let binary = directory.0.join(if direct {
            "values-field-move-direct"
        } else {
            "values-field-move-default"
        });
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&build, mode);
        assert!(
            text(&build.stderr).contains(PROFILE),
            "{mode} lost its dedicated route identity: {}",
            text(&build.stderr)
        );
        assert_no_legacy_fallback(&text(&build.stderr), mode);
        assert!(binary.is_file(), "{mode} did not produce its native binary");

        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("run {mode} output: {error}"));
        assert_success(&native, "run ValuesFieldMove native artifact");
        assert_eq!(
            text(&native.stdout),
            expected_output,
            "{mode} native output"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            "ValuesFieldMove explicit MIR verify"
        } else {
            "ValuesFieldMove default verify"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify")
            .arg(source);
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&verify, mode);
        let diagnostic = text(&verify.stderr);
        assert!(
            diagnostic.contains(PROFILE),
            "{mode} lost its dedicated route receipt identity: {diagnostic}"
        );
        assert_no_legacy_fallback(&diagnostic, mode);
    }
}

fn assert_cli_values_snapshot_replacement_routes(mimi: &Path, source: &Path) {
    const PROFILE: &str = "map-any-owner-v2-values-snapshot-replacement-v1";
    const EXPECTED_OUTPUT: &str = "true\ntrue\n";

    for direct in [false, true] {
        let mode = if direct {
            "ValuesSnapshotReplacement explicit MIR run"
        } else {
            "ValuesSnapshotReplacement default run"
        };
        let mut command = Command::new(mimi);
        command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&run, mode);
        assert_eq!(text(&run.stdout), EXPECTED_OUTPUT, "{mode} output");
        assert!(
            text(&run.stderr).contains(PROFILE),
            "{mode} lost its dedicated route identity: {}",
            text(&run.stderr)
        );
        assert_no_legacy_fallback(&text(&run.stderr), mode);
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            "ValuesSnapshotReplacement explicit MIR native build"
        } else {
            "ValuesSnapshotReplacement default native build"
        };
        let binary = directory.0.join(if direct {
            "values-snapshot-replacement-direct"
        } else {
            "values-snapshot-replacement-default"
        });
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&build, mode);
        assert!(
            text(&build.stderr).contains(PROFILE),
            "{mode} lost its dedicated route identity: {}",
            text(&build.stderr)
        );
        assert_no_legacy_fallback(&text(&build.stderr), mode);
        assert!(binary.is_file(), "{mode} did not produce its native binary");

        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("run {mode} output: {error}"));
        assert_success(&native, "run ValuesSnapshotReplacement native artifact");
        assert_eq!(
            text(&native.stdout),
            EXPECTED_OUTPUT,
            "{mode} native output"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            "ValuesSnapshotReplacement explicit MIR verify"
        } else {
            "ValuesSnapshotReplacement default verify"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify")
            .arg(source);
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&verify, mode);
        let diagnostic = text(&verify.stderr);
        assert!(
            diagnostic.contains(PROFILE),
            "{mode} lost its route receipt identity: {diagnostic}"
        );
        assert_no_legacy_fallback(&diagnostic, mode);
    }
}

fn route_admission_identity(
    output: &Output,
    consumer: &str,
    mode_name: &str,
    mode: &str,
) -> (String, String, String) {
    route_admission_identity_with_profile(
        output,
        consumer,
        mode_name,
        mode,
        "map-any-owner-v2-values-mixed-snapshot-v1",
    )
}

fn route_admission_identity_with_profile(
    output: &Output,
    consumer: &str,
    mode_name: &str,
    mode: &str,
    expected_profile: &str,
) -> (String, String, String) {
    route_admission_identity_with_profile_and_status(
        output,
        consumer,
        mode_name,
        mode,
        expected_profile,
        0,
    )
}

fn route_admission_identity_with_profile_and_status(
    output: &Output,
    consumer: &str,
    mode_name: &str,
    mode: &str,
    expected_profile: &str,
    expected_status: i32,
) -> (String, String, String) {
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{mode} exit status: {}",
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
        "{mode} must report exactly one Checker route-admission record: {stderr}"
    );
    let route_tokens = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route-admission line has its stable prefix")
        .split_whitespace()
        .collect::<Vec<_>>();
    let expected_route_order: &[&str] = match consumer {
        "build" => &[
            "consumer",
            "mode",
            "checker_profile",
            "build_route_profile",
            "mir_digest",
            "mir_route_identity_digest",
            "receipt_identity_digest",
            "build_route_identity_digest",
        ],
        "run" | "verify" => &[
            "consumer",
            "mode",
            "checker_profile",
            "mir_digest",
            "receipt_identity_digest",
        ],
        _ => panic!("{mode} has unsupported route-admission consumer {consumer:?}"),
    };
    let route_field_order = route_tokens
        .iter()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} has malformed route field {token:?}"))
                .0
        })
        .collect::<Vec<_>>();
    assert_eq!(
        route_field_order,
        expected_route_order.to_vec(),
        "{mode} route-admission field order changed"
    );
    let mut fields = BTreeMap::new();
    for token in route_tokens {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{mode} has malformed route field {token:?}"));
        assert!(
            fields.insert(key, value).is_none(),
            "{mode} repeats route field {key}"
        );
    }
    let expected_route_fields: &[&str] = match consumer {
        "build" => &[
            "build_route_identity_digest",
            "build_route_profile",
            "checker_profile",
            "consumer",
            "mir_digest",
            "mir_route_identity_digest",
            "mode",
            "receipt_identity_digest",
        ],
        "run" | "verify" => &[
            "checker_profile",
            "consumer",
            "mir_digest",
            "mode",
            "receipt_identity_digest",
        ],
        _ => unreachable!("consumer was checked above"),
    };
    assert_eq!(
        fields.keys().copied().collect::<Vec<_>>(),
        expected_route_fields.to_vec(),
        "{mode} route-admission field set changed"
    );
    assert_eq!(fields.get("consumer"), Some(&consumer), "{mode} consumer");
    assert_eq!(fields.get("mode"), Some(&mode_name), "{mode} route mode");
    assert_eq!(
        fields.get("checker_profile"),
        Some(&expected_profile),
        "{mode} Checker profile"
    );
    let digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{mode} omitted its MIR digest"));
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{mode} has malformed MIR digest {digest}"
    );
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{mode} omitted its route receipt identity digest"));
    assert!(
        receipt_identity_digest.len() == 64
            && receipt_identity_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{mode} has malformed route receipt identity digest {receipt_identity_digest}"
    );
    if consumer == "build" {
        let build_route_profile = fields["build_route_profile"];
        let native_data_export_profile = format!("{expected_profile}+native-data-export-v1");
        assert!(
            build_route_profile == expected_profile
                || build_route_profile == native_data_export_profile,
            "{mode} has unexpected native build route profile {build_route_profile}"
        );
        let mir_route_identity_digest = fields["mir_route_identity_digest"];
        assert!(
            mir_route_identity_digest.len() == 64
                && mir_route_identity_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{mode} has malformed MIR route identity digest {mir_route_identity_digest}"
        );
        let build_route_identity_digest = fields["build_route_identity_digest"];
        assert_eq!(
            build_route_identity_digest, *receipt_identity_digest,
            "{mode} build route identity differs from its receipt identity"
        );
    }
    assert_no_legacy_fallback(&stderr, mode);
    (
        fields["checker_profile"].to_string(),
        digest.to_string(),
        receipt_identity_digest.to_string(),
    )
}

fn assert_native_build_receipt(
    output: &Output,
    expected: &(String, String, String),
    consumer: &str,
    mode: &str,
    expected_root_owners: &str,
) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must report exactly one successful native build receipt: {stderr}"
    );
    let native_tokens = lines[0].split_whitespace().skip(2).collect::<Vec<_>>();
    let native_field_order = native_tokens
        .iter()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} has malformed build receipt field {token:?}"))
                .0
        })
        .collect::<Vec<_>>();
    let expected_native_field_order = [
        "consumer",
        "route_id",
        "build_route_profile",
        "schema",
        "profile",
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
        "root_owners",
        "mir_route_identity_digest",
        "route_identity_digest",
    ];
    assert_eq!(
        native_field_order,
        expected_native_field_order.to_vec(),
        "{mode} native build receipt field order changed"
    );
    let fields = native_tokens
        .iter()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} has malformed build receipt field {token:?}"))
        })
        .fold(BTreeMap::new(), |mut fields, (key, value)| {
            assert!(
                fields.insert(key, value).is_none(),
                "{mode} repeats native build receipt field {key}"
            );
            fields
        });
    assert_eq!(
        fields.keys().copied().collect::<Vec<_>>(),
        [
            "abi_digest",
            "build_route_profile",
            "consumer",
            "ffi_digest",
            "flow_transition_digest",
            "mir_digest",
            "mir_route_identity_digest",
            "ownership_digest",
            "profile",
            "root_owners",
            "route_id",
            "route_identity_digest",
            "schema",
            "type_desc_digest",
        ]
        .to_vec(),
        "{mode} native build receipt field set changed"
    );
    assert_eq!(
        fields.get("consumer"),
        Some(&consumer),
        "{mode} build consumer"
    );
    assert_eq!(
        fields.get("route_id"),
        Some(&expected.0.as_str()),
        "{mode} native route id"
    );
    assert_eq!(
        fields.get("profile"),
        Some(&expected.0.as_str()),
        "{mode} native receipt profile"
    );
    assert_eq!(
        fields.get("mir_digest"),
        Some(&expected.1.as_str()),
        "{mode} native receipt digest"
    );
    assert_eq!(
        fields.get("schema"),
        Some(&MIR_RECEIPT_SCHEMA),
        "{mode} native receipt schema"
    );
    let native_data_export_profile = format!("{}+native-data-export-v1", expected.0);
    let build_route_profile = fields["build_route_profile"];
    assert!(
        build_route_profile == expected.0
            || build_route_profile == native_data_export_profile.as_str(),
        "{mode} has unexpected native build profile {:?}",
        build_route_profile
    );
    assert_eq!(
        fields.get("root_owners"),
        Some(&expected_root_owners),
        "{mode} native receipt root owners"
    );
    for field in [
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
        "mir_route_identity_digest",
        "route_identity_digest",
    ] {
        let digest = fields
            .get(field)
            .unwrap_or_else(|| panic!("{mode} native receipt omitted {field}"));
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{mode} native receipt has malformed {field}: {digest}"
        );
    }
    let admission_line = stderr
        .lines()
        .find(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .expect("native build receipt has its route-admission record");
    let admission_fields = admission_line
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route-admission line has its stable prefix")
        .split_whitespace()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} has malformed route-admission field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("build_route_profile"),
        admission_fields.get("build_route_profile"),
        "{mode} native receipt build profile differs from route admission"
    );
    assert_eq!(
        fields.get("mir_route_identity_digest"),
        admission_fields.get("mir_route_identity_digest"),
        "{mode} native receipt MIR identity differs from route admission"
    );
    assert_eq!(
        fields.get("route_identity_digest"),
        admission_fields.get("build_route_identity_digest"),
        "{mode} native receipt identity differs from build route admission"
    );
    assert_eq!(
        fields.get("route_identity_digest"),
        admission_fields.get("receipt_identity_digest"),
        "{mode} native receipt identity differs from its receipt admission"
    );
}

fn assert_verifier_receipt(output: &Output, expected: &(String, String, String), mode: &str) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("canonical MIR verifier provenance: "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must report the exact receipt consumed by the MIR verifier: {stderr}"
    );
    let fields = lines[0]
        .strip_prefix("canonical MIR verifier provenance: ")
        .expect("verifier provenance line has its stable prefix")
        .split_whitespace()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} has malformed verifier receipt field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("profile"),
        Some(&expected.0.as_str()),
        "{mode} verifier profile"
    );
    assert_eq!(
        fields.get("mir_digest"),
        Some(&expected.1.as_str()),
        "{mode} verifier digest"
    );
}

fn assert_cli_values_mixed_snapshot_routes(mimi: &Path, source: &Path) {
    const EXPECTED_OUTPUT: &str = "true\ntrue\ntrue\ntrue\n";
    let mut shared_identity = None;

    for direct in [false, true] {
        let mode = if direct {
            "ValuesMixedSnapshot explicit MIR run"
        } else {
            "ValuesMixedSnapshot default run"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&run, mode);
        assert_eq!(text(&run.stdout), EXPECTED_OUTPUT, "{mode} output");
        let identity = route_admission_identity(
            &run,
            "run",
            if direct { "explicit-mir" } else { "default" },
            mode,
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            "ValuesMixedSnapshot explicit MIR native build"
        } else {
            "ValuesMixedSnapshot default native build"
        };
        let binary = directory.0.join(if direct {
            "values-mixed-snapshot-direct"
        } else {
            "values-mixed-snapshot-default"
        });
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
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&build, mode);
        assert!(binary.is_file(), "{mode} did not produce its native binary");
        let identity = route_admission_identity(
            &build,
            "build",
            if direct { "explicit-mir" } else { "default" },
            mode,
        );
        assert_native_build_receipt(
            &build,
            &identity,
            if direct { "explicit-mir" } else { "default" },
            mode,
            "function:main",
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("run {mode} output: {error}"));
        assert_success(&native, "run ValuesMixedSnapshot native artifact");
        assert_eq!(
            text(&native.stdout),
            EXPECTED_OUTPUT,
            "{mode} native output"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            "ValuesMixedSnapshot explicit MIR verify"
        } else {
            "ValuesMixedSnapshot default verify"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        let identity = route_admission_identity(
            &verify,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            mode,
        );
        assert_verifier_receipt(&verify, &identity, mode);
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }
}

fn assert_cli_values_snapshot_remove_routes(mimi: &Path, source: &Path) {
    const PROFILE: &str = "map-any-owner-v2-values-mixed-snapshot-remove-v1";
    const EXPECTED_OUTPUT: &str = "true\ntrue\ntrue\ntrue\n";
    let mut shared_identity = None;

    for direct in [false, true] {
        let mode = if direct {
            "ValuesMixedSnapshotRemove explicit MIR run"
        } else {
            "ValuesMixedSnapshotRemove default run"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert!(
            !text(&run.stderr).contains("MIR-COVERAGE-001")
                && !text(&run.stderr).contains("runtime error")
                && !text(&run.stderr).contains("[E080"),
            "{mode} returned the Boolean process code with no runtime failure: {}",
            text(&run.stderr)
        );
        assert_eq!(text(&run.stdout), EXPECTED_OUTPUT, "{mode} output");
        let identity = route_admission_identity_with_profile_and_status(
            &run,
            "run",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
            0,
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            "ValuesMixedSnapshotRemove explicit MIR native build"
        } else {
            "ValuesMixedSnapshotRemove default native build"
        };
        let binary = directory.0.join(if direct {
            "values-mixed-snapshot-remove-direct"
        } else {
            "values-mixed-snapshot-remove-default"
        });
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
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&build, mode);
        assert!(binary.is_file(), "{mode} did not produce its native binary");
        let identity = route_admission_identity_with_profile(
            &build,
            "build",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        assert_native_build_receipt(
            &build,
            &identity,
            if direct { "explicit-mir" } else { "default" },
            mode,
            "function:main",
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("run {mode} output: {error}"));
        assert_success(&native, "run ValuesMixedSnapshotRemove native artifact");
        assert_eq!(
            text(&native.stdout),
            EXPECTED_OUTPUT,
            "{mode} native output"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            "ValuesMixedSnapshotRemove explicit MIR verify"
        } else {
            "ValuesMixedSnapshotRemove default verify"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        let identity = route_admission_identity_with_profile(
            &verify,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        assert_verifier_receipt(&verify, &identity, mode);
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }
}

fn assert_cli_checked_get_branch_tail_routes(mimi: &Path, source: &Path, source_digest: &str) {
    const PROFILE: &str = "map-any-owner-v2-checked-get-v1";
    const EXPECTED_OUTPUT: &str = "";
    let mut shared_identity = None;
    let mut route_summaries: Vec<(&str, String, String)> = Vec::with_capacity(6);

    for direct in [false, true] {
        let mode = if direct {
            "CheckedGet settled-tail explicit MIR run"
        } else {
            "CheckedGet settled-tail default run"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert!(
            !text(&run.stderr).contains("MIR-COVERAGE-001")
                && !text(&run.stderr).contains("runtime error")
                && !text(&run.stderr).contains("[E080"),
            "{mode} returned the Boolean process code with no runtime failure: {}",
            text(&run.stderr)
        );
        assert_eq!(text(&run.stdout), EXPECTED_OUTPUT, "{mode} output");
        let identity = route_admission_identity_with_profile_and_status(
            &run,
            "run",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
            1,
        );
        route_summaries.push((
            if direct {
                "explicit-mir-run"
            } else {
                "default-run"
            },
            identity.0.clone(),
            identity.1.clone(),
        ));
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            "CheckedGet settled-tail explicit MIR native build"
        } else {
            "CheckedGet settled-tail default native build"
        };
        let binary = directory.0.join(if direct {
            "checked-get-settled-tail-direct"
        } else {
            "checked-get-settled-tail-default"
        });
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
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&build, mode);
        assert!(binary.is_file(), "{mode} did not produce a native binary");
        let identity = route_admission_identity_with_profile(
            &build,
            "build",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        route_summaries.push((
            if direct {
                "explicit-mir-build"
            } else {
                "default-build"
            },
            identity.0.clone(),
            identity.1.clone(),
        ));
        assert_native_build_receipt(
            &build,
            &identity,
            if direct { "explicit-mir" } else { "default" },
            mode,
            "function:main",
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("run {mode} output: {error}"));
        assert_eq!(native.status.code(), Some(1), "{mode} native exit code");
        assert_eq!(native.stdout, b"", "{mode} native stdout");
        assert!(
            native.stderr.is_empty(),
            "{mode} returns the Boolean process code without a native runtime diagnostic: {}",
            text(&native.stderr)
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            "CheckedGet settled-tail explicit MIR verify"
        } else {
            "CheckedGet settled-tail default verify"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        let identity = route_admission_identity_with_profile(
            &verify,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        route_summaries.push((
            if direct {
                "explicit-mir-verify"
            } else {
                "default-verify"
            },
            identity.0.clone(),
            identity.1.clone(),
        ));
        assert_verifier_receipt(&verify, &identity, mode);
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }

    assert_eq!(route_summaries.len(), 6);
    for ((path, profile, digest), expected_path) in route_summaries.into_iter().zip([
        "default-run",
        "explicit-mir-run",
        "default-build",
        "explicit-mir-build",
        "default-verify",
        "explicit-mir-verify",
    ]) {
        assert_eq!(path, expected_path, "CheckedGet route order or set changed");
        assert_eq!(profile, PROFILE, "{path} changed the Checker route profile");
        assert_eq!(digest, source_digest, "{path} MIR digest changed");
    }
}

fn assert_cli_computed_key_get_routes(
    mimi: &Path,
    source: &Path,
    source_digest: &str,
    expected_root_owners: &str,
) {
    const PROFILE: &str = "map-any-owner-v2-computed-key-get-v1";
    const EXPECTED_OUTPUT: &str = "";
    const EXPECTED_PATHS: [&str; 6] = [
        "default-run",
        "explicit-mir-run",
        "default-build",
        "explicit-mir-build",
        "default-verify",
        "explicit-mir-verify",
    ];
    let mut shared_identity = None;
    let mut route_summaries: Vec<(&str, String, String, String)> = Vec::with_capacity(6);

    for direct in [false, true] {
        let mode = if direct {
            "ComputedKeyGet explicit MIR run"
        } else {
            "ComputedKeyGet default run"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&run, mode);
        assert_eq!(text(&run.stdout), EXPECTED_OUTPUT, "{mode} output");
        let identity = route_admission_identity_with_profile(
            &run,
            "run",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        route_summaries.push((
            if direct {
                "explicit-mir-run"
            } else {
                "default-run"
            },
            identity.0.clone(),
            identity.1.clone(),
            identity.2.clone(),
        ));
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            "ComputedKeyGet explicit MIR native build"
        } else {
            "ComputedKeyGet default native build"
        };
        let binary = directory.0.join(if direct {
            "computed-key-get-direct"
        } else {
            "computed-key-get-default"
        });
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
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&build, mode);
        assert!(binary.is_file(), "{mode} did not produce its native binary");
        let identity = route_admission_identity_with_profile(
            &build,
            "build",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        route_summaries.push((
            if direct {
                "explicit-mir-build"
            } else {
                "default-build"
            },
            identity.0.clone(),
            identity.1.clone(),
            identity.2.clone(),
        ));
        assert_native_build_receipt(
            &build,
            &identity,
            if direct { "explicit-mir" } else { "default" },
            mode,
            expected_root_owners,
        );
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }

        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("run {mode} output: {error}"));
        assert_success(&native, "run ComputedKeyGet native artifact");
        assert_eq!(native.status.code(), Some(0), "{mode} native exit code");
        assert_eq!(
            text(&native.stdout),
            EXPECTED_OUTPUT,
            "{mode} native output"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            "ComputedKeyGet explicit MIR verify"
        } else {
            "ComputedKeyGet default verify"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        let identity = route_admission_identity_with_profile(
            &verify,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        route_summaries.push((
            if direct {
                "explicit-mir-verify"
            } else {
                "default-verify"
            },
            identity.0.clone(),
            identity.1.clone(),
            identity.2.clone(),
        ));
        assert_verifier_receipt(&verify, &identity, mode);
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }

    assert_eq!(route_summaries.len(), 6);
    for ((path, profile, digest, receipt_identity_digest), expected_path) in
        route_summaries.into_iter().zip(EXPECTED_PATHS)
    {
        assert_eq!(
            path, expected_path,
            "ComputedKeyGet route order or set changed"
        );
        assert_eq!(profile, PROFILE, "{path} changed the Checker route profile");
        assert_eq!(
            digest, source_digest,
            "{path} MIR digest differs from the read-only source receipt"
        );
        eprintln!(
            "MIMI_COMPUTED_KEY_GET_L1L2_ROUTE path={path} profile={profile} mir_digest={digest} receipt_identity_digest={receipt_identity_digest} no_legacy_fallback=true"
        );
    }
    eprintln!("MIMI_COMPUTED_KEY_GET_L1L2_SOURCE profile=cli-mir-v1 mir_digest={source_digest}");
}

fn assert_computed_key_get_near_miss_fail_closed(
    mimi: &Path,
    source: &Path,
    prefix: &str,
    expected_diagnostic: &str,
) {
    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR run")
        } else {
            format!("{prefix} default run")
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_computed_key_get_checker_rejection(&run, &mode, expected_diagnostic);
        let diagnostic = format!("{}\n{}", text(&run.stdout), text(&run.stderr));
        assert!(
            diagnostic.contains(expected_diagnostic),
            "{mode} lost the stable ComputedKeyGet near-miss diagnostic {expected_diagnostic:?}:\n{diagnostic}"
        );
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR native build")
        } else {
            format!("{prefix} default native build")
        };
        let binary = directory.0.join(if direct {
            format!("{prefix}-direct")
        } else {
            format!("{prefix}-default")
        });
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
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_computed_key_get_checker_rejection(&build, &mode, expected_diagnostic);
        let diagnostic = format!("{}\n{}", text(&build.stdout), text(&build.stderr));
        assert!(
            diagnostic.contains(expected_diagnostic),
            "{mode} lost the stable ComputedKeyGet near-miss diagnostic {expected_diagnostic:?}:\n{diagnostic}"
        );
        assert!(
            !binary.exists(),
            "{mode} left an executable after rejecting the ComputedKeyGet near-miss"
        );
    }

    for direct in [false, true] {
        let mode = if direct {
            format!("{prefix} explicit MIR verify")
        } else {
            format!("{prefix} default verify")
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_computed_key_get_checker_rejection(&verify, &mode, expected_diagnostic);
        let diagnostic = format!("{}\n{}", text(&verify.stdout), text(&verify.stderr));
        assert!(
            diagnostic.contains(expected_diagnostic),
            "{mode} lost the stable ComputedKeyGet near-miss diagnostic {expected_diagnostic:?}:\n{diagnostic}"
        );
    }
}

fn assert_computed_key_get_checker_rejection(output: &Output, mode: &str, expected_code: &str) {
    assert!(
        !output.status.success(),
        "{mode} unexpectedly accepted a malformed ComputedKeyGet source:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains(expected_code),
        "{mode} did not report the Checker rejection {expected_code:?}:\n{diagnostic}"
    );
    assert_no_legacy_fallback(&text(&output.stderr), mode);
    assert_no_canonical_route(output, mode);
}

#[test]
fn map_any_owner_v2_receipt_is_read_only_while_production_routes_fail_closed() {
    let mimi = mimi_bin();
    let source = fixture();
    assert_read_only_receipt(&mimi, &source);
    assert_cli_production_routes_fail_closed(&mimi, &source, "record");
}

#[test]
fn map_any_owner_v2_values_candidate_is_receipted_but_all_cli_routes_fail_closed() {
    let mimi = mimi_bin();
    let source = values_unpack_fixture();

    // `mimi mir --all --receipt` is diagnostic materialization only. Its
    // checker/MIR identities remain inspectable without claiming production
    // admission for this CompleteCoverage candidate.
    assert_read_only_receipt(&mimi, &source);

    // Default dispatch and explicit `--mir` execution/build/verification
    // must all stop at the shared owner-v2 production-readiness boundary.
    assert_cli_production_routes_fail_closed(&mimi, &source, "ValuesUnpack");
}

#[test]
fn map_any_owner_v2_exact_values_field_move_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = values_field_move_fixture();

    assert_read_only_receipt(&mimi, &source);
    assert_cli_values_field_move_routes(&mimi, &source);
}

#[test]
fn map_any_owner_v2_exact_values_snapshot_replacement_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = values_snapshot_replacement_fixture();

    assert_read_only_receipt(&mimi, &source);
    assert_cli_values_snapshot_replacement_routes(&mimi, &source);
}

#[test]
fn map_any_owner_v2_exact_values_mixed_snapshot_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = values_mixed_snapshot_fixture();

    assert_read_only_receipt(&mimi, &source);
    assert_cli_values_mixed_snapshot_routes(&mimi, &source);
}

#[test]
fn map_any_owner_v2_exact_values_mixed_snapshot_remove_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = values_snapshot_remove_fixture();

    assert_cli_values_snapshot_remove_routes(&mimi, &source);
}

#[test]
fn map_any_owner_v2_exact_computed_key_get_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = computed_key_get_fixture();

    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow ComputedKeyGet CLI profile must not open broad owner-v2 readiness"
        );
    };
    let source_digest = assert_read_only_receipt(&mimi, &source);
    assert_cli_computed_key_get_routes(&mimi, &source, &source_digest, "function:main");
}

#[test]
fn map_any_owner_v2_computed_key_string_get_uses_its_receipted_production_route() {
    let mimi = mimi_bin();
    let source = computed_key_string_get_fixture();

    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "String ComputedKeyGet must not open broad owner-v2 readiness"
        );
    };
    let source_digest = assert_read_only_receipt(&mimi, &source);
    assert_cli_computed_key_get_routes(
        &mimi,
        &source,
        &source_digest,
        "function:main,function:probe",
    );
}

#[test]
fn map_any_owner_v2_checked_get_settled_branch_tail_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = checked_get_settled_branch_tail_fixture();

    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the settled-tail CheckedGet profile must not open broad V2 readiness"
        );
    };
    let source_digest = assert_read_only_receipt(&mimi, &source);
    assert_cli_checked_get_branch_tail_routes(&mimi, &source, &source_digest);
}

#[test]
fn map_any_owner_v2_checked_get_bool_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = checked_get_bool_fixture();

    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow Bool CheckedGet profile must not open broad V2 readiness"
        );
    };
    let source_digest = assert_read_only_receipt(&mimi, &source);
    assert_cli_checked_get_branch_tail_routes(&mimi, &source, &source_digest);
}

#[test]
fn map_any_owner_v2_checked_get_multi_lookup_keeps_each_lookup_receipt_independent() {
    let mimi = mimi_bin();
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow multi-lookup CheckedGet profile must not open broad V2 readiness"
        );
    };

    for reversed in [false, true] {
        let source = checked_get_multi_lookup_fixture(reversed);
        let source_digest = assert_read_only_receipt(&mimi, &source);
        assert_cli_checked_get_branch_tail_routes(&mimi, &source, &source_digest);
    }
}

#[test]
fn map_any_owner_v2_bool_values_remains_fail_closed_without_legacy_retry() {
    let mimi = mimi_bin();
    let source = values_bool_fixture();

    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "Bool CheckedGet must not implicitly enable Bool Values snapshots"
        );
    };
    assert_cli_production_routes_fail_closed(&mimi, &source, "Bool Values");
}

#[test]
fn map_any_owner_v2_checked_get_carried_result_diamond_stays_fail_closed() {
    let mimi = mimi_bin();
    let fixture_text = std::fs::read_to_string(checked_get_settled_branch_tail_fixture())
        .expect("read settled-tail CheckedGet fixture");
    let drop_before_selector = "        drop(pair)\n        let observed = if found";
    assert_eq!(
        fixture_text.matches(drop_before_selector).count(),
        1,
        "the carried-result mutation has one exact pair-settlement anchor"
    );
    let near_miss_text = fixture_text
        .replace(drop_before_selector, "        let observed = if found")
        .replace(
            "        observed\n",
            "        drop(pair)\n        observed\n",
        );
    assert_ne!(
        fixture_text, near_miss_text,
        "the carried-result mutation applies"
    );
    let directory = TemporaryDirectory::new();
    let source = directory.0.join("checked-get-carried-result.mimi");
    std::fs::write(&source, near_miss_text).expect("write carried-result near-miss source");

    assert_cli_checker_rejects_production_routes(
        &mimi,
        &source,
        "CheckedGetCarriedResult",
        "E0200",
    );
}

#[test]
fn map_any_owner_v2_checked_get_branch_tail_map_operation_stays_fail_closed() {
    let mimi = mimi_bin();
    let fixture_text = std::fs::read_to_string(checked_get_settled_branch_tail_fixture())
        .expect("read settled-tail CheckedGet fixture");
    let anchor = "let observed = if found { payload == 41 } else { false }";
    assert_eq!(
        fixture_text.matches(anchor).count(),
        1,
        "the branch-tail Map operation mutation has one exact selector anchor"
    );
    let near_miss_text = fixture_text.replace(
        anchor,
        "let observed = if found { let branch_root = map_new()\n        drop(branch_root)\n        payload == 41 } else { false }",
    );
    assert_ne!(
        fixture_text, near_miss_text,
        "the branch Map operation mutation applies"
    );
    let directory = TemporaryDirectory::new();
    let source = directory.0.join("checked-get-branch-map-operation.mimi");
    std::fs::write(&source, near_miss_text).expect("write branch Map operation near-miss source");

    assert_cli_checker_rejects_production_routes(
        &mimi,
        &source,
        "CheckedGetBranchMapOperation",
        "E0200",
    );
}

#[test]
fn map_any_owner_v2_checked_get_branch_tail_helper_call_stays_fail_closed() {
    let mimi = mimi_bin();
    let fixture_text = std::fs::read_to_string(checked_get_settled_branch_tail_fixture())
        .expect("read settled-tail CheckedGet fixture");
    let with_helper = fixture_text
        .replace(
            "func main() -> bool {",
            "func is_answer(value: i32) -> bool { value == 41 }\n\nfunc main() -> bool {",
        )
        .replace("payload == 41", "is_answer(payload)");
    assert_ne!(
        fixture_text, with_helper,
        "the branch helper-call mutation applies"
    );
    let directory = TemporaryDirectory::new();
    let source = directory.0.join("checked-get-branch-helper-call.mimi");
    std::fs::write(&source, with_helper).expect("write branch helper-call near-miss source");

    assert_cli_checker_rejects_production_routes(
        &mimi,
        &source,
        "CheckedGetBranchHelperCall",
        "E0200",
    );
}

#[test]
fn map_any_owner_v2_computed_key_get_wrong_key_near_miss_stays_fail_closed() {
    let mimi = mimi_bin();
    let fixture_text = std::fs::read_to_string(computed_key_get_fixture())
        .expect("read exact ComputedKeyGet fixture");
    let anchor = "let pair = map_get(root, key)";
    assert_eq!(
        fixture_text.matches(anchor).count(),
        1,
        "the wrong-key mutation has one exact Get anchor"
    );
    let near_miss_text = fixture_text.replace(anchor, "let pair = map_get(root, key + \"?\")");
    assert_ne!(
        fixture_text, near_miss_text,
        "the wrong computed key mutation must apply"
    );
    let directory = TemporaryDirectory::new();
    let source = directory.0.join("computed-key-get-wrong-key.mimi");
    std::fs::write(&source, near_miss_text).expect("write wrong computed-key near-miss fixture");

    assert_computed_key_get_near_miss_fail_closed(
        &mimi,
        &source,
        "ComputedKeyGetWrongKey",
        "E0200",
    );
}

#[test]
fn map_any_owner_v2_computed_key_get_wrong_unpack_type_near_miss_stays_fail_closed() {
    let mimi = mimi_bin();
    let fixture_text = std::fs::read_to_string(computed_key_get_fixture())
        .expect("read exact ComputedKeyGet fixture");
    let unpack_anchor = "let payload = unpack_i32(pair.1)";
    let result_anchor = "payload - 41";
    assert_eq!(
        fixture_text.matches(unpack_anchor).count(),
        1,
        "the wrong unpack-type mutation has one exact payload unpack anchor"
    );
    assert_eq!(
        fixture_text.matches(result_anchor).count(),
        1,
        "the wrong unpack-type mutation has one exact result anchor"
    );
    let near_miss_text = fixture_text
        .replace(unpack_anchor, "let payload = unpack_string(pair.1)")
        .replace(result_anchor, "drop(payload)\n    0");
    assert_ne!(
        fixture_text, near_miss_text,
        "the wrong unpack-type mutation must apply"
    );
    let directory = TemporaryDirectory::new();
    let source = directory.0.join("computed-key-get-wrong-unpack-type.mimi");
    std::fs::write(&source, near_miss_text).expect("write wrong unpack-type near-miss fixture");
    for (flags, mode, expected_diagnostic) in [
        (vec!["--all", "--receipt"], "--all --receipt", Some("E0200")),
        (vec!["--all"], "--all without --receipt", None),
        (vec!["--receipt"], "--receipt without --all", None),
    ] {
        let output = Command::new(&mimi)
            .arg("mir")
            .args(flags)
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("run mimi mir {mode}: {error}"));
        assert!(
            !output.status.success(),
            "mimi mir {mode} unexpectedly materialized the incomplete TypeCatalog candidate"
        );
        if let Some(expected_diagnostic) = expected_diagnostic {
            assert_computed_key_get_checker_rejection(
                &output,
                &format!("ComputedKeyGetWrongUnpackType mimi mir {mode}"),
                expected_diagnostic,
            );
        }
        assert!(
            !text(&output.stdout).contains(CHECKER_INSPECTION_HEADER),
            "mimi mir {mode} emitted a Checker-only receipt for a rejected source"
        );
    }
    assert_computed_key_get_near_miss_fail_closed(
        &mimi,
        &source,
        "ComputedKeyGetWrongUnpackType",
        "E0200",
    );
}

#[test]
fn map_any_owner_v2_values_nonzero_index_near_miss_stays_fail_closed() {
    let mimi = mimi_bin();
    let fixture_text = std::fs::read_to_string(values_field_move_fixture())
        .expect("read exact ValuesFieldMove fixture");
    let near_miss_text = fixture_text.replace("snapshot[0] as Label", "snapshot[1] as Label");
    assert_ne!(
        fixture_text, near_miss_text,
        "the nonzero-index near-miss mutation must apply"
    );
    let directory = TemporaryDirectory::new();
    let source = directory.0.join("values-index-one.mimi");
    std::fs::write(&source, near_miss_text).expect("write nonzero-index near-miss fixture");

    assert_read_only_receipt(&mimi, &source);
    assert_cli_production_routes_fail_closed(&mimi, &source, "ValuesIndexOne");
}

#[test]
fn map_any_owner_v2_record_set_then_unsupported_keys_fails_all_cli_routes() {
    let mimi = mimi_bin();
    let source = incomplete_keys_fixture();

    // The Checker records the concrete-Record Set as a V2 candidate signal,
    // then withholds the V2 action receipt because `keys` has no V2 consumer.
    // Every public execution, build, and verification route must stop at that
    // coverage boundary without dispatching the source to legacy Map code.
    assert_cli_production_routes_fail_closed(&mimi, &source, "IncompleteKeys");
}

#[test]
fn map_any_owner_v2_keys_snapshot_uses_its_production_profile() {
    let mimi = mimi_bin();
    let source = keys_snapshot_fixture();
    assert_cli_keys_snapshot_routes(&mimi, &source);
}

fn assert_cli_keys_snapshot_routes(mimi: &Path, source: &Path) {
    const PROFILE: &str = "map-any-owner-v2-keys-snapshot-v1";
    const EXPECTED_OUTPUT: &str = "answer\n";

    for direct in [false, true] {
        let mode = if direct {
            "KeysSnapshot explicit MIR run"
        } else {
            "KeysSnapshot default run"
        };
        let mut command = Command::new(mimi);
        command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
        if direct {
            command.arg("--mir");
        }
        let run = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&run, mode);
        assert_eq!(text(&run.stdout), EXPECTED_OUTPUT, "{mode} output");
        let stderr = text(&run.stderr);
        assert!(
            stderr.contains(PROFILE),
            "{mode} lost its KeysSnapshot route identity: {stderr}"
        );
        assert_no_legacy_fallback(&stderr, mode);
    }

    let directory = TemporaryDirectory::new();
    for direct in [false, true] {
        let mode = if direct {
            "KeysSnapshot explicit MIR native build"
        } else {
            "KeysSnapshot default native build"
        };
        let binary = directory.0.join(if direct {
            "keys-snapshot-direct"
        } else {
            "keys-snapshot-default"
        });
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&build, mode);
        let stderr = text(&build.stderr);
        assert!(
            stderr.contains(PROFILE),
            "{mode} lost its KeysSnapshot route identity: {stderr}"
        );
        assert_no_legacy_fallback(&stderr, mode);
        assert!(binary.is_file(), "{mode} did not produce a native binary");

        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("run {mode} artifact: {error}"));
        assert_success(&native, "run KeysSnapshot native artifact");
        assert_eq!(
            text(&native.stdout),
            EXPECTED_OUTPUT,
            "{mode} native output"
        );
    }

    let mut shared_identity = None;
    for direct in [false, true] {
        let mode = if direct {
            "KeysSnapshot explicit MIR verify"
        } else {
            "KeysSnapshot default verify"
        };
        let mut command = Command::new(mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if direct {
            command.arg("--mir");
        }
        let verify = command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
        assert_success(&verify, mode);
        let identity = route_admission_identity_with_profile(
            &verify,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            mode,
            PROFILE,
        );
        assert_verifier_receipt(&verify, &identity, mode);
        if let Some(expected) = &shared_identity {
            assert_eq!(&identity, expected, "{mode} changed route identity");
        } else {
            shared_identity = Some(identity);
        }
    }
}
