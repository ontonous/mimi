use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RECEIPT_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];

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
    let fields = text(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some("cli-mir-v1"),
        "the CLI receipt schema changed"
    );
    for field in RECEIPT_FIELDS {
        let digest = fields
            .get(*field)
            .unwrap_or_else(|| panic!("MIR receipt omitted {field}"));
        assert!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "MIR receipt has malformed {field}: {digest}"
        );
    }
    assert!(
        fields.contains_key("root_owners"),
        "MIR receipt omitted its ownership root identity"
    );
    fields
}

fn assert_read_only_receipt(mimi: &Path, source: &Path) {
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
