//! Wave-1 audit-fix regression tests — scripts.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

// ---------------------------------------------------------------------------
// Full audit §13: CI/test script control-flow fixes (vacuous pass checks).
// The tests below assert syntax and exercise isolated helper fragments with
// temporary fixtures. They never run a full evidence gate or compile Actor
// fixtures, so script regressions can be checked without replaying the gate.
// ---------------------------------------------------------------------------

/// `bash -n <script>` must parse cleanly (parse-only, no execution).
fn assert_bash_syntax(script_rel_path: &str) {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(script_rel_path);
    assert!(
        path.is_file(),
        "{}: script missing at {}",
        script_rel_path,
        path.display()
    );
    let out = std::process::Command::new("bash")
        .arg("-n")
        .arg(&path)
        .output()
        .unwrap_or_else(|e| panic!("{}: failed to spawn `bash -n`: {}", script_rel_path, e));
    assert!(
        out.status.success(),
        "{}: `bash -n` failed (exit {:?})\n--- stderr ---\n{}",
        script_rel_path,
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn unique_legacy_owner_audit_temp_root(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{}-{sequence}", std::process::id()))
}

fn mirror_source_tree_except(
    source_root: &std::path::Path,
    source_directory: &std::path::Path,
    destination: &std::path::Path,
    excluded: &[&str],
) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source_directory)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let relative = source_path
            .strip_prefix(source_root)
            .expect("directory entry must be under its source tree");
        if entry.file_type()?.is_dir() {
            mirror_source_tree_except(source_root, &source_path, &destination_path, excluded)?;
        } else if !excluded
            .iter()
            .any(|path| std::path::Path::new(path) == relative)
        {
            if std::fs::hard_link(&source_path, &destination_path).is_err() {
                std::fs::copy(&source_path, &destination_path)?;
            }
        }
    }
    Ok(())
}

fn legacy_owner_temp_root_residues(prefix: &str) -> Vec<std::path::PathBuf> {
    let entries = std::fs::read_dir(std::env::temp_dir())
        .expect("scan temporary directory for owner audit residues");
    collect_legacy_owner_temp_root_residues(
        entries.map(|entry| entry.map(|entry| entry.path())),
        prefix,
    )
    .unwrap_or_else(|error| {
        panic!("scan temporary directory entry for owner audit residues: {error}")
    })
}

fn collect_legacy_owner_temp_root_residues<I, E>(
    entries: I,
    prefix: &str,
) -> Result<Vec<std::path::PathBuf>, String>
where
    I: IntoIterator<Item = Result<std::path::PathBuf, E>>,
    E: std::fmt::Display,
{
    let mut residues = entries
        .into_iter()
        .map(|entry| entry.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .map(|name| name.to_string_lossy().starts_with(prefix))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    residues.sort();
    Ok(residues)
}

fn assert_no_legacy_owner_temp_roots(prefix: &str) {
    no_legacy_owner_temp_root_residues(prefix).unwrap_or_else(|error| panic!("{error}"));
}

fn no_legacy_owner_temp_root_residues(prefix: &str) -> Result<(), String> {
    let residues = legacy_owner_temp_root_residues(prefix);
    if residues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "owner audit temp root residues found for {prefix}: {residues:?}"
        ))
    }
}

struct LegacyOwnerAuditTempRootCleanup(std::path::PathBuf);

impl LegacyOwnerAuditTempRootCleanup {
    fn cleanup_path(path: &std::path::Path) -> Result<(), String> {
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove owner audit temp root {}: {error}",
                path.display()
            )),
        }
    }
}

impl Drop for LegacyOwnerAuditTempRootCleanup {
    fn drop(&mut self) {
        let _ = Self::cleanup_path(&self.0);
    }
}

#[test]
fn script_syntax_test_ffi_contracts_sh() {
    // Fixed: `if [ $? -eq 0 ]` tested the *assignment* status (always 0),
    // not the binary's exit code; z3-pass now requires exit 0 + non-empty
    // output + an explicit success marker instead of absence-of-failure.
    assert_bash_syntax("scripts/test-ffi-contracts.sh");
}

#[test]
fn script_syntax_run_ci_matrix_sh() {
    // Fixed: two matrix cells ended with `; true`, forcing exit 0 so they
    // could never fail; they are now advisory cells with an ADVISORY counter.
    assert_bash_syntax("scripts/run-ci-matrix.sh");
}

#[test]
fn script_syntax_stress_test_sh() {
    // Fixed: top-level `local` (illegal outside a function) errored and,
    // under `set -e`, killed the script before most stress tests ran.
    assert_bash_syntax("scripts/stress-test.sh");
}

#[test]
fn script_syntax_mms_consistency_sh() {
    // Fixed: top-level `local` in the bootstrap-oracle block.
    assert_bash_syntax("scripts/mms-consistency.sh");
}

#[test]
fn script_syntax_verify_actor_two_fixture_current_sh() {
    // The current-source Actor replay runner must stay parseable as its gates evolve.
    assert_bash_syntax("scripts/verify-actor-two-fixture-current.sh");
}

#[test]
fn actor_census_gate_status_is_rendered_from_passed_boolean() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(root.join("scripts/verify-actor-two-fixture-current.sh"))
        .expect("read Actor current-source runner");
    assert!(
        script
            .lines()
            .any(|line| line.trim() == "gate_passed = gate.get(\"passed\")"),
        "Actor census status must use the report's boolean gate.passed field"
    );
    assert!(
        script.contains("f\"census_global_gate_status={census_gate_status}\""),
        "Actor census status output must use the boolean-derived status"
    );
    let status_expression = script
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("census_gate_status = "))
        .expect("find Actor census gate status mapping");
    for (gate_value, expected) in [
        ("True", "passed"),
        ("False", "failed"),
        ("None", "<missing>"),
    ] {
        let python =
            format!("gate_passed = {gate_value}\n{status_expression}\nprint(census_gate_status)\n");
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(python)
            .output()
            .expect("run Actor census gate status mapping");
        assert!(
            output.status.success(),
            "Python status mapping failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            expected,
            "unexpected census status for gate.passed={gate_value}"
        );
    }
}

struct ActorRunnerTempRoot(std::path::PathBuf);

impl ActorRunnerTempRoot {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "actor-current-script-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create Actor script test temp root");
        Self(path)
    }
}

impl Drop for ActorRunnerTempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn extract_actor_script_region(script: &str, start: &str, end: &str) -> String {
    let start_index = script
        .find(start)
        .unwrap_or_else(|| panic!("Actor runner source is missing marker: {start}"));
    let region = &script[start_index..];
    let end_index = region
        .find(end)
        .unwrap_or_else(|| panic!("Actor runner source is missing marker: {end}"));
    region[..end_index].to_owned()
}

fn actor_runner_host_target() -> String {
    let output = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("resolve native Rust host target for Actor runner tests");
    assert!(
        output.status.success(),
        "rustc -vV failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_owned))
        .expect("rustc -vV must report a host target")
}

#[test]
fn actor_runner_cargo_artifact_path_is_bound_to_isolated_host_target() {
    use std::path::Path;

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(root.join("scripts/verify-actor-two-fixture-current.sh"))
        .expect("read Actor current-source runner");
    assert!(
        script.contains("helper_path=\"$ROOT_DIR/scripts/actor_census_build_receipt.py\""),
        "Actor runner must delegate artifact path validation to actor_census_build_receipt.py"
    );
    assert!(
        script.contains("--target-dir \"$build_target_dir\"")
            && script.contains("--host-target \"$host_target\""),
        "Actor runner must bind receipt validation to the isolated build_target_dir and native host_target"
    );
    let scripts_dir = root.join("scripts");
    let temp = ActorRunnerTempRoot::new();
    let target_root = temp.0.join("isolated-target");
    let host = actor_runner_host_target();
    let make_executable = |path: &Path| {
        std::fs::create_dir_all(path.parent().expect("executable has a parent"))
            .expect("create executable directory");
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write test executable");
        let chmod = std::process::Command::new("chmod")
            .arg("+x")
            .arg(path)
            .output()
            .expect("make test executable executable");
        assert!(
            chmod.status.success(),
            "chmod failed for {}",
            path.display()
        );
    };
    let other_target = format!("{host}-actor-test-cross-target");
    let host_executable = target_root.join(&host).join("debug/mimi");
    let other_target_executable = target_root.join(&other_target).join("debug/mimi");
    let escaped_executable = temp.0.join("shared-target/debug/mimi");
    for path in [
        &host_executable,
        &other_target_executable,
        &escaped_executable,
    ] {
        make_executable(path);
    }
    let cargo_output = temp.0.join("cargo-output.jsonl");
    let path_file = temp.0.join("mimi.path");
    let python = r#"
import pathlib, sys
sys.path.insert(0, sys.argv[1])
import actor_census_build_receipt as helper
stdout = pathlib.Path(sys.argv[2]).read_bytes()
target_dir = pathlib.Path(sys.argv[3])
host_target = sys.argv[4]
path_out = pathlib.Path(sys.argv[5])
exe, _digest = helper._parse_mimi_artifact(stdout, target_dir, host_target)
path_out.write_text(f"{exe}\n", encoding="utf-8")
"#;
    let run_path_check = |executable: &Path| {
        let event = serde_json::json!({
            "reason": "compiler-artifact",
            "target": { "name": "mimi", "kind": ["bin"] },
            "executable": executable.to_string_lossy(),
        });
        std::fs::write(&cargo_output, format!("{event}\n"))
            .expect("write fake Cargo compiler-artifact event");
        std::process::Command::new("python3")
            .arg("-c")
            .arg(python)
            .arg(&scripts_dir)
            .arg(&cargo_output)
            .arg(&target_root)
            .arg(&host)
            .arg(&path_file)
            .output()
            .expect("run Cargo artifact path validation")
    };

    let valid = run_path_check(&host_executable);
    assert!(
        valid.status.success(),
        "native isolated Cargo artifact was rejected: {}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&path_file).expect("read accepted Cargo artifact path"),
        format!("{}\n", host_executable.canonicalize().unwrap().display())
    );

    let escaped = run_path_check(&escaped_executable);
    assert!(
        !escaped.status.success(),
        "shared-target artifact was accepted"
    );
    assert!(
        String::from_utf8_lossy(&escaped.stderr).contains("escaped the fresh isolated target"),
        "unexpected escaped-target error: {}",
        String::from_utf8_lossy(&escaped.stderr)
    );

    let wrong_target = run_path_check(&other_target_executable);
    assert!(
        !wrong_target.status.success(),
        "non-host target artifact was accepted"
    );
    assert!(
        String::from_utf8_lossy(&wrong_target.stderr)
            .contains("outside the forced host-target subtree"),
        "unexpected wrong-target error: {}",
        String::from_utf8_lossy(&wrong_target.stderr)
    );
}

#[test]
fn actor_runner_target_configuration_fails_closed_for_cross_targets() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(root.join("scripts/verify-actor-two-fixture-current.sh"))
        .expect("read Actor current-source runner");
    let function = extract_actor_script_region(
        &script,
        "validate_cargo_target_configuration() {\n",
        "\nsource_snapshot \"$ARTIFACT_DIR/source.inputs.before.nul\"",
    );
    let temp = ActorRunnerTempRoot::new();
    let host = actor_runner_host_target();
    let cross_target = format!("{host}-actor-test-cross-target");
    let run_configuration_check =
        |case: &str, env_target: Option<&str>, config_target: Option<(&str, bool)>| {
            let project = temp.0.join(case);
            let cargo_home = temp.0.join(format!("{case}-cargo-home"));
            std::fs::create_dir_all(&project).expect("create Cargo config test project");
            std::fs::create_dir_all(&cargo_home).expect("create isolated Cargo home");
            if let Some((target, in_cargo_home)) = config_target {
                let config_path = if in_cargo_home {
                    cargo_home.join("config.toml")
                } else {
                    let config_dir = project.join(".cargo");
                    std::fs::create_dir_all(&config_dir)
                        .expect("create project Cargo config directory");
                    config_dir.join("config.toml")
                };
                std::fs::write(config_path, format!("[build]\ntarget = {target:?}\n"))
                    .expect("write project Cargo target config");
            }
            let report = temp.0.join(format!("{case}.report"));
            let mut command = std::process::Command::new("bash");
            command
            .arg("-c")
            .arg(format!(
                "set -euo pipefail\n{function}\nvalidate_cargo_target_configuration \"$ACTOR_TEST_HOST\" \"$ACTOR_TEST_REPORT\"\n"
            ))
            .env("ROOT_DIR", &project)
            .env("CARGO_HOME", &cargo_home)
            .env("ACTOR_TEST_HOST", &host)
            .env("ACTOR_TEST_REPORT", &report)
            .env_remove("CARGO_BUILD_TARGET");
            if let Some(target) = env_target {
                command.env("CARGO_BUILD_TARGET", target);
            }
            command
                .output()
                .expect("run Cargo target configuration check")
        };

    let native = run_configuration_check("native", None, None);
    assert!(
        native.status.success(),
        "unset target settings should select the native host: {}",
        String::from_utf8_lossy(&native.stderr)
    );

    let environment_cross = run_configuration_check("environment-cross", Some(&cross_target), None);
    assert!(
        !environment_cross.status.success(),
        "cross-target CARGO_BUILD_TARGET was accepted"
    );
    assert!(
        std::fs::read_to_string(temp.0.join("environment-cross.report"))
            .expect("read environment-target rejection report")
            .contains("CARGO_BUILD_TARGET selects")
    );

    let config_cross = run_configuration_check("config-cross", None, Some((&cross_target, false)));
    assert!(
        !config_cross.status.success(),
        "cross-target .cargo/config.toml build.target was accepted"
    );
    assert!(std::fs::read_to_string(temp.0.join("config-cross.report"))
        .expect("read config-target rejection report")
        .contains("expected native host"));

    let cargo_home_config_cross =
        run_configuration_check("cargo-home-config-cross", None, Some((&cross_target, true)));
    assert!(
        !cargo_home_config_cross.status.success(),
        "cross-target CARGO_HOME/config.toml build.target was accepted"
    );
    assert!(
        std::fs::read_to_string(temp.0.join("cargo-home-config-cross.report"))
            .expect("read Cargo-home target rejection report")
            .contains("expected native host")
    );
}

#[test]
fn actor_runner_memcheck_mkdir_failure_counts_as_gate_failure() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(root.join("scripts/verify-actor-two-fixture-current.sh"))
        .expect("read Actor current-source runner");
    let function = extract_actor_script_region(
        &script,
        "require_l3_memcheck_reports() {\n",
        "\nvalidate_full_route_census() {",
    );
    let callsite_start = script
        .find("if ! require_l3_memcheck_reports; then\n")
        .expect("find Memcheck report gate callsite");
    let callsite_tail = &script[callsite_start..];
    let callsite_end = callsite_tail
        .find("\nfi")
        .map(|index| index + "\nfi".len())
        .expect("find Memcheck report gate callsite end");
    let callsite = &callsite_tail[..callsite_end];
    assert!(
        callsite.contains("GATE_FAILURES=$((GATE_FAILURES + 1))"),
        "Memcheck report failures must increment the gate failure count"
    );

    let temp = ActorRunnerTempRoot::new();
    let artifact_dir = temp.0.join("artifact");
    std::fs::create_dir_all(&artifact_dir).expect("create Memcheck artifact root");
    std::fs::write(
        artifact_dir.join("l3-memcheck"),
        b"block directory creation",
    )
    .expect("create path obstruction for Memcheck report directory");
    let harness = format!(
        "set -uo pipefail\nARTIFACT_DIR=\"$ACTOR_TEST_ARTIFACT_DIR\"\nGATE_FAILURES=0\n{function}\n{callsite}\nprintf 'GATE_FAILURES=%s\\n' \"$GATE_FAILURES\"\n"
    );
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("ACTOR_TEST_ARTIFACT_DIR", &artifact_dir)
        .output()
        .expect("run extracted Memcheck gate");
    assert!(
        output.status.success(),
        "Memcheck failure harness exited unexpectedly: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "GATE_FAILURES=1",
        "a failed Memcheck report directory setup must count against the gate"
    );
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("could not create Memcheck report directory"));
}

#[test]
fn actor_runner_census_schema_v9_is_required() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(root.join("scripts/verify-actor-two-fixture-current.sh"))
        .expect("read Actor current-source runner");
    let schema_check = extract_actor_script_region(
        &script,
        "    if value.get(\"schema_version\") != \"0.1.11-full-route-census-v9\":\n",
        "\n    if value.get(\"collection_mode\")",
    );
    for (schema, expected) in [
        ("0.1.11-full-route-census-v9", ""),
        (
            "0.1.11-full-route-census-v8",
            "strict full census report has an unexpected census schema version",
        ),
    ] {
        let report_schema = serde_json::to_string(schema).expect("encode test census schema");
        let python = format!(
            "value = {{\"schema_version\": {report_schema}}}\nlabel = \"strict full census report\"\nissues = []\n{check}\nprint('\\n'.join(issues))\n",
            check = schema_check
                .lines()
                .map(|line| line.strip_prefix("    ").unwrap_or(line))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(python)
            .output()
            .expect("run census schema validation fragment");
        assert!(
            output.status.success(),
            "schema validation fragment failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            expected,
            "unexpected census schema v9 decision for {schema}"
        );
    }
}

#[test]
fn legacy_owner_static_path_roster_is_executable_and_repeatable() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("scripts/audit-mir-legacy-owners-static.sh");
    let run = || {
        std::process::Command::new("bash")
            .arg(&path)
            .current_dir(&root)
            .output()
            .expect("legacy owner static inventory must execute")
    };
    let first = run();
    let second = run();
    for output in [&first, &second] {
        assert!(
            output.status.success(),
            "legacy owner audit failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let roster = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.starts_with("production_path_roster=")
                    || line.starts_with("reachable_legacy_ast_owner_count=")
                    || line.starts_with("reachable_checked_proof_compatibility_owner_count=")
                    || line.starts_with("checked_native_boundary_count=")
                    || line.starts_with("owner_retirement_status=")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let first_roster = roster(&first);
    assert_eq!(
        first_roster.len(),
        5,
        "legacy owner static inventory must expose the current four-path roster and retirement status"
    );
    assert_eq!(
        first_roster,
        roster(&second),
        "legacy owner path roster changed across repeated static inventories"
    );
    assert!(
        String::from_utf8_lossy(&first.stdout).contains("static_audit_status=ok"),
        "legacy owner static inventory omitted its static-only success marker"
    );
    assert!(
        !String::from_utf8_lossy(&first.stdout).contains("route_boundary_audit_status=passed"),
        "static inventory must never report the full dynamic audit as passed"
    );
}

#[test]
fn legacy_owner_audit_never_reports_skipped_dynamic_probes_as_success() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script_path = root.join("scripts/audit-mir-legacy-owners.sh");
    let output = std::process::Command::new("bash")
        .arg(&script_path)
        .env("MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES", "1")
        .current_dir(&root)
        .output()
        .expect("legacy owner audit must execute its skip guard");
    assert_eq!(
        output.status.code(),
        Some(2),
        "omitting direct owner probes must fail before an audit can report success: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("owner_audit_error=dynamic_probe_skip_is_forbidden"),
        "the skip refusal must be explicit: {stderr}"
    );
    assert!(
        !stdout.contains("route_boundary_audit_status=passed"),
        "a skipped dynamic audit must never emit a success marker: {stdout}"
    );

    let audit_source = std::fs::read_to_string(&script_path).expect("read audit source");
    for marker in [
        "DefaultBytecodeAstCompatibility",
        "NativeCheckedCompatibilityBoundary",
        "CheckedVerifierActorProofCompatibility",
        "FfiVerifierOnlyOwnerSlice",
        "printf 'classified_production_path_count=%s\\n' \"$owner_count\"",
        "MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES",
        "dynamic_probe_skip_is_forbidden",
        "owner_retirement_status=reachable-compatibility-surfaces-remain;route-and-boundary-evidence-only",
    ] {
        assert!(
            audit_source.contains(marker),
            "the audit must retain its exact owner classification and non-retirement status marker: {marker}"
        );
    }
    let static_inventory = std::process::Command::new("bash")
        .arg(root.join("scripts/audit-mir-legacy-owners-static.sh"))
        .current_dir(&root)
        .output()
        .expect("run independent static boundary inventory");
    assert!(
        static_inventory.status.success(),
        "static boundary inventory failed:\n{}",
        String::from_utf8_lossy(&static_inventory.stderr)
    );
    assert!(
        String::from_utf8_lossy(&static_inventory.stdout)
            .lines()
            .any(|line| line == "classified_production_path_count=4"),
        "static boundary inventory must emit the formatted current path count"
    );

    let direct_contracts = std::fs::read_to_string(root.join("tests/legacy_owner_boundary_api.rs"))
        .expect("read direct owner contracts");
    for marker in [
        "default_bytecode_compatibility_route_executes_its_named_legacy_disposition_without_artifacts",
        "bytecode_ast_compatibility_direct_api_rejects_dynamic_protocol_receivers",
        "native_checked_api_rejects_dynamic_protocol_receivers_without_main_artifact",
        "checked_verifier_actor_proof_compatibility_preserves_contracts_and_rejects_mir_without_artifacts",
        "checked_verifier_apis_reject_dynamic_protocol_receivers_without_proof_results",
        "ffi_verifier_only_owner_proves_and_defers_without_execution_or_file_artifacts",
        "default_and_explicit_routes_reject_dynamic_protocol_receiver_shapes_without_artifacts",
    ] {
        assert!(
            direct_contracts.contains(marker),
            "the owner census must retain a named direct route contract: {marker}"
        );
    }
}

#[test]
fn legacy_owner_closed_route_guard_drift_fail_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-context");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create context audit temp root");
    mirror_source_tree_except(
        &root.join("src"),
        &root.join("src"),
        &temp_root.join("src"),
        &["codegen/tests.rs"],
    )
    .expect("mirror source tree into context audit temp root");
    std::fs::copy(
        root.join("src/codegen/tests.rs"),
        temp_root.join("src/codegen/tests.rs"),
    )
    .expect("copy codegen tests into context audit temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into context audit temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, source_script.clone()).expect("write owner audit script");
    let tampered_owner_tripwire_script = source_script.replacen(
        "    compile_checked_routes_scalar_actor_through_canonical_mir_without_legacy_access \\\n",
        "    compile_checked_missing_scalar_actor_route_test \\\n",
        1,
    );
    assert!(
        source_script != tampered_owner_tripwire_script,
        "owner tripwire fixture must replace the actor fallback test name"
    );
    std::fs::write(&script_path, tampered_owner_tripwire_script)
        .expect("write tampered owner-class tripwire audit script");
    let owner_tripwire_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered owner-class tripwire audit");
    assert!(
        !owner_tripwire_output.status.success(),
        "missing actor fallback evidence must fail closed:\n{}",
        String::from_utf8_lossy(&owner_tripwire_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&owner_tripwire_output.stderr)
            .contains("owner_audit_error=missing_codegen_closed_route_tripwire class=actor-method"),
        "missing actor canonical route tripwire omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&owner_tripwire_output.stderr)
    );
    let tampered_closed_actor_route_script = source_script.replacen(
        "    compile_checked_rejects_contract_bearing_actor_without_legacy_owner \\\n",
        "    compile_checked_missing_contract_bearing_actor_route_test \\\n",
        1,
    );
    assert!(
        source_script != tampered_closed_actor_route_script,
        "closed-route fixture must replace the scalar Actor test name"
    );
    std::fs::write(&script_path, tampered_closed_actor_route_script)
        .expect("write tampered closed Actor route audit script");
    let closed_actor_route_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered closed Actor route audit");
    assert!(
        !closed_actor_route_output.status.success(),
        "missing closed ScalarActor route evidence must fail closed:\n{}",
        String::from_utf8_lossy(&closed_actor_route_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&closed_actor_route_output.stderr).contains(
            "owner_audit_error=missing_codegen_closed_route_tripwire class=actor-method-rejected"
        ),
        "missing rejected Actor route tripwire omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&closed_actor_route_output.stderr)
    );
    let tampered_map_tripwire_script = source_script.replacen(
        "    compile_checked_fails_closed_on_reachable_map_any_unpack \\\n",
        "    compile_checked_missing_map_any_native_gate_test \\\n",
        1,
    );
    assert!(
        source_script != tampered_map_tripwire_script,
        "closed-route fixture must replace the Map/Any owner test name"
    );
    std::fs::write(&script_path, tampered_map_tripwire_script)
        .expect("write tampered Map/Any closed-route tripwire audit script");
    let map_tripwire_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered Map/Any closed-route tripwire audit");
    assert!(
        !map_tripwire_output.status.success(),
        "missing Map/Any canonical route evidence must fail closed:\n{}",
        String::from_utf8_lossy(&map_tripwire_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&map_tripwire_output.stderr).contains(
            "owner_audit_error=missing_codegen_closed_route_tripwire class=map-any-native-hard-gate"
        ),
        "missing Map/Any direct native-codegen gate omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&map_tripwire_output.stderr)
    );
    std::fs::write(&script_path, &source_script)
        .expect("restore the Map/Any audit before mutating its direct test contract");
    let codegen_tests_path = temp_root.join("src/codegen/tests.rs");
    let codegen_tests_source =
        std::fs::read_to_string(&codegen_tests_path).expect("read mirrored codegen tests");
    let map_any_test_start = codegen_tests_source
        .find("fn compile_checked_fails_closed_on_reachable_map_any_unpack() {")
        .expect("direct Map/Any native hard-gate test exists");
    let map_any_test_end = codegen_tests_source[map_any_test_start..]
        .find("\n#[test]")
        .map(|offset| map_any_test_start + offset)
        .expect("Map/Any native hard-gate test has a following test boundary");
    let map_any_test_body = &codegen_tests_source[map_any_test_start..map_any_test_end];
    let tampered_map_any_test_body = map_any_test_body.replacen(
        "Some(crate::diagnostic::codes::E0722)",
        "Some(crate::diagnostic::codes::E0721)",
        1,
    );
    assert_ne!(
        map_any_test_body, tampered_map_any_test_body,
        "Map/Any source fixture must mutate its asserted native hard-gate diagnostic"
    );
    let tampered_codegen_tests = format!(
        "{}{}{}",
        &codegen_tests_source[..map_any_test_start],
        tampered_map_any_test_body,
        &codegen_tests_source[map_any_test_end..]
    );
    std::fs::write(&codegen_tests_path, tampered_codegen_tests)
        .expect("write codegen fixture with a changed Map/Any hard-gate assertion");
    let map_markers_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run static audit against a changed Map/Any hard-gate assertion");
    assert!(
        !map_markers_output.status.success(),
        "Map/Any native-codegen tripwire without its hard-error marker must fail closed:\n{}",
        String::from_utf8_lossy(&map_markers_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&map_markers_output.stderr).contains(
            "owner_audit_error=codegen_closed_route_tripwire_missing_marker class=map-any-native-hard-gate test=compile_checked_fails_closed_on_reachable_map_any_unpack marker=Some(crate::diagnostic::codes::E0722)"
        ),
        "Map/Any direct native hard-gate drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&map_markers_output.stderr)
    );
    let tampered_guard_script = source_script.replacen(
        "    'if let Some(canonical) = self.try_compile_exact_migrated_mir_island(program)?'",
        "    'if let Some(canonical) = self.missing_closed_route_guard(program)?'",
        1,
    );
    assert!(
        source_script != tampered_guard_script,
        "guard fixture must replace the expected route guard"
    );
    std::fs::write(&script_path, tampered_guard_script)
        .expect("write tampered route guard audit script");
    let guard_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered route guard audit");
    assert!(
        !guard_output.status.success(),
        "tampered closed route guard must fail closed:\n{}",
        String::from_utf8_lossy(&guard_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&guard_output.stderr)
            .contains("owner_audit_error=CodegenLegacyRemainder closed_route_guard_missing="),
        "route guard drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&guard_output.stderr)
    );
    let tampered_receipt_script =
        source_script.replacen("    scalar-ffi-v1\n", "    missing-route-receipt\n", 1);
    assert!(
        source_script != tampered_receipt_script,
        "receipt fixture must replace the expected scalar route label"
    );
    std::fs::write(&script_path, tampered_receipt_script)
        .expect("write tampered scalar receipt audit script");
    let receipt_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered scalar receipt audit");
    assert!(
        !receipt_output.status.success(),
        "tampered scalar route receipt must fail closed:\n{}",
        String::from_utf8_lossy(&receipt_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&receipt_output.stderr)
            .contains("owner_audit_error=scalar_route_receipt_missing="),
        "scalar receipt drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&receipt_output.stderr)
    );
    let tampered_consumer_script =
        source_script.replacen("    native-direct-v1\n", "    missing-native-receipt\n", 1);
    assert!(
        source_script != tampered_consumer_script,
        "consumer receipt fixture must replace the native direct label"
    );
    std::fs::write(&script_path, tampered_consumer_script)
        .expect("write tampered consumer receipt audit script");
    let consumer_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered consumer receipt audit");
    assert!(
        !consumer_output.status.success(),
        "tampered consumer receipt must fail closed:\n{}",
        String::from_utf8_lossy(&consumer_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&consumer_output.stderr)
            .contains("owner_audit_error=consumer_receipt_missing="),
        "consumer receipt drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&consumer_output.stderr)
    );
    let tampered_native_preflight_script =
        source_script.replacen("&?canonical, &receipt", "&?other_canonical, &receipt", 1);
    assert!(
        source_script != tampered_native_preflight_script,
        "native preflight fixture must replace the MIR identity in its receipt check"
    );
    std::fs::write(&script_path, tampered_native_preflight_script)
        .expect("write tampered native preflight audit script");
    let native_preflight_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered native preflight audit");
    assert!(
        !native_preflight_output.status.success(),
        "native preflight receipt bound to a different MIR must fail closed:\n{}",
        String::from_utf8_lossy(&native_preflight_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&native_preflight_output.stderr)
            .contains("owner_audit_error=native_direct_preflight_receipt_binding_missing="),
        "native preflight identity drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&native_preflight_output.stderr)
    );
    let tampered_provenance_script = source_script.replacen(
        "    'self.compile_mir_native_with_route_receipt(&canonical, &receipt)' \\\n",
        "    'self.missing_native_route_receipt(&canonical, &receipt)' \\\n",
        1,
    );
    assert!(
        source_script != tampered_provenance_script,
        "provenance fixture must replace the native receipt invocation"
    );
    std::fs::write(&script_path, tampered_provenance_script)
        .expect("write tampered provenance audit script");
    let provenance_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered provenance audit");
    assert!(
        !provenance_output.status.success(),
        "tampered receipt provenance must fail closed:\n{}",
        String::from_utf8_lossy(&provenance_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&provenance_output.stderr)
            .contains("owner_audit_error=consumer_receipt_invocation_missing="),
        "receipt provenance drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&provenance_output.stderr)
    );
    let tampered_profile_script = source_script.replacen(
        r#"    verify-ffi-v1 \
    ScalarFfi

# The verifier-only singleton-For exception"#,
        r#"    verify-ffi-v1 \
    ScalarCollection

# The verifier-only singleton-For exception"#,
        1,
    );
    assert!(
        source_script != tampered_profile_script,
        "profile fixture must replace the expected verifier profile"
    );
    std::fs::write(&script_path, tampered_profile_script)
        .expect("write tampered profile audit script");
    let profile_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered profile audit");
    assert!(
        !profile_output.status.success(),
        "tampered verifier route profile must fail closed:\n{}",
        String::from_utf8_lossy(&profile_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&profile_output.stderr)
            .contains("owner_audit_error=consumer_receipt_profile_missing="),
        "verifier profile drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&profile_output.stderr)
    );
    let tampered_source_hash_script = source_script.replacen(
        "    'blake3::hash(source.as_bytes()).to_hex().to_string(),'\n",
        "    'missing_source_hash(source.as_bytes()).to_string(),'\n",
        1,
    );
    assert!(
        source_script != tampered_source_hash_script,
        "source hash fixture must replace the expected hash expression"
    );
    std::fs::write(&script_path, tampered_source_hash_script)
        .expect("write tampered source hash audit script");
    let source_hash_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered source hash audit");
    assert!(
        !source_hash_output.status.success(),
        "tampered source hash entry must fail closed:\n{}",
        String::from_utf8_lossy(&source_hash_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&source_hash_output.stderr)
            .contains("owner_audit_error=source_hash_entry_hash_missing="),
        "source hash drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&source_hash_output.stderr)
    );
    let tampered_source_hash_parameter_script = source_script.replacen(
        "    'source_hash: String' \\\n",
        "    'source_hash_value: String' \\\n",
        1,
    );
    assert!(
        source_script != tampered_source_hash_parameter_script,
        "source hash parameter fixture must replace the expected declaration"
    );
    std::fs::write(&script_path, tampered_source_hash_parameter_script)
        .expect("write tampered source hash parameter audit script");
    let source_hash_parameter_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered source hash parameter audit");
    assert!(
        !source_hash_parameter_output.status.success(),
        "tampered source hash parameter must fail closed:\n{}",
        String::from_utf8_lossy(&source_hash_parameter_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&source_hash_parameter_output.stderr)
            .contains("owner_audit_error=source_hash_parameter_missing="),
        "source hash parameter drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&source_hash_parameter_output.stderr)
    );
    let tampered_mir_provenance_script = source_script.replacen(
        "    'validate_mir_result_provenance(&results, &receipt, &source_hash, \"verify_ffi_mir\")?'\n",
        "    'missing_mir_result_provenance(&results, &receipt, &source_hash, \"verify_ffi_mir\")?'\n",
        1,
    );
    assert!(
        source_script != tampered_mir_provenance_script,
        "MIR provenance fixture must replace the expected artifact validation"
    );
    std::fs::write(&script_path, tampered_mir_provenance_script)
        .expect("write tampered MIR provenance audit script");
    let mir_provenance_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered MIR provenance audit");
    assert!(
        !mir_provenance_output.status.success(),
        "tampered MIR provenance validation must fail closed:\n{}",
        String::from_utf8_lossy(&mir_provenance_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&mir_provenance_output.stderr)
            .contains("owner_audit_error=mir_verifier_source_provenance_missing="),
        "MIR provenance drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&mir_provenance_output.stderr)
    );
    let tampered_identity_script = source_script.replacen(
        "    'if artifact.mir_hash != receipt.mir_digest' \\\n",
        "    'if artifact.mir_hash == receipt.mir_digest' \\\n",
        1,
    );
    assert!(
        source_script != tampered_identity_script,
        "MIR identity fixture must replace the expected digest guard"
    );
    std::fs::write(&script_path, tampered_identity_script)
        .expect("write tampered MIR identity audit script");
    let identity_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered MIR identity audit");
    assert!(
        !identity_output.status.success(),
        "tampered MIR identity validation must fail closed:\n{}",
        String::from_utf8_lossy(&identity_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&identity_output.stderr)
            .contains("owner_audit_error=mir_result_identity_missing="),
        "MIR identity drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&identity_output.stderr)
    );
    let tampered_route_validation_script = source_script.replacen(
        "    if ! printf '%s\\n' \"$context\" | rg -F '.validate_against_program(program)' >/dev/null; then\n",
        "    if ! printf '%s\\n' \"$context\" | rg -F '.validate_without_program(program)' >/dev/null; then\n",
        1,
    );
    assert!(
        source_script != tampered_route_validation_script,
        "route validation fixture must replace the expected receipt guard"
    );
    std::fs::write(&script_path, tampered_route_validation_script)
        .expect("write tampered route validation audit script");
    let route_validation_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered route validation audit");
    assert!(
        !route_validation_output.status.success(),
        "tampered route receipt validation must fail closed:\n{}",
        String::from_utf8_lossy(&route_validation_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&route_validation_output.stderr)
            .contains("owner_audit_error=mir_route_receipt_validation_missing="),
        "route receipt validation drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&route_validation_output.stderr)
    );
    let tampered_manifest_replay_script = source_script.replacen(
        "    'verify_mir_with_route_receipt(program, &receipt, source_hash)'\n",
        "    'missing_mir_with_route_receipt(program, &receipt, source_hash)'\n",
        1,
    );
    assert!(
        source_script != tampered_manifest_replay_script,
        "manifest replay fixture must replace the direct MIR adapter"
    );
    std::fs::write(&script_path, tampered_manifest_replay_script)
        .expect("write tampered manifest replay audit script");
    let manifest_replay_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered manifest replay audit");
    assert!(
        !manifest_replay_output.status.success(),
        "tampered manifest replay adapter must fail closed:\n{}",
        String::from_utf8_lossy(&manifest_replay_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&manifest_replay_output.stderr)
            .contains("owner_audit_error=mir_route_manifest_replay_missing="),
        "manifest replay drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&manifest_replay_output.stderr)
    );
    let tampered_capability_order_script = source_script.replacen(
        r#"mir_ffi_capability_order_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_mir_results_with_route_receipt(' \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'validate_ffi_verifier_mir_capabilities(program)' \
    'mir::verify_ffi_program_with_route_receipt(program, source_hash, receipt)'"#,
        r#"mir_ffi_capability_order_binding \
    src/verifier/mod.rs \
    'fn verify_ffi_mir_results_with_route_receipt(' \
    'pub fn verify_ffi_mir_with_route_receipt(' \
    'missing_ffi_verifier_mir_capability_gate(program)' \
    'mir::verify_ffi_program_with_route_receipt(program, source_hash, receipt)'"#,
        1,
    );
    assert!(
        source_script != tampered_capability_order_script,
        "capability order fixture must replace the expected gate"
    );
    std::fs::write(&script_path, tampered_capability_order_script)
        .expect("write tampered capability order audit script");
    let capability_order_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered capability order audit");
    assert!(
        !capability_order_output.status.success(),
        "tampered capability order must fail closed:\n{}",
        String::from_utf8_lossy(&capability_order_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&capability_order_output.stderr)
            .contains("owner_audit_error=mir_ffi_capability_order_missing="),
        "capability order drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&capability_order_output.stderr)
    );
    let tampered_route_order_script = source_script.replacen(
        "    '.validate_against_program(program)' \\\n    'let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?'",
        "    'let results = verify_mir_results_with_route_receipt(program, source_hash.clone(), receipt)?' \\\n    '.validate_against_program(program)'",
        1,
    );
    assert!(
        source_script != tampered_route_order_script,
        "route order fixture must swap the expected receipt and adapter patterns"
    );
    std::fs::write(&script_path, tampered_route_order_script)
        .expect("write tampered route order audit script");
    let route_order_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered route order audit");
    assert!(
        !route_order_output.status.success(),
        "tampered route receipt order must fail closed:\n{}",
        String::from_utf8_lossy(&route_order_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&route_order_output.stderr)
            .contains("owner_audit_error=mir_route_receipt_order_drift="),
        "route receipt order drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&route_order_output.stderr)
    );
    let tampered_cli_route_script = source_script.replacen(
        "    'verify_mir_with_route_receipt(&canonical, &receipt, source_hash)?'",
        "    'missing_cli_verifier_route(&canonical, &receipt, source_hash)?'",
        1,
    );
    assert!(
        source_script != tampered_cli_route_script,
        "CLI route fixture must replace the receipt-bearing adapter"
    );
    std::fs::write(&script_path, tampered_cli_route_script)
        .expect("write tampered CLI route audit script");
    let cli_route_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered CLI route audit");
    assert!(
        !cli_route_output.status.success(),
        "tampered CLI verifier route must fail closed:\n{}",
        String::from_utf8_lossy(&cli_route_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&cli_route_output.stderr)
            .contains("owner_audit_error=mir_cli_verifier_route_missing="),
        "CLI route drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&cli_route_output.stderr)
    );
    let tampered_cli_boundary_script = source_script.replacen(
        "    'eprintln!(\"{}\", format_cli_error(&e));'",
        "    'eprintln!(\"{}\", e);'",
        1,
    );
    assert!(
        source_script != tampered_cli_boundary_script,
        "CLI error boundary fixture must replace the shared formatter"
    );
    std::fs::write(&script_path, tampered_cli_boundary_script)
        .expect("write tampered CLI error boundary audit script");
    let cli_boundary_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered CLI error boundary audit");
    assert!(
        !cli_boundary_output.status.success(),
        "tampered CLI error boundary must fail closed:\n{}",
        String::from_utf8_lossy(&cli_boundary_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&cli_boundary_output.stderr)
            .contains("owner_audit_error=mir_cli_error_boundary_missing="),
        "CLI error boundary drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&cli_boundary_output.stderr)
    );
    let tampered_cli_disasm_renderer_script = source_script.replacen(
        "    'crate::format_cli_error(&error.to_string())'",
        "    'crate::format_plain_error(&error.to_string())'",
        1,
    );
    assert!(
        source_script != tampered_cli_disasm_renderer_script,
        "disasm renderer fixture must replace the shared formatter"
    );
    std::fs::write(&script_path, tampered_cli_disasm_renderer_script)
        .expect("write tampered disasm renderer audit script");
    let disasm_renderer_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered disasm renderer audit");
    assert!(
        !disasm_renderer_output.status.success(),
        "tampered disasm renderer must fail closed:\n{}",
        String::from_utf8_lossy(&disasm_renderer_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&disasm_renderer_output.stderr)
            .contains("owner_audit_error=mir_cli_disasm_route_missing="),
        "disasm renderer drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&disasm_renderer_output.stderr)
    );
    let tampered_cli_disasm_rejection_script = source_script.replacen(
        "    'crate::format_cli_error(&message)'",
        "    'crate::format_plain_error(&message)'",
        1,
    );
    assert!(
        source_script != tampered_cli_disasm_rejection_script,
        "disasm rejection fixture must replace the shared formatter"
    );
    std::fs::write(&script_path, tampered_cli_disasm_rejection_script)
        .expect("write tampered disasm rejection audit script");
    let disasm_rejection_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered disasm rejection audit");
    assert!(
        !disasm_rejection_output.status.success(),
        "tampered disasm rejection must fail closed:\n{}",
        String::from_utf8_lossy(&disasm_rejection_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&disasm_rejection_output.stderr)
            .contains("owner_audit_error=mir_cli_disasm_route_missing="),
        "disasm rejection drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&disasm_rejection_output.stderr)
    );
    let tampered_cli_classifier_script = source_script.replacen(
        "    'canonical_mir_route_code_location_in_message(message)' \\\n    'runtime_system(\"mir.route\")'",
        "    'legacy_route_code_location(message)' \\\n    'runtime_system(\"mir.route\")'",
        1,
    );
    assert!(
        source_script != tampered_cli_classifier_script,
        "CLI classifier fixture must replace the registry classifier"
    );
    std::fs::write(&script_path, tampered_cli_classifier_script)
        .expect("write tampered CLI classifier audit script");
    let cli_classifier_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered CLI classifier audit");
    assert!(
        !cli_classifier_output.status.success(),
        "tampered CLI classifier must fail closed:\n{}",
        String::from_utf8_lossy(&cli_classifier_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&cli_classifier_output.stderr)
            .contains("owner_audit_error=mir_cli_route_formatter_provenance_missing="),
        "CLI classifier drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&cli_classifier_output.stderr)
    );
    let tampered_cli_origin_script = source_script.replacen(
        "    'runtime_system(\"mir.route\")'",
        "    'runtime_system(\"legacy.route\")'",
        1,
    );
    assert!(
        source_script != tampered_cli_origin_script,
        "CLI origin fixture must replace the mir.route provenance"
    );
    std::fs::write(&script_path, tampered_cli_origin_script)
        .expect("write tampered CLI origin audit script");
    let cli_origin_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered CLI origin audit");
    assert!(
        !cli_origin_output.status.success(),
        "tampered CLI origin must fail closed:\n{}",
        String::from_utf8_lossy(&cli_origin_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&cli_origin_output.stderr)
            .contains("owner_audit_error=mir_cli_route_formatter_provenance_missing="),
        "CLI origin drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&cli_origin_output.stderr)
    );
    std::fs::remove_dir_all(&temp_root).expect("remove context audit temp root");
    drop(cleanup);
    let restored = std::process::Command::new("bash")
        .arg(root.join("scripts/audit-mir-legacy-owners-static.sh"))
        .current_dir(&root)
        .output()
        .expect("rerun restored context audit");
    assert!(
        restored.status.success(),
        "restored owner static inventory failed after context probe:\n{}",
        String::from_utf8_lossy(&restored.stderr)
    );
}

#[test]
fn legacy_owner_evidence_tests_execute_as_lib_tests() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let llvm_config = std::process::Command::new("/tmp/llvm-wrapper/llvm-config")
        .arg("--version")
        .output()
        .expect("LLVM 18 wrapper must be installed for dynamic owner probes");
    assert!(
        llvm_config.status.success(),
        "LLVM wrapper version probe failed: {}",
        String::from_utf8_lossy(&llvm_config.stderr)
    );
    assert!(
        String::from_utf8_lossy(&llvm_config.stdout)
            .trim()
            .starts_with("18."),
        "owner probes require LLVM 18 wrapper, got {}",
        String::from_utf8_lossy(&llvm_config.stdout).trim()
    );
    let llvm_version = String::from_utf8_lossy(&llvm_config.stdout)
        .trim()
        .to_owned();
    let cargo_version = std::process::Command::new(&cargo)
        .arg("--version")
        .output()
        .expect("Cargo must be executable for dynamic owner probes");
    assert!(
        cargo_version.status.success(),
        "Cargo version probe failed: {}",
        String::from_utf8_lossy(&cargo_version.stderr)
    );
    let cargo_version = String::from_utf8_lossy(&cargo_version.stdout)
        .trim()
        .to_owned();
    assert!(
        cargo_version.starts_with("cargo "),
        "unexpected Cargo version output: {cargo_version}"
    );
    println!("legacy_owner_probe_llvm_version={llvm_version}");
    println!("legacy_owner_probe_cargo_version={cargo_version}");
    let mut first_round_results = Vec::new();
    for round in 1..=2 {
        for test_name in [
            "codegen::tests::compile_checked_routes_generic_ignored_bool_binding_projection_without_legacy_access",
            "codegen::tests::compile_checked_rejects_used_generic_integer_side_binding_without_legacy_owner",
            "codegen::tests::compile_checked_routes_generic_local_alias_projection_without_legacy_access",
            "codegen::tests::compile_checked_routes_exact_s8_flow_through_canonical_mir",
            "codegen::tests::compile_checked_routes_multi_transition_flow_through_canonical_mir",
            "codegen::tests::compile_checked_routes_scalar_actor_through_canonical_mir_without_legacy_access",
            "codegen::tests::compile_checked_rejects_contract_bearing_actor_without_legacy_owner",
            "codegen::tests::compile_checked_routes_concrete_protocol_method_without_legacy_access",
            "codegen::tests::compile_checked_rejects_dynamic_protocol_method_without_legacy_owner",
            "codegen::tests::compile_checked_rejects_trait_impl_specialization_without_legacy_owner",
            "codegen::tests::compile_checked_routes_captured_nested_helper_without_legacy_owner",
            "codegen::tests::compile_checked_routes_scalar_export_wrapper_without_legacy_access",
            "codegen::tests::compile_checked_fails_closed_on_reachable_map_any_unpack",
            "codegen::tests::compile_checked_rejects_map_any_borrowed_access_without_legacy_owner",
            "verifier::tests::typed_ffi_deferred_predicate_preserves_explicit_compatibility_obligation",
            "verifier::tests::nested_requires_in_expression_block_is_rejected_before_verifier_routing",
            "verifier::tests::typed_ffi_deferred_actual_argument_preserves_old_conditional_proof",
            "verifier::tests::typed_ffi_symbolic_string_contexts_defer_without_legacy_access",
        ] {
            let output = std::process::Command::new(&cargo)
                .args([
                    "test",
                    "--features",
                    "llvm18-host-dynamic",
                    "--lib",
                    test_name,
                    "--",
                    "--exact",
                    "--test-threads=1",
                ])
                .env("LLVM_SYS_181_PREFIX", "/tmp/llvm-wrapper")
                .env_remove("RUSTFLAGS")
                .env_remove("LD_PRELOAD")
                .current_dir(&root)
                .output()
                .unwrap_or_else(|error| {
                    panic!("failed to run owner evidence test {test_name} (round {round}): {error}")
                });
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "owner evidence test {test_name} (round {round}) failed:\n{stdout}\n{stderr}"
            );
            assert!(
                stdout.lines().any(|line| {
                    line.starts_with("test result: ok.")
                        && line.contains("1 passed;")
                        && line.contains("0 failed")
                }),
                "owner evidence test {test_name} (round {round}) must execute exactly one passing test:\n{stdout}"
            );
            let result_summary = stdout
                .lines()
                .find(|line| line.starts_with("test result: ok."))
                .expect("passing owner evidence test must emit a result summary")
                .split("; finished")
                .next()
                .expect("result summary must include a stable prefix")
                .to_owned();
            println!(
                "legacy_owner_probe_result round={round} test={test_name} summary={result_summary}"
            );
            if round == 1 {
                first_round_results.push((test_name, result_summary));
            } else {
                let (_, expected_summary) = first_round_results
                    .iter()
                    .find(|(name, _)| *name == test_name)
                    .expect("first-round owner evidence result must exist");
                assert_eq!(
                    &result_summary, expected_summary,
                    "owner evidence result drifted between repeated probes for {test_name}"
                );
            }
        }
    }
}

#[test]
fn legacy_owner_audit_pins_bytecode_route_receipt_provenance() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("scripts/audit-mir-legacy-owners-static.sh");
    let output = std::process::Command::new("bash")
        .arg(&path)
        .current_dir(&root)
        .output()
        .expect("legacy owner static inventory must execute");
    assert!(
        output.status.success(),
        "legacy owner audit failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let route_audit_source_path = root.join("scripts/audit-mir-legacy-owners.sh");
    let audit_source = std::fs::read_to_string(&route_audit_source_path)
        .expect("read legacy owner route audit source");
    assert!(
        audit_source.contains(
            r#"build_route_context="$(sed -n '/let (compile_result, canonical_default, route_receipt, route_consumer) = if mir {/,/^    let native_data_export_table_receipt =/p' "$build_source")"#
        ),
        "Observer V1 route audit must limit native receipt counts to the three compile branches, excluding later census labels"
    );
    assert!(
        stdout.contains(
            "native_build_route_receipt_binding=three-route-selections+exact-receipt+checked-native-adapter consumer=src/main/build.rs"
        ),
        "legacy owner audit must prove each native build branch passes its exact selected receipt to the checked adapter"
    );
    assert!(
        stdout.contains(
            "checked_route_profile_fallback_binding=scalar-export+actor+ffi+nested+generic-i32+i64+native-data-export+static-generic-trait+default consumer=src/main/canonical_dispatch.rs"
        ),
        "legacy owner audit must pin the ordered non-Map fallback profiles before the caller's default receipt"
    );
    assert!(
        stdout.contains(
            "bytecode_route_receipt_binding=compile_mir_program_inner(program, Some(receipt), validated_narrow_route)"
        ),
        "legacy owner audit must pin route receipt hand-off from the canonical bytecode entry"
    );
    assert!(
        stdout.contains(
            "bytecode_direct_print_route_adapter_binding=ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE->validator+typed-token+receipt+single-inner-call consumer=src/interp/bytecode/mir.rs::compile_mir_program_with_route_receipt("
        ),
        "legacy owner audit must pin the direct-print adapter's exact typed token and receipt hand-off"
    );
    assert!(
        stdout.contains(
            "bytecode_direct_route_receipt_binding=ffi-call-site->MIR_BYTECODE_DIRECT_ROUTE_PROFILE->compile_mir_program_inner(program, receipt.as_ref(), None) consumer=src/interp/bytecode/mir.rs::pub fn compile_mir_program("
        ),
        "legacy owner audit must pin direct bytecode receipt derivation"
    );
    assert!(
        stdout.contains(
            "bytecode_ordinary_map_owned_any_v2_route_binding=21-dedicated-adapters->ValidatedOrdinaryMapOwnedAnyV2Route-third-argument+exact-profile-guard consumer=src/interp/bytecode/mir.rs::compile_mir_program_inner("
        ) && audit_source.contains(
            r#"    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_producer_caller_route \
        pub\ fn\ compile_mir_program_with_values_crosscall_string_caller_route\( \
        validate_ordinary_map_owned_any_v2_values_producer_caller_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_PRODUCER_CALLER_PROFILE \
        ValuesProducerCaller \
        checker-validator"#
        ),
        "legacy owner audit must pin ValuesProducerCaller's typed route token and exact adapter boundary"
    );
    assert!(
        stdout.contains(
            "bytecode_result_crosscall_route_binding=V3-index0+V4-index1->separate-profile+validated-Checker-MIR-receipt+typed-token+VM-anchor consumer=src/interp/bytecode/mir.rs+src/interp/bytecode/vm.rs"
        ) && audit_source.contains(
            "validate_ordinary_map_owned_any_v2_values_result_crosscall_string_caller_route("
        ) && audit_source.contains(
            "ValuesResultCrosscallStringCallerIndex1"
        ) && audit_source.contains(
            "ValidatedOrdinaryMapOwnedAnyV2Route::${variant}(&receipt)"
        ),
        "legacy owner audit must pin the distinct Result Values caller V3/V4 bytecode adapters and route tokens"
    );
    assert!(
        audit_source.contains(
            r#"    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_record_unit_route \
        pub\ fn\ compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_route\( \
        validate_ordinary_map_owned_any_v2_record_unit_route \
        ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE \
        RecordUnit"#
        ),
        "legacy owner audit must pin RecordUnit to its Checker-aware bytecode adapter and exact profile"
    );
    let computed_string_key_owner_chain_adapter_binding = r#"    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_computed_string_key_owner_chain_route \
        pub\ fn\ compile_mir_program_with_dynamic_keys_get_route\( \
        validate_ordinary_map_owned_any_v2_computed_string_key_owner_chain_route \
        ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE \
        ComputedStringKeyOwnerChain"#;
    assert!(
        audit_source.contains(computed_string_key_owner_chain_adapter_binding),
        "legacy owner audit must pin ComputedStringKeyOwnerChain to its own receipt-backed bytecode adapter and exact profile"
    );
    let persistent_root_alias_replacement_adapter_binding = r#"    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route \
        pub\ fn\ compile_mir_program_with_empty_root_root_queries_v1_route\( \
        validate_ordinary_map_owned_any_v2_persistent_root_alias_replacement_route \
        ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE \
        PersistentRootAliasReplacement"#;
    assert!(
        audit_source.contains(persistent_root_alias_replacement_adapter_binding),
        "legacy owner audit must pin PersistentRootAliasReplacement to its own receipt-backed bytecode adapter and exact profile"
    );
    let crosscall_adapter_binding = r#"    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_crosscall_string_caller_route \
        pub\ fn\ compile_mir_program_with_values_crosscall_string_caller_index1_route\( \
        validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_PROFILE \
        ValuesCrosscallStringCaller \
        checker-validator"#;
    assert!(
        audit_source.contains(crosscall_adapter_binding),
        "legacy owner audit must pin ValuesCrosscallStringCaller's exact bounded adapter check"
    );
    let crosscall_index1_adapter_binding = r#"    bytecode_narrow_map_any_route_adapter \
        compile_mir_program_with_values_crosscall_string_caller_index1_route \
        pub\ fn\ compile_mir_program_with_map_root_producer_caller_route\( \
        validate_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1_route \
        ORDINARY_MAP_OWNED_ANY_V2_VALUES_CROSSCALL_STRING_CALLER_V2_PROFILE \
        ValuesCrosscallStringCallerIndex1 \
        checker-validator"#;
    assert!(
        audit_source.contains(crosscall_index1_adapter_binding),
        "legacy owner audit must pin ValuesCrosscallStringCaller Index1's distinct V2 adapter and profile"
    );
    assert!(
        stdout.contains(
            "values_crosscall_string_caller_route_binding=V1-index0+V2-index1->distinct-ordered-selector+exact-bytecode-token+Checker-MIR-verifier+CLI-profile-dispatch consumer=src/main/canonical_dispatch.rs+src/interp/bytecode/mir.rs+src/verifier/mod.rs+src/main/verify.rs"
        ),
        "legacy owner audit must pin index-zero V1 and index-one V2 selector, bytecode, and verifier bindings"
    );
    assert!(
        stdout.contains(
            "observer_v1_exact_route_binding=CanonicalMirRouteProfile::OrdinaryMapOwnerLifecycleObserverV1->ordinary-map-owner-lifecycle-observer-v1->ordered-default-selector+materialization-receipt-equality+checker-MIR-bytecode-native-verifier-preflight+same-bytecode-and-native-receipt-adapters consumer=src/main/canonical_dispatch.rs+src/main/build.rs+src/interp/bytecode/mir.rs+src/codegen/mir/eligibility.rs"
        ),
        "legacy owner audit must pin the Observer V1 profile from default selection through exact receipt transfer"
    );
    assert!(
        stdout.contains(
            "bytecode_route_receipt_binding=canonical_ffi_route_receipt: if !has_canonical_ffi_bindings"
        ),
        "legacy owner audit must pin the program-level route receipt anchor"
    );
    assert!(
        stdout.contains(
            "bytecode_route_receipt_binding=binding.route_receipt = Some(receipt.clone())"
        ),
        "legacy owner audit must pin receipt propagation into every FFI binding"
    );
    assert!(
        stdout.contains(
            "bytecode_route_manifest_adapter_binding=manifest-round-trip-to-typed-receipt consumer=src/interp/bytecode/mir.rs"
        ),
        "legacy owner audit must pin bytecode manifest adapter forwarding"
    );
    assert!(
        stdout.contains(
            "bytecode_route_receipt_vm_guard=program-anchor+identity-consistency+manifest-replay consumer=src/interp/bytecode/vm.rs"
        ),
        "legacy owner audit must pin the VM boundary receipt guard"
    );
    assert!(
        stdout.contains(
            "receiptless_bytecode_ffi=raw_compiler_reject+vm_whole_program_preflight+old_loader_removed"
        ),
        "legacy owner audit must pin receiptless bytecode FFI retirement"
    );
    assert!(
        stdout.contains(
            "bytecode_vm_ffi_preflight_order=canonical-receipts+ffi-descriptors-before-actor-root;worker-and-normal-host-symbol-preflight-before-first-instruction"
        ),
        "legacy owner audit must pin whole-program FFI receipt preflight before Actor setup and host symbol resolution"
    );
    assert!(
        stdout.contains(
            "native_route_receipt_anchor=program-anchor+semantic-replay consumer=src/codegen/mir/eligibility.rs"
        ),
        "legacy owner audit must pin the native program-level route receipt anchor"
    );
    assert!(
        stdout.contains(
            "mir_route_cache_identity_binding=shared-receipt-digest consumer=src/verifier/ctx.rs"
        ),
        "legacy owner audit must pin the shared route cache identity algorithm"
    );
    assert!(
        stdout.contains(
            "native_route_manifest_replay_binding=manifest-parse+identity-check consumer=src/codegen/mir/eligibility.rs"
        ),
        "legacy owner audit must pin native route manifest replay"
    );
    assert!(
        stdout.contains(
            "native_route_manifest_adapter_binding=manifest-to-typed-receipt-forward consumer=src/codegen/mir/eligibility.rs"
        ),
        "legacy owner audit must pin native manifest adapter forwarding"
    );
}

#[test]
fn legacy_owner_audit_rejects_map_any_route_regressions() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-map-any-route");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts"))
        .expect("create Map/Any route audit temp root");
    let excluded = [
        "verifier/mod.rs",
        "interp/bytecode/mir.rs",
        "main/canonical_dispatch.rs",
        "codegen/mir/eligibility.rs",
    ];
    mirror_source_tree_except(
        &root.join("src"),
        &root.join("src"),
        &temp_root.join("src"),
        &excluded,
    )
    .expect("mirror source tree for isolated route mutations");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into Map/Any route audit temp root");
    std::fs::copy(
        root.join("scripts/audit-mir-legacy-owners.sh"),
        temp_root.join("scripts/audit-mir-legacy-owners.sh"),
    )
    .expect("copy legacy owner audit script");
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");

    let verifier_path = temp_root.join("src/verifier/mod.rs");
    std::fs::copy(root.join("src/verifier/mod.rs"), &verifier_path)
        .expect("copy verifier source before route mutations");
    let bytecode_path = temp_root.join("src/interp/bytecode/mir.rs");
    std::fs::copy(root.join("src/interp/bytecode/mir.rs"), &bytecode_path)
        .expect("copy bytecode adapter source before route mutations");
    let native_path = temp_root.join("src/codegen/mir/eligibility.rs");
    std::fs::copy(root.join("src/codegen/mir/eligibility.rs"), &native_path)
        .expect("copy native adapter source before route mutations");

    let dispatcher_path = temp_root.join("src/main/canonical_dispatch.rs");
    std::fs::copy(
        root.join("src/main/canonical_dispatch.rs"),
        &dispatcher_path,
    )
    .expect("copy default dispatcher source for Observer V1 route mutation");
    let dispatcher_source =
        std::fs::read_to_string(&dispatcher_path).expect("read copied default dispatcher source");
    let baseline_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run legacy owner audit against the clean source snapshot");
    assert!(
        baseline_output.status.success(),
        "the unmodified legacy owner audit must pass before route mutations:\n{}\n{}",
        String::from_utf8_lossy(&baseline_output.stdout),
        String::from_utf8_lossy(&baseline_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&baseline_output.stdout).contains("static_audit_status=ok"),
        "the clean-source static inventory must report its static-only success marker"
    );
    assert!(
        !String::from_utf8_lossy(&baseline_output.stdout)
            .contains("route_boundary_audit_status=passed"),
        "a synthetic source snapshot must not claim the dynamic boundary audit passed"
    );
    let wrong_observer_profile = dispatcher_source.replacen(
        "mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE",
        "mimi::core::mir::ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND",
        1,
    );
    assert!(
        dispatcher_source != wrong_observer_profile,
        "Observer V1 profile mutation fixture must replace the selected main-route profile"
    );
    std::fs::write(&dispatcher_path, wrong_observer_profile)
        .expect("write dispatcher source with a mismatched Observer V1 profile");
    let dispatcher_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a mismatched Observer V1 main-route profile");
    assert!(
        !dispatcher_output.status.success(),
        "a default selector using the wrong Observer V1 receipt profile must fail closed:\n{}",
        String::from_utf8_lossy(&dispatcher_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&dispatcher_output.stderr)
            .contains("owner_audit_error=observer_v1_exact_route_binding_missing="),
        "Observer V1 profile drift omitted the exact-route diagnostic:\n{}",
        String::from_utf8_lossy(&dispatcher_output.stderr)
    );
    std::fs::write(&dispatcher_path, &dispatcher_source)
        .expect("restore dispatcher source after Observer V1 profile mutation");

    let swapped_values_crosscall_index1_profile = dispatcher_source.replacen(
        "                                        OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\n                                        .as_str(),",
        "                                        OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\n                                        .as_str(),",
        1,
    );
    assert_ne!(
        dispatcher_source, swapped_values_crosscall_index1_profile,
        "Values Index1 mutation fixture must replace the default selector's exact V2 profile"
    );
    std::fs::write(&dispatcher_path, swapped_values_crosscall_index1_profile)
        .expect("write dispatcher source with V1 profile on the index-one route");
    let swapped_values_crosscall_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a V1 profile on the index-one route");
    assert!(
        !swapped_values_crosscall_output.status.success(),
        "the default selector must not authorize the index-one Values route with the V1 profile:\n{}",
        String::from_utf8_lossy(&swapped_values_crosscall_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&swapped_values_crosscall_output.stderr)
            .contains("owner_audit_error=values_crosscall_string_caller_selector_binding_missing="),
        "Values Index1 profile drift omitted the exact-route diagnostic:\n{}",
        String::from_utf8_lossy(&swapped_values_crosscall_output.stderr)
    );
    std::fs::write(&dispatcher_path, &dispatcher_source)
        .expect("restore dispatcher source after Values Index1 profile mutation");

    let zero_miss_route = "    if admission.ordinary_map_owned_any_v2_zero_miss_i32() {\n        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ZERO_MISS_I32_PROFILE;\n    }\n";
    let keys_snapshot_route = "    if admission.ordinary_map_owned_any_v2\n        == mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::KeysSnapshot\n    {\n        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_KEYS_SNAPSHOT_PROFILE;\n    }\n";
    let dynamic_keys_get_route = "    if admission.ordinary_map_owned_any_v2_dynamic_keys_get() {\n        return mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_DYNAMIC_KEYS_GET_PROFILE;\n    }\n";
    let values_producer_caller_route = "    if admission.ordinary_map_owned_any_v2_values_producer_caller() {\n        return mimi::core::mir::CanonicalMirRouteProfile::\n            OrdinaryMapOwnedAnyV2ValuesProducerCaller\n            .as_str();\n    }\n";
    let values_crosscall_string_caller_index1_route = "    if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_index1() {\n        return mimi::core::mir::CanonicalMirRouteProfile::\n            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1\n            .as_str();\n    }\n";
    let values_crosscall_string_caller_route = "    if admission.ordinary_map_owned_any_v2_values_crosscall_string_caller_v1() {\n        return mimi::core::mir::CanonicalMirRouteProfile::\n            OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller\n            .as_str();\n    }\n";
    let map_root_get_any_caller_route = "    if admission.ordinary_map_owned_any_v2_map_root_get_any_caller() {\n        return mimi::core::mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller\n            .as_str();\n    }\n";
    let map_root_producer_caller_route = "    if admission.ordinary_map_owned_any_v2_map_root_producer_caller() {\n        return mimi::core::mir::CanonicalMirRouteProfile::\n            OrdinaryMapOwnedAnyV2MapRootProducerCaller\n            .as_str();\n    }\n";
    let early_route_order = format!(
        "{zero_miss_route}{keys_snapshot_route}{dynamic_keys_get_route}{values_crosscall_string_caller_index1_route}{values_crosscall_string_caller_route}{values_producer_caller_route}{map_root_get_any_caller_route}{map_root_producer_caller_route}"
    );
    let early_route_reorders = [
        (
            "zero-miss and ValuesProducerCaller",
            format!(
                "{keys_snapshot_route}{dynamic_keys_get_route}{values_crosscall_string_caller_index1_route}{values_crosscall_string_caller_route}{values_producer_caller_route}{zero_miss_route}{map_root_get_any_caller_route}{map_root_producer_caller_route}"
            ),
        ),
        (
            "ValuesCrosscallStringCallerIndex1 and ValuesCrosscallStringCaller",
            format!(
                "{zero_miss_route}{keys_snapshot_route}{dynamic_keys_get_route}{values_crosscall_string_caller_route}{values_crosscall_string_caller_index1_route}{values_producer_caller_route}{map_root_get_any_caller_route}{map_root_producer_caller_route}"
            ),
        ),
        (
            "ValuesCrosscallStringCaller and ValuesProducerCaller",
            format!(
                "{zero_miss_route}{keys_snapshot_route}{dynamic_keys_get_route}{values_crosscall_string_caller_index1_route}{values_producer_caller_route}{values_crosscall_string_caller_route}{map_root_get_any_caller_route}{map_root_producer_caller_route}"
            ),
        ),
        (
            "MapRootGetAnyCaller and MapRootProducerCaller",
            format!(
                "{zero_miss_route}{keys_snapshot_route}{dynamic_keys_get_route}{values_crosscall_string_caller_index1_route}{values_crosscall_string_caller_route}{values_producer_caller_route}{map_root_producer_caller_route}{map_root_get_any_caller_route}"
            ),
        ),
    ];
    for (order_name, reordered_early_route_order) in early_route_reorders {
        let reordered_early_routes =
            dispatcher_source.replacen(&early_route_order, &reordered_early_route_order, 1);
        assert_ne!(
            dispatcher_source, reordered_early_routes,
            "Observer V1 route-order fixture must swap {order_name}"
        );
        std::fs::write(&dispatcher_path, reordered_early_routes)
            .expect("write dispatcher source with reordered early V2 routes");
        let early_order_output = std::process::Command::new("bash")
            .arg(&script_path)
            .arg("--static-only")
            .current_dir(&temp_root)
            .output()
            .expect("run audit against reordered early V2 routes");
        assert!(
            !early_order_output.status.success(),
            "the default selector must preserve the complete early V2 route order ({order_name}):\n{}",
            String::from_utf8_lossy(&early_order_output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&early_order_output.stderr)
                .contains("owner_audit_error=observer_v1_exact_route_binding_missing="),
            "early V2 route order drift ({order_name}) omitted the exact-route diagnostic:\n{}",
            String::from_utf8_lossy(&early_order_output.stderr)
        );
        std::fs::write(&dispatcher_path, &dispatcher_source)
            .expect("restore dispatcher source after early V2 route-order mutation");
    }

    let map_any_fallbacks = "    } else if admission.map_any_owner_complete() {\n        mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND\n    } else if admission.ordinary_map_owned_any_v2_complete() {\n        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND\n";
    let reversed_map_any_fallbacks = "    } else if admission.ordinary_map_owned_any_v2_complete() {\n        mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND\n    } else if admission.map_any_owner_complete() {\n        mimi::core::mir::MAP_ANY_OWNER_V1_ISLAND\n";
    let reordered_fallbacks =
        dispatcher_source.replacen(map_any_fallbacks, reversed_map_any_fallbacks, 1);
    assert_ne!(
        dispatcher_source, reordered_fallbacks,
        "Observer V1 route-order fixture must swap the broad owner fallbacks"
    );
    std::fs::write(&dispatcher_path, reordered_fallbacks)
        .expect("write dispatcher source with reordered owner fallbacks");
    let fallback_order_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against reordered owner fallbacks");
    assert!(
        !fallback_order_output.status.success(),
        "the default selector must preserve the complete owner fallback order:\n{}",
        String::from_utf8_lossy(&fallback_order_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&fallback_order_output.stderr)
            .contains("owner_audit_error=observer_v1_exact_route_binding_missing="),
        "owner fallback order drift omitted the exact-route diagnostic:\n{}",
        String::from_utf8_lossy(&fallback_order_output.stderr)
    );
    std::fs::write(&dispatcher_path, &dispatcher_source)
        .expect("restore dispatcher source after fallback-order mutation");

    let verifier_source =
        std::fs::read_to_string(&verifier_path).expect("read copied verifier source");
    let missing_map_empty_selector = verifier_source.replacen(
        "validate_map_empty_root_root_queries_v1_mir_route(program)",
        "validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(program)",
        1,
    );
    assert!(
        verifier_source != missing_map_empty_selector,
        "verifier mutation fixture must remove the Map Empty Root selector"
    );
    std::fs::write(&verifier_path, missing_map_empty_selector)
        .expect("write verifier source without the Map Empty Root selector");
    let map_empty_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a missing Map Empty Root selector");
    assert!(
        !map_empty_output.status.success(),
        "a verifier omitting the Map Empty Root route must fail closed:\n{}",
        String::from_utf8_lossy(&map_empty_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&map_empty_output.stderr)
            .contains("owner_audit_error=verifier_default_entry_profile_selector_missing="),
        "Map Empty Root selector drift omitted the ordered-selector diagnostic:\n{}",
        String::from_utf8_lossy(&map_empty_output.stderr)
    );
    std::fs::write(&verifier_path, &verifier_source)
        .expect("restore verifier source after Map Empty Root selector mutation");

    let missing_zero_miss_selector = verifier_source.replacen(
        "validate_ordinary_map_owned_any_v2_zero_miss_i32_mir_route(\n            program,\n        )",
        "validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(program)",
        1,
    );
    assert!(
        verifier_source != missing_zero_miss_selector,
        "verifier mutation fixture must remove the ZeroMissI32 selector"
    );
    std::fs::write(&verifier_path, missing_zero_miss_selector)
        .expect("write verifier source without the ZeroMissI32 selector");
    let zero_miss_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a missing ZeroMissI32 selector");
    assert!(
        !zero_miss_output.status.success(),
        "a verifier omitting the ZeroMissI32 route must fail closed:\n{}",
        String::from_utf8_lossy(&zero_miss_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&zero_miss_output.stderr)
            .contains("owner_audit_error=verifier_default_entry_profile_selector_missing="),
        "ZeroMissI32 selector drift omitted the ordered-selector diagnostic:\n{}",
        String::from_utf8_lossy(&zero_miss_output.stderr)
    );
    std::fs::write(&verifier_path, &verifier_source)
        .expect("restore verifier source after ZeroMissI32 selector mutation");

    let wrong_observer_validator = verifier_source.replacen(
        "validate_ordinary_map_owner_lifecycle_observer_v1_mir_route(program)",
        "validate_ordinary_map_owned_any_v2_direct_print_mir_route(program)",
        1,
    );
    assert!(
        verifier_source != wrong_observer_validator,
        "verifier mutation fixture must replace the Observer V1 selector validator"
    );
    std::fs::write(&verifier_path, wrong_observer_validator)
        .expect("write verifier source with a regressed Observer V1 selector");
    let observer_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a regressed Observer V1 selector");
    assert!(
        !observer_output.status.success(),
        "a verifier pairing the Observer V1 receipt with the wrong validator must fail closed:\n{}",
        String::from_utf8_lossy(&observer_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&observer_output.stderr)
            .contains("owner_audit_error=verifier_default_entry_profile_selector_missing="),
        "Observer V1 selector drift omitted the ordered-selector diagnostic:\n{}",
        String::from_utf8_lossy(&observer_output.stderr)
    );

    std::fs::write(&verifier_path, &verifier_source)
        .expect("restore verifier source after Observer V1 selector mutation");
    let wrong_direct_print_profile = verifier_source.replacen(
        "ORDINARY_MAP_OWNED_ANY_V2_DIRECT_PRINT_PROFILE",
        "ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE",
        1,
    );
    assert!(
        verifier_source != wrong_direct_print_profile,
        "verifier mutation fixture must replace the direct-print selector profile"
    );
    std::fs::write(&verifier_path, wrong_direct_print_profile)
        .expect("write verifier source with a regressed direct-print profile");
    let direct_print_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a regressed direct-print profile selector");
    assert!(
        !direct_print_output.status.success(),
        "a verifier selecting the wrong direct-print profile must fail closed:\n{}",
        String::from_utf8_lossy(&direct_print_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&direct_print_output.stderr)
            .contains("owner_audit_error=verifier_default_entry_profile_selector_missing="),
        "direct-print profile drift omitted the ordered-selector diagnostic:\n{}",
        String::from_utf8_lossy(&direct_print_output.stderr)
    );

    let wrong_verifier_profile = verifier_source.replacen(
        "ORDINARY_MAP_OWNED_ANY_V2_COMPUTED_KEY_GET_PROFILE",
        "ORDINARY_MAP_OWNED_ANY_V2_VALUES_FIELD_MOVE_PROFILE",
        1,
    );
    assert!(
        verifier_source != wrong_verifier_profile,
        "verifier mutation fixture must replace the computed-key selector profile"
    );
    std::fs::write(&verifier_path, wrong_verifier_profile)
        .expect("write verifier source with a regressed narrow profile");
    let verifier_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against regressed verifier profile selector");
    assert!(
        !verifier_output.status.success(),
        "a verifier selecting the wrong narrow Map/Any profile must fail closed:\n{}",
        String::from_utf8_lossy(&verifier_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&verifier_output.stderr)
            .contains("owner_audit_error=verifier_default_entry_profile_selector_missing="),
        "wrong verifier profile expectation omitted the selector diagnostic:\n{}",
        String::from_utf8_lossy(&verifier_output.stderr)
    );

    std::fs::write(&verifier_path, &verifier_source)
        .expect("restore verifier source after route mutation");
    let wrong_selector_order = verifier_source.replacen(
        "validate_ordinary_map_owned_any_v2_values_mixed_snapshot_remove_mir_route",
        "validate_ordinary_map_owned_any_v2_values_mixed_snapshot_mir_route",
        1,
    );
    assert!(
        verifier_source != wrong_selector_order,
        "selector-order fixture must replace the second narrow route branch"
    );
    std::fs::write(&verifier_path, wrong_selector_order)
        .expect("write verifier source with a missing ordered narrow branch");
    let order_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a reordered verifier selector");
    assert!(
        !order_output.status.success(),
        "a verifier selector missing the required ordered narrow branch must fail closed:\n{}",
        String::from_utf8_lossy(&order_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&order_output.stderr)
            .contains("owner_audit_error=verifier_default_entry_profile_selector_missing="),
        "selector order drift omitted the fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&order_output.stderr)
    );

    std::fs::write(&verifier_path, &verifier_source)
        .expect("restore verifier source after selector-order mutation");
    let wrong_default_route =
        verifier_source.replacen("\"verifier-mir-v1\"", "\"verifier-mir-v2\"", 1);
    assert!(
        verifier_source != wrong_default_route,
        "default-route fixture must replace the verifier fallback profile"
    );
    std::fs::write(&verifier_path, wrong_default_route)
        .expect("write verifier source with a regressed default route");
    let default_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a regressed verifier default route");
    assert!(
        !default_output.status.success(),
        "a verifier selector with the wrong default route must fail closed:\n{}",
        String::from_utf8_lossy(&default_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&default_output.stderr)
            .contains("owner_audit_error=verifier_default_entry_profile_selector_missing="),
        "default route drift omitted the fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&default_output.stderr)
    );

    std::fs::write(&verifier_path, &verifier_source)
        .expect("restore verifier source after default-route mutation");
    let bytecode_source =
        std::fs::read_to_string(&bytecode_path).expect("read copied bytecode adapter source");
    let wrong_bytecode_token = bytecode_source.replacen(
        "Some(ValidatedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet(",
        "Some(ValidatedOrdinaryMapOwnedAnyV2Route::ValuesFieldMove(",
        1,
    );
    assert!(
        bytecode_source != wrong_bytecode_token,
        "bytecode mutation fixture must replace the computed-key route token"
    );
    std::fs::write(&bytecode_path, wrong_bytecode_token)
        .expect("write bytecode adapter source with a regressed route token");
    let bytecode_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against regressed bytecode route token");
    assert!(
        !bytecode_output.status.success(),
        "a bytecode adapter receiving the wrong narrow route token must fail closed:\n{}",
        String::from_utf8_lossy(&bytecode_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&bytecode_output.stderr)
            .contains("owner_audit_error=bytecode_narrow_map_any_adapter_token_missing="),
        "wrong bytecode token expectation omitted the adapter diagnostic:\n{}",
        String::from_utf8_lossy(&bytecode_output.stderr)
    );

    let raw_receipt_as_token = bytecode_source.replacen(
        "Some(ValidatedOrdinaryMapOwnedAnyV2Route::ComputedKeyGet(\n            &receipt,\n        )),",
        "Some(&receipt),",
        1,
    );
    assert!(
        bytecode_source != raw_receipt_as_token,
        "bytecode token fixture must replace the typed route token with a raw receipt"
    );
    std::fs::write(&bytecode_path, raw_receipt_as_token)
        .expect("write bytecode adapter source with a raw receipt in the token slot");
    let raw_receipt_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a bytecode adapter receiving a raw receipt token");
    assert!(
        !raw_receipt_output.status.success(),
        "a raw route receipt cannot replace the typed bytecode route token:\n{}",
        String::from_utf8_lossy(&raw_receipt_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&raw_receipt_output.stderr)
            .contains("owner_audit_error=bytecode_narrow_map_any_adapter_token_missing="),
        "raw receipt token drift omitted the fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&raw_receipt_output.stderr)
    );

    let wrong_is_empty_token = bytecode_source.replacen(
        "Some(ValidatedOrdinaryMapOwnedAnyV2Route::IsEmpty(&receipt)),",
        "Some(ValidatedOrdinaryMapOwnedAnyV2Route::CloneIsolation(&receipt)),",
        1,
    );
    assert!(
        bytecode_source != wrong_is_empty_token,
        "IsEmpty mutation fixture must replace its typed route token"
    );
    std::fs::write(&bytecode_path, wrong_is_empty_token)
        .expect("write IsEmpty adapter source with a CloneIsolation route token");
    let is_empty_token_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against an IsEmpty adapter with a CloneIsolation token");
    assert!(
        !is_empty_token_output.status.success(),
        "the IsEmpty adapter must reject a CloneIsolation typed route token:\n{}",
        String::from_utf8_lossy(&is_empty_token_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&is_empty_token_output.stderr)
            .contains("owner_audit_error=bytecode_narrow_map_any_adapter_token_missing="),
        "wrong IsEmpty token omitted the fail-closed adapter diagnostic:\n{}",
        String::from_utf8_lossy(&is_empty_token_output.stderr)
    );

    let wrong_get_or_default_token = bytecode_source.replacen(
        "Some(ValidatedOrdinaryMapOwnedAnyV2Route::GetOrDefaultI32(\n            receipt,\n        )),",
        "Some(ValidatedOrdinaryMapOwnedAnyV2Route::ZeroMissI32(&receipt)),",
        1,
    );
    assert_ne!(
        bytecode_source, wrong_get_or_default_token,
        "GetOrDefaultI32 mutation fixture must replace its exact typed route token"
    );
    std::fs::write(&bytecode_path, wrong_get_or_default_token)
        .expect("write GetOrDefaultI32 adapter source with a ZeroMissI32 route token");
    let get_or_default_token_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a mismatched GetOrDefaultI32 route token");
    let get_or_default_token_stderr = String::from_utf8_lossy(&get_or_default_token_output.stderr);
    assert!(
        !get_or_default_token_output.status.success()
            && get_or_default_token_stderr
                .contains("owner_audit_error=bytecode_narrow_map_any_adapter_token_missing=")
            && get_or_default_token_stderr.contains(
                "compile_mir_program_with_ordinary_map_owned_any_v2_get_or_default_i32_route"
            ),
        "a GetOrDefaultI32 adapter with another route token must fail closed:\n{}\n{}",
        String::from_utf8_lossy(&get_or_default_token_output.stdout),
        get_or_default_token_stderr
    );
    std::fs::write(&bytecode_path, &bytecode_source)
        .expect("restore bytecode adapter source after GetOrDefaultI32 token mutation");

    for (variant, adapter, token_source) in [
        (
            "ZeroMissI32",
            "compile_mir_program_with_ordinary_map_owned_any_v2_zero_miss_i32_route",
            "Some(ValidatedOrdinaryMapOwnedAnyV2Route::ZeroMissI32(&receipt)),",
        ),
        (
            "MapSetDropControl",
            "compile_mir_program_with_ordinary_map_owned_any_v2_map_set_drop_control_route",
            "Some(ValidatedOrdinaryMapOwnedAnyV2Route::MapSetDropControl(\n            &receipt,\n        )),",
        ),
        (
            "BranchLocalSetDiamond",
            "compile_mir_program_with_ordinary_map_owned_any_v2_branch_local_set_diamond_route",
            "Some(ValidatedOrdinaryMapOwnedAnyV2Route::BranchLocalSetDiamond(\n            &receipt,\n        )),",
        ),
        (
            "EmptyRootRootQueriesV1",
            "compile_mir_program_with_empty_root_root_queries_v1_route",
            "Some(ValidatedOrdinaryMapOwnedAnyV2Route::EmptyRootRootQueriesV1(\n            &receipt,\n        )),",
        ),
    ] {
        let wrong_token = bytecode_source.replacen(
            token_source,
            "Some(ValidatedOrdinaryMapOwnedAnyV2Route::CloneIsolation(&receipt)),",
            1,
        );
        assert!(
            bytecode_source != wrong_token,
            "{variant} mutation fixture must replace its typed route token"
        );
        std::fs::write(&bytecode_path, wrong_token)
            .unwrap_or_else(|_| panic!("write {variant} adapter source with a CloneIsolation token"));
        let token_output = std::process::Command::new("bash")
            .arg(&script_path)
            .arg("--static-only")
            .current_dir(&temp_root)
            .output()
            .unwrap_or_else(|_| panic!("run audit against a {variant} token mutation"));
        let token_stderr = String::from_utf8_lossy(&token_output.stderr);
        assert!(
            !token_output.status.success(),
            "the {variant} adapter must reject a CloneIsolation typed route token:\n{}",
            String::from_utf8_lossy(&token_output.stdout)
        );
        assert!(
            token_stderr.contains("owner_audit_error=bytecode_narrow_map_any_adapter_token_missing=")
                && token_stderr.contains(adapter),
            "wrong {variant} token omitted its fail-closed adapter diagnostic:\n{token_stderr}"
        );
    }
    std::fs::write(&bytecode_path, &bytecode_source)
        .expect("restore bytecode adapter source after added route-token mutations");

    let wrong_clone_validator = bytecode_source.replacen(
        "validate_ordinary_map_owned_any_v2_clone_isolation_route(checked, program)",
        "validate_ordinary_map_owned_any_v2_is_empty_route(checked, program)",
        1,
    );
    assert!(
        bytecode_source != wrong_clone_validator,
        "CloneIsolation mutation fixture must replace its Checker/MIR validator"
    );
    std::fs::write(&bytecode_path, wrong_clone_validator)
        .expect("write CloneIsolation adapter source with the IsEmpty validator");
    let clone_validator_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a CloneIsolation adapter with the IsEmpty validator");
    assert!(
        !clone_validator_output.status.success(),
        "the CloneIsolation adapter must reject an IsEmpty validator:\n{}",
        String::from_utf8_lossy(&clone_validator_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&clone_validator_output.stderr)
            .contains("owner_audit_error=bytecode_narrow_map_any_validator_missing="),
        "wrong CloneIsolation validator omitted the fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&clone_validator_output.stderr)
    );

    std::fs::write(&bytecode_path, &bytecode_source)
        .expect("restore bytecode adapter source after route mutations");
    let wrong_observer_receipt_handoff = bytecode_source.replacen(
        "checker_receipts.clone(),\n                observer_bindings,",
        "Vec::new(),\n                observer_bindings,",
        1,
    );
    assert!(
        bytecode_source != wrong_observer_receipt_handoff,
        "Observer V1 mutation fixture must replace the Checker receipt transfer"
    );
    std::fs::write(&bytecode_path, wrong_observer_receipt_handoff)
        .expect("write bytecode adapter source with a dropped Observer V1 receipt transfer");
    let observer_handoff_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a dropped Observer V1 receipt transfer");
    assert!(
        !observer_handoff_output.status.success(),
        "a bytecode anchor that drops the exact Observer V1 Checker receipts must fail closed:\n{}",
        String::from_utf8_lossy(&observer_handoff_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&observer_handoff_output.stderr)
            .contains("owner_audit_error=observer_v1_exact_route_binding_missing="),
        "Observer V1 receipt-transfer drift omitted the exact-route diagnostic:\n{}",
        String::from_utf8_lossy(&observer_handoff_output.stderr)
    );

    let native_source =
        std::fs::read_to_string(&native_path).expect("read copied native adapter source");
    let dropped_native_receipt = native_source.replacen(
        "let result = self.compile_mir_native_with_route_receipt_inner(\n            program,\n            receipt,\n            native_checker_authorized_route,\n        );",
        "let result = self.compile_mir_native_with_route_receipt_inner(\n            program,\n            canonical_receipt,\n            native_checker_authorized_route,\n        );",
        1,
    );
    assert_ne!(
        native_source, dropped_native_receipt,
        "Observer V1 native mutation fixture must replace the exact receipt handoff"
    );
    std::fs::write(&native_path, dropped_native_receipt)
        .expect("write native adapter source with a mismatched route receipt handoff");
    let native_handoff_output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run audit against a mismatched Observer V1 native receipt handoff");
    assert!(
        !native_handoff_output.status.success(),
        "a native adapter that forwards another receipt must fail closed:\n{}",
        String::from_utf8_lossy(&native_handoff_output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&native_handoff_output.stderr)
            .contains("owner_audit_error=observer_v1_exact_route_binding_missing="),
        "Observer V1 native receipt-transfer drift omitted the exact-route diagnostic:\n{}",
        String::from_utf8_lossy(&native_handoff_output.stderr)
    );
    drop(cleanup);
}

#[test]
fn legacy_owner_audit_rejects_checked_program_surface_body_snapshot() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-body-retirement");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts"))
        .expect("create legacy-body retirement audit temp root");
    mirror_source_tree_except(
        &root.join("src"),
        &root.join("src"),
        &temp_root.join("src"),
        &["core/resolved/mod.rs"],
    )
    .expect("mirror source tree for legacy-body mutations");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into legacy-body temp root");
    std::fs::copy(
        root.join("scripts/audit-mir-legacy-owners.sh"),
        temp_root.join("scripts/audit-mir-legacy-owners.sh"),
    )
    .expect("copy legacy owner audit script");

    let resolved_path = temp_root.join("src/core/resolved/mod.rs");
    std::fs::copy(root.join("src/core/resolved/mod.rs"), &resolved_path)
        .expect("copy CheckedProgram source for cfg mutations");
    let resolved_source =
        std::fs::read_to_string(&resolved_path).expect("read copied CheckedProgram source");
    let baseline = std::process::Command::new("bash")
        .arg(temp_root.join("scripts/audit-mir-legacy-owners.sh"))
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run baseline retired-body audit");
    assert!(
        baseline.status.success(),
        "retired CheckedProgram body snapshot must pass the audit:\n{}",
        String::from_utf8_lossy(&baseline.stderr)
    );

    for (label, injected_source) in [
        (
            "CheckedProgram field",
            resolved_source.replacen(
                "pub struct CheckedProgram {",
                "pub struct CheckedProgram {\n    legacy_file: Arc<File>,",
                1,
            ),
        ),
        (
            "CheckedProgram accessor",
            format!("{resolved_source}\n// pub(crate) fn legacy_body_file(&self) -> &File {{}}\n"),
        ),
        (
            "legacy-body trace consumer",
            format!("{resolved_source}\n// enum LegacyBodyConsumer {{}}\n"),
        ),
    ] {
        assert!(
            resolved_source != injected_source,
            "{label} mutation fixture must inject the retired owner symbol"
        );
        std::fs::write(&resolved_path, injected_source)
            .unwrap_or_else(|_| panic!("write injected {label}"));
        let output = std::process::Command::new("bash")
            .arg(temp_root.join("scripts/audit-mir-legacy-owners.sh"))
            .arg("--static-only")
            .current_dir(&temp_root)
            .output()
            .unwrap_or_else(|_| panic!("run audit against injected {label}"));
        assert!(
            !output.status.success(),
            "reintroduced {label} must fail the legacy owner audit:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("owner_audit_error=checked_program_surface_body_snapshot_present"),
            "reintroduced {label} omitted the fail-closed diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(&resolved_path, &resolved_source)
            .unwrap_or_else(|_| panic!("restore CheckedProgram source after {label} mutation"));
    }
    drop(cleanup);
}

#[test]
fn legacy_owner_evidence_marker_drift_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-audit");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create audit temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into audit temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into audit temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "    'test_legacy_codegen_function_calls().is_empty()'\n",
        "    'test_legacy_codegen_function_calls().missing()'\n",
        1,
    );
    assert_ne!(
        source_script, tampered_script,
        "owner evidence mutation must change the executable expected marker"
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write tampered audit script");
    let output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run tampered legacy owner audit");
    let repeated = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("repeat tampered legacy owner audit");
    assert_eq!(
        output.status.code(),
        repeated.status.code(),
        "tampered owner evidence exit code drifted across repeats"
    );
    assert_eq!(
        output.stdout, repeated.stdout,
        "tampered owner evidence stdout drifted across repeats"
    );
    assert_eq!(
        output.stderr, repeated.stderr,
        "tampered owner evidence stderr drifted across repeats"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "tampered owner evidence must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("evidence_test_missing_marker=compile_checked_routes_generic_ignored_bool_binding_projection_without_legacy_access marker=test_legacy_codegen_function_calls().missing()"),
        "tampered owner evidence omitted marker failure:\n{stderr}"
    );
    std::fs::remove_dir_all(&temp_root).expect("remove audit temp root");
    let restored = std::process::Command::new("bash")
        .arg(root.join("scripts/audit-mir-legacy-owners-static.sh"))
        .current_dir(&root)
        .output()
        .expect("rerun restored legacy owner audit");
    assert!(
        restored.status.success(),
        "restored owner audit failed after negative probe:\n{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let restored_stdout = String::from_utf8_lossy(&restored.stdout);
    assert!(restored_stdout.contains("static_audit_status=ok"));
    assert!(
        !restored_stdout
            .lines()
            .any(|line| line == "audit_status=ok"),
        "static inventory must never emit the full audit_status=ok line"
    );
    assert!(
        !restored_stdout.contains("route_boundary_audit_status=passed"),
        "static inventory must never report the full dynamic audit as passed"
    );
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_zero_owner_evidence_missing_or_forged_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-scalar-evidence");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create scalar audit temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into scalar audit temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into scalar audit temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replace(
        "\"$ROOT_DIR/src/tests/canonical_scalar_ffi.rs\"",
        "\"$ROOT_DIR/fake_scalar_ffi.rs\"",
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write scalar audit script fixture");
    let scalar_fixture = temp_root.join("fake_scalar_ffi.rs");
    std::fs::write(
        &scalar_fixture,
        "fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers() {\n}\n#[test]\n",
    )
    .expect("write scalar fixture without zero-owner assertion");
    let missing = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run missing scalar assertion audit");
    assert!(
        !missing.status.success(),
        "missing scalar canonical-route assertion must fail closed:\n{}",
        String::from_utf8_lossy(&missing.stdout)
    );
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains(
            "owner_audit_error=closed_scalar_zero_owner_evidence_missing_canonical_route_assertion"
        ),
        "missing scalar route assertion omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    std::fs::write(
        &scalar_fixture,
        "fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers() {\n}\n#[test]\nfn forged_marker_outside_scalar_test() { assert!(crate::core::mir::CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route)); }\n",
    )
    .expect("write scalar fixture with forged outside marker");
    let forged = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run forged scalar assertion audit");
    assert!(
        !forged.status.success(),
        "forged marker outside scalar test must fail closed:\n{}",
        String::from_utf8_lossy(&forged.stdout)
    );
    assert!(
        String::from_utf8_lossy(&forged.stderr).contains(
            "owner_audit_error=closed_scalar_zero_owner_evidence_missing_canonical_route_assertion"
        ),
        "forged outside marker omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&forged.stderr)
    );
    std::fs::remove_dir_all(&temp_root).expect("remove scalar audit temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_cli_matrix_missing_or_forged_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-cli-matrix");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create CLI audit temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into CLI audit temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into CLI audit temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replace(
        "\"$ROOT_DIR/tests/real_world_cli.rs\"",
        "\"$ROOT_DIR/fake_scalar_cli.rs\"",
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write CLI audit script fixture");
    let cli_fixture = temp_root.join("fake_scalar_cli.rs");
    std::fs::write(
        &cli_fixture,
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n}\n#[test]\n",
    )
    .expect("write CLI fixture missing MIR matrix");
    let missing = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run missing CLI matrix audit");
    assert!(
        !missing.status.success(),
        "missing CLI matrix dimension must fail closed:\n{}",
        String::from_utf8_lossy(&missing.stdout)
    );
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("owner_audit_error=closed_scalar_cli_evidence_missing_mir_matrix"),
        "missing CLI matrix omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    std::fs::write(
        &cli_fixture,
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n}\n#[test]\nfn forged_mir_matrix_outside_cli_test() { for explicit_mir in [false, true] {} }\n",
    )
    .expect("write CLI fixture with forged outside MIR matrix");
    let forged = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run forged CLI matrix audit");
    assert!(
        !forged.status.success(),
        "forged CLI matrix outside test must fail closed:\n{}",
        String::from_utf8_lossy(&forged.stdout)
    );
    assert!(
        String::from_utf8_lossy(&forged.stderr)
            .contains("owner_audit_error=closed_scalar_cli_evidence_missing_mir_matrix"),
        "forged outside CLI matrix omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&forged.stderr)
    );
    std::fs::write(
        &cli_fixture,
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for explicit_mir in [false, true] {}\n}\n#[test]\nfn forged_contract_matrix_outside_cli_test() { for contracts in [true, false] {} }\n",
    )
    .expect("write CLI fixture with forged outside contract matrix");
    let forged_contract = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run forged contract matrix audit");
    assert!(
        !forged_contract.status.success(),
        "forged contract matrix outside test must fail closed:\n{}",
        String::from_utf8_lossy(&forged_contract.stdout)
    );
    assert!(
        String::from_utf8_lossy(&forged_contract.stderr)
            .contains("owner_audit_error=closed_scalar_cli_evidence_missing_contract_matrix"),
        "forged outside contract matrix omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&forged_contract.stderr)
    );
    std::fs::remove_dir_all(&temp_root).expect("remove CLI audit temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_cli_abi_or_route_evidence_missing_or_forged_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-cli-abi");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create CLI ABI temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into CLI ABI temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into CLI ABI temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replace(
        "\"$ROOT_DIR/tests/real_world_cli.rs\"",
        "\"$ROOT_DIR/fake_scalar_cli.rs\"",
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write CLI ABI audit script fixture");
    let cli_fixture = temp_root.join("fake_scalar_cli.rs");
    std::fs::write(
        &cli_fixture,
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n    for explicit_mir in [false, true] {}\n    func relay(value: i64) -> i64 { value }\n    assert!(!run_stderr.contains(\"canonical route disposition: legacy\"));\n    let _ = [\"mir_ffi_i32\", \"mir_ffi_i64\", \"mir_ffi_bool\", \"mir_ffi_f64\"];\n}\n#[test]\n",
    )
    .expect("write CLI fixture missing ABI symbol");
    let missing_abi = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run missing ABI audit");
    assert!(
        !missing_abi.status.success(),
        "missing ABI symbol must fail closed:\n{}",
        String::from_utf8_lossy(&missing_abi.stdout)
    );
    assert!(
        String::from_utf8_lossy(&missing_abi.stderr).contains(
            "owner_audit_error=closed_scalar_cli_evidence_missing_abi_symbol symbol=mir_ffi_store"
        ),
        "missing ABI diagnostic drifted:\n{}",
        String::from_utf8_lossy(&missing_abi.stderr)
    );
    std::fs::write(
        &cli_fixture,
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n    for explicit_mir in [false, true] {}\n    func relay(value: i64) -> i64 { value }\n    assert!(!run_stderr.contains(\"canonical route disposition: legacy\"));\n    let _ = [\"mir_ffi_i32\", \"mir_ffi_i64\", \"mir_ffi_bool\", \"mir_ffi_f64\"];\n}\n#[test]\nfn forged_abi_outside_cli_test() { let _ = \"mir_ffi_store\"; }\n",
    )
    .expect("write CLI fixture with forged ABI symbol outside test");
    let forged_abi = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run forged ABI audit");
    assert!(
        !forged_abi.status.success(),
        "forged ABI symbol outside test must fail closed:\n{}",
        String::from_utf8_lossy(&forged_abi.stdout)
    );
    assert!(
        String::from_utf8_lossy(&forged_abi.stderr).contains(
            "owner_audit_error=closed_scalar_cli_evidence_missing_abi_symbol symbol=mir_ffi_store"
        ),
        "forged ABI diagnostic drifted:\n{}",
        String::from_utf8_lossy(&forged_abi.stderr)
    );
    std::fs::write(
        &cli_fixture,
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n    for explicit_mir in [false, true] {}\n    func relay(value: i64) -> i64 { value }\n    let _ = [\"mir_ffi_i32\", \"mir_ffi_i64\", \"mir_ffi_bool\", \"mir_ffi_f64\", \"mir_ffi_store\"];\n}\n#[test]\n",
    )
    .expect("write CLI fixture missing route assertion");
    let missing_route = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run missing route audit");
    assert!(
        !missing_route.status.success(),
        "missing route assertion must fail closed:\n{}",
        String::from_utf8_lossy(&missing_route.stdout)
    );
    assert!(
        String::from_utf8_lossy(&missing_route.stderr).contains(
            "owner_audit_error=closed_scalar_cli_evidence_missing_legacy_route_assertion"
        ),
        "missing route diagnostic drifted:\n{}",
        String::from_utf8_lossy(&missing_route.stderr)
    );
    std::fs::remove_dir_all(&temp_root).expect("remove CLI ABI temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_evidence_markers_have_stable_order() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("scripts/audit-mir-legacy-owners.sh");
    let run = || {
        std::process::Command::new("bash")
            .arg(&path)
            .arg("--static-only")
            .current_dir(&root)
            .output()
            .expect("legacy owner audit must execute")
    };
    let first = run();
    let second = run();
    assert!(first.status.success(), "first owner audit failed");
    assert!(second.status.success(), "second owner audit failed");
    let markers = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.starts_with("closed_scalar_"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let expected = vec![
        "closed_scalar_zero_owner_evidence=src/tests/canonical_scalar_ffi.rs::scalar_ffi_c_abi_and_side_effect_order_match_three_consumers".to_owned(),
        "closed_scalar_zero_owner_evidence_marker=CanonicalMirRouteProfile::ScalarFfi.is_materialized(&route)".to_owned(),
        "closed_scalar_cli_evidence=tests/real_world_cli.rs::canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts".to_owned(),
        "closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]".to_owned(),
        "closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f32,mir_ffi_f32_code,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent".to_owned(),
        "closed_scalar_cli_nested_helper_marker=relay(i64);separate_main_ffi_call".to_owned(),
    ];
    assert_eq!(
        markers(&first),
        expected,
        "scalar evidence marker order drifted"
    );
    assert_eq!(
        markers(&first),
        markers(&second),
        "scalar evidence markers drifted across runs"
    );
}

#[test]
fn legacy_owner_scalar_marker_sequence_injection_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-marker-sequence");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create marker sequence temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into marker sequence temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into marker sequence temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "expected_closed_scalar_marker_sequence=(",
        "emit_closed_scalar_marker 'closed_scalar_forged_marker=1'\n\nexpected_closed_scalar_marker_sequence=(",
        1,
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write injected marker audit script");
    let output = std::process::Command::new("bash")
        .arg(&script_path)
        .arg("--static-only")
        .current_dir(&temp_root)
        .output()
        .expect("run injected marker audit");
    assert!(
        !output.status.success(),
        "injected scalar marker must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("owner_audit_error=closed_scalar_evidence_marker_sequence_drift"),
        "injected marker omitted sequence drift diagnostic:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(&temp_root).expect("remove marker sequence temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_marker_sequence_missing_duplicate_or_reordered_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-marker-drift");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create marker drift temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into marker drift temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into marker drift temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    let matrix_call = "            emit_closed_scalar_marker 'closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]'\n";
    let abi_call = "            emit_closed_scalar_marker 'closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f32,mir_ffi_f32_code,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent'\n";
    let run_drift = |tampered_script: String, label: &str| {
        std::fs::write(&script_path, tampered_script).expect("write marker drift script");
        let output = std::process::Command::new("bash")
            .arg(&script_path)
            .arg("--static-only")
            .current_dir(&temp_root)
            .output()
            .expect("run marker drift audit");
        assert!(
            !output.status.success(),
            "{label} marker drift must fail closed:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("owner_audit_error=closed_scalar_evidence_marker_sequence_drift"),
            "{label} marker drift omitted sequence diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_drift(source_script.replacen(matrix_call, "", 1), "missing");
    run_drift(
        source_script.replacen(matrix_call, &format!("{matrix_call}{matrix_call}"), 1),
        "duplicate",
    );
    run_drift(
        source_script.replacen(
            &format!("{matrix_call}{abi_call}"),
            &format!("{abi_call}{matrix_call}"),
            1,
        ),
        "reordered",
    );
    std::fs::remove_dir_all(&temp_root).expect("remove marker drift temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_marker_sequence_failure_snapshot_is_repeatable() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-marker-snapshot");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create marker snapshot temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into marker snapshot temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into marker snapshot temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "expected_closed_scalar_marker_sequence=(",
        "emit_closed_scalar_marker 'closed_scalar_forged_marker=1'\n\nexpected_closed_scalar_marker_sequence=(",
        1,
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write snapshot audit script");
    let run = || {
        std::process::Command::new("bash")
            .arg(&script_path)
            .arg("--static-only")
            .current_dir(&temp_root)
            .output()
            .expect("run snapshot audit")
    };
    let first = run();
    let second = run();
    assert_eq!(
        first.status.code(),
        second.status.code(),
        "failure exit code drifted"
    );
    assert_eq!(
        first.stdout, second.stdout,
        "failure stdout snapshot drifted"
    );
    assert_eq!(
        first.stderr, second.stderr,
        "failure stderr snapshot drifted"
    );
    assert!(!first.status.success(), "injected marker must fail closed");
    assert!(
        String::from_utf8_lossy(&first.stderr)
            .contains("owner_audit_error=closed_scalar_evidence_marker_sequence_drift"),
        "failure snapshot omitted sequence diagnostic:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    std::fs::remove_dir_all(&temp_root).expect("remove marker snapshot temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_marker_sequence_failure_snapshot_is_concurrency_stable() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-marker-concurrent");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create concurrent marker temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into concurrent marker temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into concurrent marker temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "expected_closed_scalar_marker_sequence=(",
        "emit_closed_scalar_marker 'closed_scalar_forged_marker=1'\n\nexpected_closed_scalar_marker_sequence=(",
        1,
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write concurrent marker audit script");

    let runs = std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| {
                let script_path = script_path.clone();
                let temp_root = temp_root.clone();
                scope.spawn(move || {
                    std::process::Command::new("bash")
                        .arg(&script_path)
                        .arg("--static-only")
                        .current_dir(&temp_root)
                        .output()
                        .expect("run concurrent marker audit")
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("concurrent marker audit thread panicked")
            })
            .collect::<Vec<_>>()
    });
    let first = &runs[0];
    assert!(!first.status.success(), "injected marker must fail closed");
    assert!(
        String::from_utf8_lossy(&first.stderr)
            .contains("owner_audit_error=closed_scalar_evidence_marker_sequence_drift"),
        "failure snapshot omitted sequence diagnostic:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    for (index, run) in runs.iter().enumerate().skip(1) {
        assert_eq!(
            run.status.code(),
            first.status.code(),
            "concurrent run {index} failure exit code drifted"
        );
        assert_eq!(
            run.stdout, first.stdout,
            "concurrent run {index} failure stdout snapshot drifted"
        );
        assert_eq!(
            run.stderr, first.stderr,
            "concurrent run {index} failure stderr snapshot drifted"
        );
    }
    std::fs::remove_dir_all(&temp_root).expect("remove concurrent marker temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_marker_sequence_failure_snapshot_is_multi_batch_stable() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-marker-batches");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create batch marker temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into batch marker temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into batch marker temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "expected_closed_scalar_marker_sequence=(",
        "emit_closed_scalar_marker 'closed_scalar_forged_marker=1'\n\nexpected_closed_scalar_marker_sequence=(",
        1,
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write batch marker audit script");

    let run_batch = |reverse: bool| {
        let width = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(width + 1));
        std::thread::scope(|scope| {
            let order: Vec<usize> = if reverse {
                (0..width).rev().collect()
            } else {
                (0..width).collect()
            };
            let handles = order
                .into_iter()
                .map(|_| {
                    let script_path = script_path.clone();
                    let temp_root = temp_root.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        std::process::Command::new("bash")
                            .arg(&script_path)
                            .arg("--static-only")
                            .current_dir(&temp_root)
                            .output()
                            .expect("run batch marker audit")
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("batch marker audit thread panicked"))
                .collect::<Vec<_>>()
        })
    };

    let batches = [run_batch(false), run_batch(true), run_batch(false)];
    let first = &batches[0][0];
    assert!(!first.status.success(), "injected marker must fail closed");
    assert!(
        String::from_utf8_lossy(&first.stderr)
            .contains("owner_audit_error=closed_scalar_evidence_marker_sequence_drift"),
        "batch failure omitted sequence diagnostic:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    for (batch_index, batch) in batches.iter().enumerate() {
        for (run_index, run) in batch.iter().enumerate() {
            assert_eq!(
                run.status.code(),
                first.status.code(),
                "batch {batch_index} run {run_index} failure exit code drifted"
            );
            assert_eq!(
                run.stdout, first.stdout,
                "batch {batch_index} run {run_index} failure stdout snapshot drifted"
            );
            assert_eq!(
                run.stderr, first.stderr,
                "batch {batch_index} run {run_index} failure stderr snapshot drifted"
            );
        }
    }
    std::fs::remove_dir_all(&temp_root).expect("remove batch marker temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_marker_mixed_success_failure_batches_are_isolated() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-marker-mixed");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create mixed marker temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into mixed marker temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into mixed marker temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "expected_closed_scalar_marker_sequence=(",
        "emit_closed_scalar_marker 'closed_scalar_forged_marker=1'\n\nexpected_closed_scalar_marker_sequence=(",
        1,
    );
    let valid_path = temp_root.join("scripts/audit-valid.sh");
    let tampered_path = temp_root.join("scripts/audit-tampered.sh");
    std::fs::write(&valid_path, &source_script).expect("write valid mixed audit script");
    std::fs::write(&tampered_path, tampered_script).expect("write tampered mixed audit script");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
    let runs = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for expected_success in [true, false, true, false, true, false, true, false] {
            let script_path = if expected_success {
                valid_path.clone()
            } else {
                tampered_path.clone()
            };
            let temp_root = temp_root.clone();
            let barrier = barrier.clone();
            handles.push(scope.spawn(move || {
                barrier.wait();
                let output = std::process::Command::new("bash")
                    .arg(&script_path)
                    .arg("--static-only")
                    .current_dir(&temp_root)
                    .output()
                    .expect("run mixed owner audit");
                (expected_success, output)
            }));
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("mixed owner audit thread panicked"))
            .collect::<Vec<_>>()
    });

    let success_runs = runs
        .iter()
        .filter(|(expected_success, _)| *expected_success)
        .collect::<Vec<_>>();
    let failure_runs = runs
        .iter()
        .filter(|(expected_success, _)| !*expected_success)
        .collect::<Vec<_>>();
    assert_eq!(success_runs.len(), 4, "mixed batch lost successful runs");
    assert_eq!(failure_runs.len(), 4, "mixed batch lost failure runs");
    let successful_output = &success_runs[0].1;
    assert!(
        successful_output.status.success(),
        "valid owner audit unexpectedly failed:\n{}",
        String::from_utf8_lossy(&successful_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&successful_output.stdout)
            .contains("scalar_evidence_marker_sequence_status=ok"),
        "valid owner audit omitted marker success status"
    );
    for (index, (_, output)) in success_runs.iter().enumerate().skip(1) {
        assert_eq!(
            output.status.code(),
            successful_output.status.code(),
            "successful mixed run {index} exit code drifted"
        );
        assert_eq!(
            output.stdout, successful_output.stdout,
            "successful mixed run {index} stdout drifted"
        );
        assert_eq!(
            output.stderr, successful_output.stderr,
            "successful mixed run {index} stderr drifted"
        );
    }
    let failed_output = &failure_runs[0].1;
    assert!(
        !failed_output.status.success(),
        "tampered owner audit must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&failed_output.stderr)
            .contains("owner_audit_error=closed_scalar_evidence_marker_sequence_drift"),
        "tampered owner audit omitted sequence drift diagnostic"
    );
    for (index, (_, output)) in failure_runs.iter().enumerate().skip(1) {
        assert_eq!(
            output.status.code(),
            failed_output.status.code(),
            "failed mixed run {index} exit code drifted"
        );
        assert_eq!(
            output.stdout, failed_output.stdout,
            "failed mixed run {index} stdout drifted"
        );
        assert_eq!(
            output.stderr, failed_output.stderr,
            "failed mixed run {index} stderr drifted"
        );
    }
    std::fs::remove_dir_all(&temp_root).expect("remove mixed marker temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_scalar_marker_repeated_roots_keep_paths_isolated() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "expected_closed_scalar_marker_sequence=(",
        "emit_closed_scalar_marker 'closed_scalar_forged_marker=1'\n\nexpected_closed_scalar_marker_sequence=(",
        1,
    );
    let mut cases = Vec::new();
    let mut cleanups = Vec::new();
    for (index, expected_success) in [true, false, true, false].into_iter().enumerate() {
        let temp_root =
            unique_legacy_owner_audit_temp_root(&format!("mimi-legacy-owner-marker-root-{index}"));
        std::fs::create_dir_all(temp_root.join("scripts")).expect("create isolated marker root");
        std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
            .expect("link source tree into isolated marker root");
        std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
            .expect("link integration tests into isolated marker root");
        let script_path = temp_root.join("scripts/audit.sh");
        std::fs::write(
            &script_path,
            if expected_success {
                &source_script
            } else {
                &tampered_script
            },
        )
        .expect("write isolated marker audit script");
        cleanups.push(LegacyOwnerAuditTempRootCleanup(temp_root.clone()));
        cases.push((expected_success, temp_root, script_path));
    }

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(cases.len() + 1));
    let runs = std::thread::scope(|scope| {
        let handles = cases
            .iter()
            .map(|(expected_success, temp_root, script_path)| {
                let expected_success = *expected_success;
                let temp_root = temp_root.clone();
                let script_path = script_path.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let output = std::process::Command::new("bash")
                        .arg(&script_path)
                        .arg("--static-only")
                        .current_dir(&temp_root)
                        .output()
                        .expect("run isolated marker audit");
                    (expected_success, temp_root, output)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("isolated marker audit thread panicked")
            })
            .collect::<Vec<_>>()
    });

    let normalize =
        |expected_success: bool, temp_root: &std::path::Path, output: &std::process::Output| {
            let root_marker = format!("root={}", temp_root.display());
            let stdout = String::from_utf8_lossy(&output.stdout)
                .replace(&root_marker, "root=<isolated>")
                .replace(&temp_root.display().to_string(), "<isolated>");
            let stderr = String::from_utf8_lossy(&output.stderr)
                .replace(&temp_root.display().to_string(), "<isolated>");
            (expected_success, output.status.code(), stdout, stderr)
        };
    let normalized = runs
        .iter()
        .map(|(expected_success, temp_root, output)| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains(&temp_root.display().to_string()),
                "audit output omitted its own isolated root {}",
                temp_root.display()
            );
            for (_, other_root, _) in &runs {
                if other_root != temp_root {
                    assert!(
                        !stdout.contains(&other_root.display().to_string()),
                        "audit output for {} leaked another root {}",
                        temp_root.display(),
                        other_root.display()
                    );
                }
            }
            normalize(*expected_success, temp_root, output)
        })
        .collect::<Vec<_>>();
    let successful = normalized
        .iter()
        .filter(|(expected_success, _, _, _)| *expected_success)
        .collect::<Vec<_>>();
    let failed = normalized
        .iter()
        .filter(|(expected_success, _, _, _)| !*expected_success)
        .collect::<Vec<_>>();
    assert_eq!(successful.len(), 2, "isolated root success runs missing");
    assert_eq!(failed.len(), 2, "isolated root failure runs missing");
    for (label, group) in [("success", successful), ("failure", failed)] {
        let first = group[0];
        for (index, current) in group.iter().enumerate().skip(1) {
            assert_eq!(
                current.1, first.1,
                "isolated {label} run {index} exit code drifted"
            );
            assert_eq!(
                current.2, first.2,
                "isolated {label} run {index} stdout drifted"
            );
            assert_eq!(
                current.3, first.3,
                "isolated {label} run {index} stderr drifted"
            );
        }
    }
    assert!(normalized
        .iter()
        .any(|(expected_success, _, _, _)| !expected_success));
    for (expected_success, _, stdout, stderr) in &normalized {
        if *expected_success {
            assert!(stdout.contains("scalar_evidence_marker_sequence_status=ok"));
        } else {
            assert!(
                stderr.contains("owner_audit_error=closed_scalar_evidence_marker_sequence_drift")
            );
        }
    }
    for cleanup in &cleanups {
        let _ = LegacyOwnerAuditTempRootCleanup::cleanup_path(&cleanup.0);
    }
    drop(cleanups);
}

#[test]
fn legacy_owner_scalar_marker_roots_cleanup_is_idempotent_and_residue_free() {
    let prefix = format!("mimi-legacy-owner-marker-cleanup-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let mut cleanups = Vec::new();
    for _ in 0..6 {
        let root = unique_legacy_owner_audit_temp_root(&prefix);
        let nested = root.join("nested").join("evidence");
        std::fs::create_dir_all(&nested).expect("create marker cleanup root");
        std::fs::write(nested.join("snapshot"), b"marker cleanup snapshot")
            .expect("write marker cleanup snapshot");
        cleanups.push(LegacyOwnerAuditTempRootCleanup(root));
    }
    let paths = cleanups
        .iter()
        .map(|cleanup| cleanup.0.clone())
        .collect::<Vec<_>>();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(paths.len() * 2 + 1));
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for path in &paths {
            for _ in 0..2 {
                let path = path.clone();
                let barrier = barrier.clone();
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    LegacyOwnerAuditTempRootCleanup::cleanup_path(&path)
                }));
            }
        }
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("marker cleanup worker panicked"))
            .collect::<Vec<_>>()
    });
    for result in results {
        assert!(
            result.is_ok(),
            "concurrent marker cleanup must remain idempotent: {result:?}"
        );
    }
    assert!(
        paths.iter().all(|path| !path.exists()),
        "concurrent marker cleanup left a root residue"
    );
    drop(cleanups);
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_scalar_marker_concurrent_cleanup_failure_snapshot_recovers_in_order() {
    use std::os::unix::fs::PermissionsExt;

    let prefix = format!("mimi-legacy-owner-marker-recovery-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let mut roots = Vec::new();
    let mut cleanups = Vec::new();
    for index in 0..4 {
        let root = unique_legacy_owner_audit_temp_root(&format!("{prefix}{index}"));
        let nested = root.join("nested").join("evidence");
        std::fs::create_dir_all(&nested).expect("create recovery marker root");
        std::fs::write(nested.join("snapshot"), b"recovery marker snapshot")
            .expect("write recovery marker snapshot");
        cleanups.push(LegacyOwnerAuditTempRootCleanup(root.clone()));
        roots.push(root);
    }
    let blocked = roots[1].clone();
    let mut permissions = std::fs::metadata(&blocked)
        .expect("read recovery marker permissions")
        .permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&blocked, permissions).expect("make recovery marker root unreadable");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(roots.len() + 1));
    let results = std::thread::scope(|scope| {
        let handles = roots
            .iter()
            .map(|root| {
                let root = root.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let result = LegacyOwnerAuditTempRootCleanup::cleanup_path(&root);
                    (root, result)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("recovery cleanup worker panicked"))
            .collect::<Vec<_>>()
    });
    let failures = results
        .iter()
        .filter(|(_, result)| result.is_err())
        .collect::<Vec<_>>();
    assert_eq!(
        failures.len(),
        1,
        "exactly one concurrent cleanup must fail"
    );
    assert_eq!(
        failures[0].0.as_path(),
        blocked.as_path(),
        "wrong root reported as cleanup failure"
    );
    assert!(
        failures[0]
            .1
            .as_ref()
            .expect_err("blocked cleanup must carry an error")
            .contains(&blocked.display().to_string()),
        "blocked cleanup error omitted its root path"
    );
    for (root, result) in &results {
        if result.is_ok() {
            assert!(
                !root.exists(),
                "successful cleanup left root {}",
                root.display()
            );
        }
    }
    let snapshots = (0..3)
        .map(|_| legacy_owner_temp_root_residues(&prefix))
        .collect::<Vec<_>>();
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot == &vec![blocked.clone()]),
        "concurrent cleanup residue snapshots drifted: {snapshots:?}"
    );

    let mut permissions = std::fs::metadata(&blocked)
        .expect("read blocked recovery marker permissions")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&blocked, permissions)
        .expect("restore blocked recovery marker permissions");
    LegacyOwnerAuditTempRootCleanup::cleanup_path(&blocked)
        .expect("permission recovery cleanup must succeed");
    LegacyOwnerAuditTempRootCleanup::cleanup_path(&blocked)
        .expect("permission recovery cleanup must be idempotent");
    assert!(
        roots.iter().all(|root| !root.exists()),
        "recovery cleanup left a root residue"
    );
    drop(cleanups);
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_scalar_marker_recovery_batches_clear_stale_snapshots() {
    use std::os::unix::fs::PermissionsExt;

    let prefix = format!(
        "mimi-legacy-owner-marker-recovery-batch-{}-",
        std::process::id()
    );
    assert_no_legacy_owner_temp_roots(&prefix);
    for batch in 0..3 {
        let root = unique_legacy_owner_audit_temp_root(&format!("{prefix}{batch}"));
        let nested = root.join("nested").join("evidence");
        std::fs::create_dir_all(&nested).expect("create recovery batch root");
        std::fs::write(nested.join("snapshot"), b"recovery batch snapshot")
            .expect("write recovery batch snapshot");
        let mut permissions = std::fs::metadata(&root)
            .expect("read recovery batch permissions")
            .permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&root, permissions).expect("make recovery batch root unreadable");

        let error = LegacyOwnerAuditTempRootCleanup::cleanup_path(&root)
            .expect_err("unreadable recovery batch root must fail closed");
        assert!(
            error.contains(&root.display().to_string()),
            "recovery batch error omitted root path"
        );
        let snapshots = (0..3)
            .map(|_| legacy_owner_temp_root_residues(&prefix))
            .collect::<Vec<_>>();
        assert!(
            snapshots
                .iter()
                .all(|snapshot| snapshot == &vec![root.clone()]),
            "recovery batch {batch} snapshots drifted or retained stale roots: {snapshots:?}"
        );

        let mut permissions = std::fs::metadata(&root)
            .expect("read blocked recovery batch permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&root, permissions).expect("restore recovery batch permissions");
        LegacyOwnerAuditTempRootCleanup::cleanup_path(&root)
            .expect("recovery batch retry must succeed");
        LegacyOwnerAuditTempRootCleanup::cleanup_path(&root)
            .expect("recovery batch retry must be idempotent");
        assert_no_legacy_owner_temp_roots(&prefix);
    }
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_scalar_marker_shuffled_failure_batches_keep_snapshot_order() {
    let prefix = format!("mimi-legacy-owner-marker-shuffled-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let paths = (0..6)
        .map(|index| {
            let path = unique_legacy_owner_audit_temp_root(&format!("{prefix}{index}"));
            std::fs::write(&path, b"shuffled failure residue")
                .expect("create shuffled failure residue");
            path
        })
        .collect::<Vec<_>>();
    let mut expected = paths.clone();
    expected.sort();
    let orders = [
        (0..paths.len()).collect::<Vec<_>>(),
        (0..paths.len()).rev().collect::<Vec<_>>(),
        vec![2, 5, 1, 4, 0, 3],
        vec![3, 0, 4, 1, 5, 2],
    ];
    let mut baseline_errors = None;
    for (batch, order) in orders.into_iter().enumerate() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(paths.len() + 1));
        let errors = std::thread::scope(|scope| {
            let handles = order
                .into_iter()
                .map(|index| {
                    let path = paths[index].clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        LegacyOwnerAuditTempRootCleanup::cleanup_path(&path)
                            .expect_err("regular-file cleanup must fail closed")
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("shuffled cleanup worker panicked"))
                .collect::<Vec<_>>()
        });
        let snapshot = legacy_owner_temp_root_residues(&prefix);
        assert_eq!(snapshot, expected, "batch {batch} snapshot order drifted");
        let mut sorted_errors = errors;
        sorted_errors.sort();
        assert_eq!(
            sorted_errors.len(),
            expected.len(),
            "batch {batch} lost a failure diagnostic"
        );
        for (error, path) in sorted_errors.iter().zip(&expected) {
            assert!(
                error.contains(&path.display().to_string()),
                "batch {batch} error did not identify its path: {error}"
            );
        }
        if let Some(baseline) = &baseline_errors {
            assert_eq!(
                &sorted_errors, baseline,
                "batch {batch} failure diagnostics drifted"
            );
        } else {
            baseline_errors = Some(sorted_errors);
        }
    }
    for path in paths {
        std::fs::remove_file(path).expect("remove shuffled failure residue");
    }
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_scalar_marker_interleaved_duplicate_recovery_is_stable() {
    use std::os::unix::fs::PermissionsExt;

    let prefix = format!(
        "mimi-legacy-owner-marker-interleaved-{}-",
        std::process::id()
    );
    assert_no_legacy_owner_temp_roots(&prefix);
    let roots = (0..5)
        .map(|index| {
            let root = unique_legacy_owner_audit_temp_root(&format!("{prefix}{index}"));
            let nested = root.join("nested").join("evidence");
            std::fs::create_dir_all(&nested).expect("create interleaved recovery root");
            std::fs::write(nested.join("snapshot"), b"interleaved recovery snapshot")
                .expect("write interleaved recovery snapshot");
            root
        })
        .collect::<Vec<_>>();
    let blocked = roots[2].clone();
    let mut permissions = std::fs::metadata(&blocked)
        .expect("read interleaved recovery permissions")
        .permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&blocked, permissions)
        .expect("make interleaved recovery root unreadable");

    let run_batch = |order: &[usize]| {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(order.len() + 1));
        std::thread::scope(|scope| {
            let handles = order
                .iter()
                .map(|index| {
                    let root = roots[*index].clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        let result = LegacyOwnerAuditTempRootCleanup::cleanup_path(&root);
                        (root, result)
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("interleaved cleanup worker panicked"))
                .collect::<Vec<_>>()
        })
    };
    let first_order = [2, 0, 4, 1, 3, 3, 1, 4, 0, 2];
    let first_results = run_batch(&first_order);
    let first_snapshots = (0..3)
        .map(|_| legacy_owner_temp_root_residues(&prefix))
        .collect::<Vec<_>>();

    let mut permissions = std::fs::metadata(&blocked)
        .expect("read blocked interleaved recovery permissions")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&blocked, permissions)
        .expect("restore interleaved recovery permissions");
    let second_results = run_batch(&first_order);
    let final_snapshot = legacy_owner_temp_root_residues(&prefix);

    let first_failures = first_results
        .iter()
        .filter(|(_, result)| result.is_err())
        .collect::<Vec<_>>();
    assert_eq!(
        first_failures.len(),
        2,
        "both duplicate attempts for the blocked root must fail"
    );
    assert!(
        first_failures.iter().all(|(root, result)| {
            root == &blocked
                && result
                    .as_ref()
                    .expect_err("blocked cleanup must carry an error")
                    .contains(&blocked.display().to_string())
        }),
        "blocked cleanup errors were not path-specific: {first_failures:?}"
    );
    assert!(
        first_results
            .iter()
            .filter(|(root, _)| root != &blocked)
            .all(|(_, result)| result.is_ok()),
        "accessible roots must tolerate duplicate cleanup"
    );
    assert!(
        first_snapshots
            .iter()
            .all(|snapshot| snapshot == &vec![blocked.clone()]),
        "interleaved failure snapshots drifted: {first_snapshots:?}"
    );
    assert!(
        second_results.iter().all(|(_, result)| result.is_ok()),
        "recovery duplicate cleanup must be idempotent"
    );
    assert!(
        final_snapshot.is_empty(),
        "interleaved recovery left residues: {final_snapshot:?}"
    );
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_scalar_marker_cross_batch_interleaving_keeps_failure_ownership() {
    use std::os::unix::fs::PermissionsExt;

    let prefix_a = format!("mimi-legacy-owner-marker-cross-a-{}-", std::process::id());
    let prefix_b = format!("mimi-legacy-owner-marker-cross-b-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix_a);
    assert_no_legacy_owner_temp_roots(&prefix_b);
    let create_roots = |prefix: &str| {
        (0..2)
            .map(|index| {
                let root = unique_legacy_owner_audit_temp_root(&format!("{prefix}{index}"));
                let nested = root.join("nested").join("evidence");
                std::fs::create_dir_all(&nested).expect("create cross-batch root");
                std::fs::write(nested.join("snapshot"), b"cross-batch snapshot")
                    .expect("write cross-batch snapshot");
                root
            })
            .collect::<Vec<_>>()
    };
    let roots_a = create_roots(&prefix_a);
    let roots_b = create_roots(&prefix_b);
    let blocked_a = roots_a[1].clone();
    let blocked_b = roots_b[0].clone();
    for root in [&blocked_a, &blocked_b] {
        let mut permissions = std::fs::metadata(root)
            .expect("read cross-batch permissions")
            .permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(root, permissions).expect("make cross-batch root unreadable");
    }

    let operations = [
        ('a', 0),
        ('b', 1),
        ('a', 1),
        ('b', 0),
        ('b', 0),
        ('a', 1),
        ('b', 1),
        ('a', 0),
    ];
    let run_batch = |operations: &[(char, usize)]| {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(operations.len() + 1));
        std::thread::scope(|scope| {
            let handles = operations
                .iter()
                .map(|(batch, index)| {
                    let root = if *batch == 'a' {
                        roots_a[*index].clone()
                    } else {
                        roots_b[*index].clone()
                    };
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        let result = LegacyOwnerAuditTempRootCleanup::cleanup_path(&root);
                        (*batch, *index, root, result)
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("cross-batch cleanup worker panicked"))
                .collect::<Vec<_>>()
        })
    };
    let first_results = run_batch(&operations);
    let snapshot_a = legacy_owner_temp_root_residues(&prefix_a);
    let snapshot_b = legacy_owner_temp_root_residues(&prefix_b);

    for root in [&blocked_a, &blocked_b] {
        let mut permissions = std::fs::metadata(root)
            .expect("read blocked cross-batch permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(root, permissions).expect("restore cross-batch permissions");
    }
    let second_results = run_batch(&operations);
    let final_a = legacy_owner_temp_root_residues(&prefix_a);
    let final_b = legacy_owner_temp_root_residues(&prefix_b);

    let first_failures = first_results
        .iter()
        .filter(|(_, _, _, result)| result.is_err())
        .collect::<Vec<_>>();
    assert_eq!(
        first_failures.len(),
        4,
        "both blocked roots need two failures"
    );
    assert_eq!(
        first_failures
            .iter()
            .filter(|(_, _, root, _)| root == &blocked_a)
            .count(),
        2,
        "batch A failure ownership drifted"
    );
    assert_eq!(
        first_failures
            .iter()
            .filter(|(_, _, root, _)| root == &blocked_b)
            .count(),
        2,
        "batch B failure ownership drifted"
    );
    assert!(
        first_failures.iter().all(|(_, _, root, result)| {
            result
                .as_ref()
                .expect_err("failed cross-batch cleanup must carry an error")
                .contains(&root.display().to_string())
        }),
        "cross-batch errors lost their own path"
    );
    assert!(
        first_results
            .iter()
            .filter(|(_, _, root, _)| root != &blocked_a && root != &blocked_b)
            .all(|(_, _, _, result)| result.is_ok()),
        "accessible cross-batch roots must tolerate duplicate cleanup"
    );
    assert_eq!(
        snapshot_a,
        vec![blocked_a.clone()],
        "batch A snapshot drifted"
    );
    assert_eq!(
        snapshot_b,
        vec![blocked_b.clone()],
        "batch B snapshot drifted"
    );
    assert!(
        second_results
            .iter()
            .all(|(_, _, _, result)| result.is_ok()),
        "cross-batch recovery cleanup must be idempotent"
    );
    assert!(
        final_a.is_empty(),
        "batch A recovery left residues: {final_a:?}"
    );
    assert!(
        final_b.is_empty(),
        "batch B recovery left residues: {final_b:?}"
    );
    assert_no_legacy_owner_temp_roots(&prefix_a);
    assert_no_legacy_owner_temp_roots(&prefix_b);
}

#[test]
fn legacy_owner_scalar_marker_recovery_preserves_static_summary() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/audit-mir-legacy-owners-static.sh");
    let run_audit = || {
        std::process::Command::new("bash")
            .arg(&script)
            .current_dir(&root)
            .output()
            .expect("run owner condition summary audit")
    };
    let summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.starts_with("production_path_roster=")
                    || line.starts_with("reachable_legacy_ast_owner_count=")
                    || line.starts_with("reachable_checked_proof_compatibility_owner_count=")
                    || line.starts_with("checked_native_boundary_count=")
                    || line.starts_with("owner_retirement_status=")
                    || line.starts_with("closed_scalar_")
                    || line.starts_with("scalar_evidence_marker_sequence_status=")
                    || line.starts_with("checked_program_surface_body_snapshot=")
                    || line.starts_with("production_raw_ast_call_sites=")
                    || line.starts_with("production_compile_func_legacy_call_sites=")
                    || line.starts_with("test_only_codegen_legacy_body_compiler=")
                    || line.starts_with("production_compile_generic_func_call_sites=")
                    || line.starts_with("test_only_codegen_func_generic_adapter_refs=")
                    || line.starts_with("test_only_codegen_legacy_adapter_refs=")
                    || line.starts_with("classified_production_path_count=")
                    || line.starts_with("static_audit_status=")
                    || line.starts_with("scalar_ffi_direct_expression_legacy_refs=")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let baseline = run_audit();
    assert!(
        baseline.status.success(),
        "baseline owner audit failed:\n{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let baseline_summary = summary(&baseline);
    assert!(baseline_summary
        .iter()
        .any(|line| line == "static_audit_status=ok"));
    assert!(baseline_summary.iter().any(|line| line == "production_path_roster=DefaultBytecodeAstCompatibility,NativeCheckedCompatibilityBoundary,CheckedVerifierActorProofCompatibility,FfiVerifierOnlyOwnerSlice"));
    assert!(
        baseline_summary.iter().any(|line| {
            line == "checked_program_surface_body_snapshot=physically-retired;source_registry=retained;has_imports=retained"
        }),
        "owner audit must keep the original surface-body snapshot physically retired"
    );

    let prefix = format!("mimi-legacy-owner-marker-summary-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    for batch in 0..3 {
        let residue = unique_legacy_owner_audit_temp_root(&format!("{prefix}{batch}"));
        let nested = residue.join("nested").join("evidence");
        std::fs::create_dir_all(&nested).expect("create summary residue root");
        std::fs::write(nested.join("snapshot"), b"summary residue").expect("write summary residue");
        let mut permissions = std::fs::metadata(&residue)
            .expect("read summary residue permissions")
            .permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&residue, permissions).expect("make summary residue unreadable");

        let error = LegacyOwnerAuditTempRootCleanup::cleanup_path(&residue)
            .expect_err("summary residue cleanup must fail closed");
        let snapshot = legacy_owner_temp_root_residues(&prefix);
        let mut permissions = std::fs::metadata(&residue)
            .expect("read blocked summary residue permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&residue, permissions)
            .expect("restore summary residue permissions");
        LegacyOwnerAuditTempRootCleanup::cleanup_path(&residue)
            .expect("summary residue recovery must succeed");
        LegacyOwnerAuditTempRootCleanup::cleanup_path(&residue)
            .expect("summary residue recovery must be idempotent");
        let after = run_audit();

        assert!(
            error.contains(&residue.display().to_string()),
            "summary failure omitted its residue path"
        );
        assert_eq!(
            snapshot,
            vec![residue.clone()],
            "summary batch {batch} residue snapshot drifted"
        );
        assert!(
            after.status.success(),
            "owner audit failed after batch {batch}"
        );
        assert_eq!(
            summary(&after),
            baseline_summary,
            "owner static summary drifted after recovery batch {batch}"
        );
        assert_no_legacy_owner_temp_roots(&prefix);
    }
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_path_roster_drift_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-roster-audit");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create roster audit temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into roster audit temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into roster audit temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "DefaultBytecodeAstCompatibility|default Legacy route in run/test/disasm; public raw-AST compiler API|tests/legacy_owner_boundary_api.rs|default_bytecode_compatibility_route_executes_its_named_legacy_disposition_without_artifacts|",
        "UnknownBytecodeCompatibility|default Legacy route in run/test/disasm; public raw-AST compiler API|tests/legacy_owner_boundary_api.rs|missing_bytecode_compatibility_route_probe|",
        1,
    );
    assert_ne!(
        source_script, tampered_script,
        "roster mutation must change one owner identity"
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write tampered roster audit script");
    let run = || {
        std::process::Command::new("bash")
            .arg(&script_path)
            .arg("--static-only")
            .current_dir(&temp_root)
            .output()
            .expect("run tampered roster audit script")
    };
    let output = run();
    let repeated = run();
    assert_eq!(
        output.status.code(),
        repeated.status.code(),
        "tampered owner roster exit code drifted across repeats"
    );
    assert_eq!(
        output.stdout, repeated.stdout,
        "tampered owner roster stdout drifted across repeats"
    );
    assert_eq!(
        output.stderr, repeated.stderr,
        "tampered owner roster stderr drifted across repeats"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "tampered owner roster must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stderr.contains(
        "owner_audit_error=UnknownBytecodeCompatibility direct_route_probe_missing=tests/legacy_owner_boundary_api.rs::missing_bytecode_compatibility_route_probe"
    ), "tampered owner roster omitted the precise missing-probe diagnostic:\n{stderr}");
    assert!(
        stderr.contains("owner_audit_error=production_path_roster_mismatch expected=DefaultBytecodeAstCompatibility,NativeCheckedCompatibilityBoundary,CheckedVerifierActorProofCompatibility,FfiVerifierOnlyOwnerSlice actual=UnknownBytecodeCompatibility,NativeCheckedCompatibilityBoundary,CheckedVerifierActorProofCompatibility,FfiVerifierOnlyOwnerSlice"),
        "tampered owner roster must fail the exact production roster check:\n{stderr}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("production_path_roster=UnknownBytecodeCompatibility,NativeCheckedCompatibilityBoundary,CheckedVerifierActorProofCompatibility,FfiVerifierOnlyOwnerSlice"),
        "failed inventory must show the observed roster instead of restating the expected roster"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("static_audit_status=failed"),
        "tampered static roster must report only static failure"
    );
    std::fs::remove_dir_all(&temp_root).expect("remove roster audit temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_audit_temp_roots_are_unique_under_concurrency() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    let roots = Arc::new(Mutex::new(HashSet::new()));
    let workers = (0..8)
        .map(|_| {
            let roots = Arc::clone(&roots);
            std::thread::spawn(move || {
                for _ in 0..32 {
                    let root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-concurrent");
                    assert!(
                        roots.lock().expect("lock temp root set").insert(root),
                        "concurrent owner audit temp root collided"
                    );
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker
            .join()
            .expect("temp root uniqueness worker must finish");
    }
    assert_eq!(
        roots.lock().expect("lock final temp root set").len(),
        8 * 32,
        "all concurrent owner audit temp roots must be unique"
    );
}

#[test]
fn legacy_owner_audit_temp_roots_cleanup_under_concurrency() {
    use std::sync::{Arc, Mutex};
    let created = Arc::new(Mutex::new(Vec::new()));
    let workers = (0..8)
        .map(|_| {
            let created = Arc::clone(&created);
            std::thread::spawn(move || {
                for _ in 0..16 {
                    let path = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-cleanup");
                    std::fs::create_dir_all(&path).expect("create concurrent cleanup root");
                    {
                        let _cleanup = LegacyOwnerAuditTempRootCleanup(path.clone());
                        assert!(path.is_dir(), "concurrent cleanup root must exist in scope");
                        created
                            .lock()
                            .expect("lock created cleanup roots")
                            .push(path.clone());
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker
            .join()
            .expect("concurrent cleanup worker must finish");
    }
    let created = created.lock().expect("lock final cleanup roots");
    assert_eq!(
        created.len(),
        8 * 16,
        "all concurrent cleanup roots must be recorded"
    );
    assert!(
        created.iter().all(|path| !path.exists()),
        "all concurrent owner audit temp roots must be removed"
    );
}

#[test]
fn legacy_owner_audit_temp_root_cleanup_runs_on_scope_exit() {
    let path = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-cleanup");
    std::fs::create_dir_all(&path).expect("create cleanup probe root");
    {
        let _cleanup = LegacyOwnerAuditTempRootCleanup(path.clone());
        assert!(path.is_dir(), "cleanup probe root must exist in its scope");
    }
    assert!(
        !path.exists(),
        "owner audit temp root must be removed when the guard leaves scope"
    );
}

#[test]
fn legacy_owner_audit_temp_root_prefix_scan_is_clean_after_probe() {
    let prefix = format!("mimi-legacy-owner-scan-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let path = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-scan");
    std::fs::create_dir_all(&path).expect("create prefix scan probe root");
    {
        let _cleanup = LegacyOwnerAuditTempRootCleanup(path.clone());
        assert!(path.is_dir(), "prefix scan probe root must exist in scope");
    }
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_prefix_scan_detects_foreign_process_residue() {
    let prefix = "mimi-legacy-owner-cross-process-";
    assert_no_legacy_owner_temp_roots(prefix);
    let foreign =
        std::env::temp_dir().join(format!("{prefix}42424242-stale-{}", std::process::id()));
    std::fs::create_dir_all(&foreign).expect("create foreign-process residue probe");
    let residues = legacy_owner_temp_root_residues(prefix);
    assert_eq!(residues, vec![foreign.clone()]);
    std::fs::remove_dir_all(&foreign).expect("remove foreign-process residue probe");
    assert_no_legacy_owner_temp_roots(prefix);
}

#[test]
fn legacy_owner_audit_prefix_scan_keeps_non_directory_residue_fail_closed() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let prefix = format!("mimi-legacy-owner-nondirectory-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let file = std::env::temp_dir().join(format!("{prefix}file"));
    let target = std::env::temp_dir().join(format!("{prefix}target"));
    let link = std::env::temp_dir().join(format!("{prefix}link"));
    let unreadable = std::env::temp_dir().join(format!("{prefix}unreadable"));
    std::fs::write(&file, b"stale owner audit file").expect("create file residue probe");
    std::fs::create_dir(&target).expect("create symlink target residue probe");
    symlink(&target, &link).expect("create symlink residue probe");
    std::fs::create_dir(&unreadable).expect("create unreadable residue probe");
    let mut permissions = std::fs::metadata(&unreadable)
        .expect("read unreadable residue permissions")
        .permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&unreadable, permissions).expect("make residue directory unreadable");

    let expected = vec![
        file.clone(),
        link.clone(),
        target.clone(),
        unreadable.clone(),
    ];
    let mut expected_sorted = expected.clone();
    expected_sorted.sort();
    assert_eq!(legacy_owner_temp_root_residues(&prefix), expected_sorted);
    assert!(
        std::fs::remove_dir_all(&file).is_err(),
        "directory-only cleanup must not remove a regular file"
    );
    assert!(
        std::fs::remove_dir_all(&link).is_ok(),
        "directory cleanup may remove a symlink without following it"
    );
    assert!(
        target.is_dir(),
        "symlink cleanup must not remove the target directory"
    );
    expected_sorted.retain(|path| path != &link);
    assert_eq!(
        legacy_owner_temp_root_residues(&prefix),
        expected_sorted,
        "failed regular-file cleanup must remain visible to the residue scanner"
    );

    let mut permissions = std::fs::metadata(&unreadable)
        .expect("restore unreadable residue permissions")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&unreadable, permissions)
        .expect("restore residue directory permissions");
    std::fs::remove_file(&file).expect("remove file residue probe");
    std::fs::remove_dir(&target).expect("remove symlink target residue probe");
    std::fs::remove_dir(&unreadable).expect("remove unreadable residue probe");
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_prefix_scan_propagates_entry_errors() {
    let prefix = "mimi-legacy-owner-entry-error-";
    let entries = vec![
        Ok(std::env::temp_dir().join(format!("{prefix}before"))),
        Err("permission denied"),
        Ok(std::env::temp_dir().join(format!("{prefix}after"))),
    ];
    let error = collect_legacy_owner_temp_root_residues(entries, prefix)
        .expect_err("entry read failure must fail closed");
    assert_eq!(error, "permission denied");
}

#[test]
fn legacy_owner_audit_prefix_scan_observes_interleaved_workers_deterministically() {
    use std::sync::{Arc, Barrier, Mutex};

    let prefix = format!("mimi-legacy-owner-interleave-{}", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let ready = Arc::new(Barrier::new(9));
    let release = Arc::new(Barrier::new(9));
    let paths = Arc::new(Mutex::new(Vec::new()));
    let workers = (0..8)
        .map(|_| {
            let ready = Arc::clone(&ready);
            let release = Arc::clone(&release);
            let paths = Arc::clone(&paths);
            let prefix = prefix.clone();
            std::thread::spawn(move || {
                let path = unique_legacy_owner_audit_temp_root(&prefix);
                std::fs::create_dir_all(&path).expect("create interleaved worker root");
                let cleanup = LegacyOwnerAuditTempRootCleanup(path.clone());
                paths
                    .lock()
                    .expect("lock interleaved worker paths")
                    .push(path);
                ready.wait();
                release.wait();
                drop(cleanup);
            })
        })
        .collect::<Vec<_>>();
    ready.wait();
    let mut expected = paths
        .lock()
        .expect("lock interleaved worker paths for scan")
        .clone();
    expected.sort();
    assert_eq!(
        legacy_owner_temp_root_residues(&prefix),
        expected,
        "scanner must observe every worker root at the synchronized creation point"
    );
    release.wait();
    for worker in workers {
        worker
            .join()
            .expect("interleaved owner audit worker must finish");
    }
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_residue_diagnostics_include_snapshot_context() {
    let prefix = format!("mimi-legacy-owner-diagnostic-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let file = std::env::temp_dir().join(format!("{prefix}file"));
    let directory = std::env::temp_dir().join(format!("{prefix}directory"));
    std::fs::write(&file, b"stale owner audit diagnostic").expect("create diagnostic file");
    std::fs::create_dir(&directory).expect("create diagnostic directory");

    let error = no_legacy_owner_temp_root_residues(&prefix)
        .expect_err("non-empty residue snapshot must return a diagnostic");
    assert!(
        error.contains(&prefix),
        "diagnostic must include its prefix: {error}"
    );
    assert!(
        error.contains(&file.display().to_string()),
        "diagnostic must include the regular-file residue: {error}"
    );
    assert!(
        error.contains(&directory.display().to_string()),
        "diagnostic must include the directory residue: {error}"
    );

    let cleanup_error = LegacyOwnerAuditTempRootCleanup::cleanup_path(&file)
        .expect_err("directory cleanup must report regular-file failures");
    assert!(
        cleanup_error.contains(&file.display().to_string()),
        "cleanup diagnostic must include the failing path: {cleanup_error}"
    );
    assert!(
        file.is_file(),
        "failed cleanup must preserve the file residue"
    );
    std::fs::remove_file(&file).expect("remove diagnostic file");
    std::fs::remove_dir(&directory).expect("remove diagnostic directory");
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_cleanup_race_classifies_type_change_and_missing_path() {
    let prefix = format!("mimi-legacy-owner-race-{}", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let path = unique_legacy_owner_audit_temp_root(&prefix);
    std::fs::create_dir(&path).expect("create race directory residue");
    std::fs::remove_dir(&path).expect("remove race directory before replacement");
    std::fs::write(&path, b"replacement file residue").expect("replace race path with file");

    let type_error = LegacyOwnerAuditTempRootCleanup::cleanup_path(&path)
        .expect_err("directory cleanup must classify a path replaced by a file");
    assert!(
        type_error.contains(&path.display().to_string()),
        "type-change diagnostic must include the raced path: {type_error}"
    );
    assert!(
        path.is_file(),
        "type-change cleanup must preserve the replacement file"
    );
    assert_eq!(legacy_owner_temp_root_residues(&prefix), vec![path.clone()]);

    std::fs::remove_file(&path).expect("remove replacement file residue");
    assert!(
        LegacyOwnerAuditTempRootCleanup::cleanup_path(&path).is_ok(),
        "cleanup must be idempotent after an external deletion race"
    );
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_cleanup_concurrent_errors_and_missing_paths_are_stable() {
    let prefix = format!("mimi-legacy-owner-concurrent-race-{}", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let file = unique_legacy_owner_audit_temp_root(&prefix);
    std::fs::write(&file, b"concurrent file residue").expect("create concurrent file residue");
    let file_workers = (0..8)
        .map(|_| {
            let file = file.clone();
            std::thread::spawn(move || {
                LegacyOwnerAuditTempRootCleanup::cleanup_path(&file)
                    .expect_err("regular-file cleanup must fail for every concurrent worker")
            })
        })
        .collect::<Vec<_>>();
    let file_errors = file_workers
        .into_iter()
        .map(|worker| worker.join().expect("file cleanup worker must finish"))
        .collect::<Vec<_>>();
    assert!(!file_errors.is_empty());
    assert!(
        file_errors
            .iter()
            .all(|error| error == &file_errors[0] && error.contains(&file.display().to_string())),
        "concurrent type errors must be stable and path-specific: {file_errors:?}"
    );
    assert!(
        file.is_file(),
        "concurrent failed cleanup must preserve the file"
    );
    std::fs::remove_file(&file).expect("remove concurrent file residue");

    let directory = unique_legacy_owner_audit_temp_root(&prefix);
    std::fs::create_dir(&directory).expect("create concurrent directory residue");
    let directory_workers = (0..8)
        .map(|_| {
            let directory = directory.clone();
            std::thread::spawn(move || {
                LegacyOwnerAuditTempRootCleanup::cleanup_path(&directory)
                    .expect("missing directory after a concurrent delete is idempotent")
            })
        })
        .collect::<Vec<_>>();
    for worker in directory_workers {
        worker.join().expect("directory cleanup worker must finish");
    }
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_concurrent_failure_snapshot_is_path_ordered() {
    use std::sync::{Arc, Mutex};

    let prefix = format!("mimi-legacy-owner-failure-snapshot-{}", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let paths = (0..8)
        .map(|_| {
            let path = unique_legacy_owner_audit_temp_root(&prefix);
            std::fs::write(&path, b"failure snapshot residue")
                .expect("create failure snapshot file");
            path
        })
        .collect::<Vec<_>>();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let workers = paths
        .iter()
        .map(|path| {
            let path = path.clone();
            let errors = Arc::clone(&errors);
            std::thread::spawn(move || {
                let error = LegacyOwnerAuditTempRootCleanup::cleanup_path(&path)
                    .expect_err("regular-file cleanup must fail for snapshot worker");
                errors
                    .lock()
                    .expect("lock failure snapshot errors")
                    .push(error);
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().expect("failure snapshot worker must finish");
    }

    let mut expected = paths.clone();
    expected.sort();
    let snapshot = legacy_owner_temp_root_residues(&prefix);
    assert_eq!(snapshot, expected, "residue snapshot must be path ordered");
    let mut observed_errors = errors.lock().expect("lock final snapshot errors").clone();
    observed_errors.sort_by_key(|error| {
        error
            .strip_prefix("remove owner audit temp root ")
            .and_then(|rest| rest.rsplit_once(": "))
            .map(|(path, _)| std::path::PathBuf::from(path))
    });
    assert_eq!(observed_errors.len(), expected.len());
    for (error, path) in observed_errors.iter().zip(&expected) {
        assert!(
            error.contains(&path.display().to_string()),
            "ordered cleanup error must identify its matching path: {error}"
        );
    }

    for path in paths {
        std::fs::remove_file(path).expect("remove failure snapshot file");
    }
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_snapshot_handles_encoded_names_and_repeated_reads() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let prefix = format!("mimi-legacy-owner-encoding-{}-", std::process::id());
    assert_no_legacy_owner_temp_roots(&prefix);
    let unicode = std::env::temp_dir().join(format!("{prefix}路径-残留"));
    let mut invalid_name = prefix.as_bytes().to_vec();
    invalid_name.extend_from_slice(b"invalid-");
    invalid_name.push(0xff);
    let invalid = std::env::temp_dir().join(OsString::from_vec(invalid_name));
    std::fs::write(&unicode, b"unicode residue").expect("create Unicode residue");
    std::fs::write(&invalid, b"invalid UTF-8 residue").expect("create encoded residue");

    let mut expected = vec![unicode.clone(), invalid.clone()];
    expected.sort();
    let snapshots = (0..5)
        .map(|_| legacy_owner_temp_root_residues(&prefix))
        .collect::<Vec<_>>();
    assert!(
        snapshots.iter().all(|snapshot| snapshot == &expected),
        "repeated encoded-name snapshots must be stable: {snapshots:?}"
    );

    let cleanup_error = LegacyOwnerAuditTempRootCleanup::cleanup_path(&invalid)
        .expect_err("directory cleanup must reject the encoded regular file");
    assert!(
        cleanup_error.contains("remove owner audit temp root"),
        "encoded cleanup failure must retain its diagnostic category: {cleanup_error}"
    );
    assert!(
        invalid.is_file(),
        "failed encoded cleanup must preserve the file"
    );
    std::fs::remove_file(&invalid).expect("remove encoded residue");
    std::fs::remove_file(&unicode).expect("remove Unicode residue");
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn legacy_owner_audit_permission_recovery_preserves_snapshot_order() {
    use std::os::unix::fs::PermissionsExt;

    let prefix = format!(
        "mimi-legacy-owner-permission-recovery-{}",
        std::process::id()
    );
    assert_no_legacy_owner_temp_roots(&prefix);
    let root = unique_legacy_owner_audit_temp_root(&prefix);
    let child = root.join("nested").join("residue");
    std::fs::create_dir_all(child.parent().expect("nested residue parent"))
        .expect("create permission recovery directory");
    std::fs::write(&child, b"permission recovery residue")
        .expect("create permission recovery child");
    let mut permissions = std::fs::metadata(&root)
        .expect("read permission recovery permissions")
        .permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&root, permissions).expect("make permission recovery root unreadable");

    let cleanup_error = LegacyOwnerAuditTempRootCleanup::cleanup_path(&root)
        .expect_err("unreadable owner audit root must fail closed");
    assert!(
        cleanup_error.contains(&root.display().to_string()),
        "permission failure must identify the root path: {cleanup_error}"
    );
    let expected = vec![root.clone()];
    let snapshots = (0..3)
        .map(|_| legacy_owner_temp_root_residues(&prefix))
        .collect::<Vec<_>>();
    assert!(
        snapshots.iter().all(|snapshot| snapshot == &expected),
        "permission failure snapshots must remain stable: {snapshots:?}"
    );

    let mut permissions = std::fs::metadata(&root)
        .expect("restore permission recovery permissions")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&root, permissions)
        .expect("restore permission recovery root permissions");
    LegacyOwnerAuditTempRootCleanup::cleanup_path(&root)
        .expect("permission recovery retry must remove the complete root");
    assert!(
        LegacyOwnerAuditTempRootCleanup::cleanup_path(&root).is_ok(),
        "permission recovery cleanup must be idempotent after removal"
    );
    assert_no_legacy_owner_temp_roots(&prefix);
}

#[test]
fn script_syntax_gen_stdlib_docs_py() {
    // Fixed: output path had one `..` too many, writing stdlib_api.md into
    // the repo's PARENT directory instead of in-repo mimispecref/.
    // `bash -n` cannot parse Python; use python3 compile() instead
    // (python3 is a hard repo-tooling dependency, AGENTS.md §12/§15.3).
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/gen_stdlib_docs.py");
    assert!(
        path.is_file(),
        "gen_stdlib_docs.py missing at {}",
        path.display()
    );
    const PY_SYNTAX_CHECK: &str =
        "import sys; compile(open(sys.argv[1], encoding='utf-8').read(), sys.argv[1], 'exec')";
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(PY_SYNTAX_CHECK)
        .arg(&path)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `python3`: {}", e));
    assert!(
        out.status.success(),
        "gen_stdlib_docs.py: python syntax check failed (exit {:?})\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
