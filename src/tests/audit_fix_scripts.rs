//! Wave-1 audit-fix regression tests — scripts.
//! Findings: devdocs/full-audit-2026-08-05.md (2026-08-05 full audit).
//! Discipline: each fix must carry a regression test here; L1 divergences
//! assert BOTH sides (VM via run_source*/bytecode helpers, codegen via compile_and_run).

// ---------------------------------------------------------------------------
// Full audit §13: CI/test script control-flow fixes (vacuous pass checks).
// The tests below assert *syntactic* validity of each edited script
// (`bash -n` for shell, python compile() for the python generator); they
// never execute the scripts. Their job is to catch future syntax breakage
// introduced while editing script logic.
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
fn legacy_owner_condition_fingerprint_is_executable_and_repeatable() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("scripts/audit-mir-legacy-owners.sh");
    let run = || {
        std::process::Command::new("bash")
            .arg(&path)
            .current_dir(&root)
            .output()
            .expect("legacy owner audit must execute")
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
    let fingerprints = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains("owner_deletion_condition_digest=")
                    || line.starts_with("owner_deletion_condition_set_digest=")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let first_fingerprints = fingerprints(&first);
    assert_eq!(
        first_fingerprints.len(),
        5,
        "legacy owner audit must expose four owner digests and one set digest"
    );
    assert_eq!(
        first_fingerprints,
        fingerprints(&second),
        "legacy owner condition fingerprints changed across repeated audits"
    );
    assert!(
        String::from_utf8_lossy(&first.stdout).contains("audit_status=ok"),
        "legacy owner audit omitted its success marker"
    );
}

#[test]
fn legacy_owner_reachability_report_stays_conservative() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("scripts/audit-mir-legacy-owners.sh");
    let output = std::process::Command::new("bash")
        .arg(&path)
        .current_dir(&root)
        .output()
        .expect("legacy owner audit must execute");
    assert!(
        output.status.success(),
        "legacy owner audit failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for (owner, dependency_class, evidence_marker) in [
        (
            "CodegenLegacyRemainder",
            "legacy-codegen-remainder",
            "LegacyBodyConsumer::CodegenLegacyRemainder",
        ),
        (
            "FlowVerifierCompatibility",
            "flow-body-compatibility",
            "LegacyBodyConsumer::FlowVerifierCompatibility",
        ),
        (
            "FfiVerifierCompatibility",
            "ffi-declaration-compatibility",
            "LegacyBodyConsumer::FfiVerifierCompatibility",
        ),
        (
            "DualVerifierCompatibility",
            "secondary-flow-vir-compatibility",
            "LegacyBodyConsumer::DualVerifierCompatibility",
        ),
    ] {
        let evidence_prefix = format!("owner={owner} evidence_scope=");
        let evidence = stdout
            .lines()
            .find(|line| line.starts_with(&evidence_prefix))
            .unwrap_or_else(|| panic!("missing evidence report for {owner}"));
        assert!(
            evidence.contains("closed scalar bypass"),
            "owner evidence for {owner} must document the closed scalar bypass"
        );
        assert!(
            evidence.contains(&format!("evidence_marker={evidence_marker}")),
            "owner evidence for {owner} must expose its legacy access marker"
        );
        let accessor = format!("owner={owner} production_accessor_call_sites=1");
        assert!(
            stdout.lines().any(|line| line == accessor),
            "owner {owner} must retain exactly one production accessor"
        );
        let status_prefix = format!("owner={owner} status=retained");
        let status = stdout
            .lines()
            .find(|line| line.starts_with(&status_prefix))
            .unwrap_or_else(|| panic!("missing retained status for {owner}"));
        assert!(
            status.contains(&format!("dependency_class={dependency_class}")),
            "owner {owner} dependency class drifted: {status}"
        );
        assert!(
            status.contains("owner_deletion_ready=0"),
            "owner {owner} must remain fail-closed for deletion: {status}"
        );
    }
    for expected in [
        "closed_scalar_zero_owner_evidence=",
        "closed_scalar_zero_owner_evidence_marker=test_legacy_body_access().is_empty()",
        "closed_scalar_cli_evidence=",
        "production_legacy_body_file_call_sites=4",
        "production_raw_ast_call_sites=0",
        "production_compile_func_legacy_call_sites=8",
        "scalar_ffi_direct_expression_legacy_refs=0",
        "owner_count=4",
        "audit_status=ok",
    ] {
        assert!(
            stdout.lines().any(|line| line.starts_with(expected)),
            "legacy owner audit omitted required marker {expected}"
        );
    }
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
            "compile_checked_tags_unmigrated_generic_body_with_legacy_owner",
            "compatibility_verifier_access_is_explicitly_tagged",
            "ffi_checked_preserves_legacy_for_unmigrated_string_contract",
        ] {
            let output = std::process::Command::new(&cargo)
                .args([
                    "test",
                    "--features",
                    "llvm18-host-dynamic",
                    "--lib",
                    test_name,
                    "--",
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
                    line.starts_with("test result: ok.") && line.contains("0 failed")
                }),
                "owner evidence test {test_name} (round {round}) omitted a passing result:\n{stdout}"
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
        "LegacyBodyConsumer::CodegenLegacyRemainder",
        "LegacyBodyConsumer::MissingMarker",
        1,
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write tampered audit script");
    let output = std::process::Command::new("bash")
        .arg(&script_path)
        .current_dir(&temp_root)
        .output()
        .expect("run tampered legacy owner audit");
    let repeated = std::process::Command::new("bash")
        .arg(&script_path)
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
        stderr.contains("evidence_test_missing_marker=compile_checked_tags_unmigrated_generic_body_with_legacy_owner"),
        "tampered owner evidence omitted marker failure:\n{stderr}"
    );
    std::fs::remove_dir_all(&temp_root).expect("remove audit temp root");
    let restored = std::process::Command::new("bash")
        .arg(root.join("scripts/audit-mir-legacy-owners.sh"))
        .current_dir(&root)
        .output()
        .expect("rerun restored legacy owner audit");
    assert!(
        restored.status.success(),
        "restored owner audit failed after negative probe:\n{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert!(
        String::from_utf8_lossy(&restored.stdout).contains("audit_status=ok"),
        "restored owner audit omitted its success marker"
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
        .current_dir(&temp_root)
        .output()
        .expect("run missing scalar assertion audit");
    assert!(
        !missing.status.success(),
        "missing scalar zero-owner assertion must fail closed:\n{}",
        String::from_utf8_lossy(&missing.stdout)
    );
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains(
            "owner_audit_error=closed_scalar_zero_owner_evidence_missing_empty_legacy_assertion"
        ),
        "missing scalar assertion omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    std::fs::write(
        &scalar_fixture,
        "fn scalar_ffi_c_abi_and_side_effect_order_match_three_consumers() {\n}\n#[test]\nfn forged_marker_outside_scalar_test() { assert!(crate::core::CheckedProgram::test_legacy_body_access().is_empty()); }\n",
    )
    .expect("write scalar fixture with forged outside marker");
    let forged = std::process::Command::new("bash")
        .arg(&script_path)
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
            "owner_audit_error=closed_scalar_zero_owner_evidence_missing_empty_legacy_assertion"
        ),
        "forged outside marker omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&forged.stderr)
    );
    std::fs::remove_dir_all(&temp_root).expect("remove scalar audit temp root");
    drop(cleanup);
}

#[test]
fn legacy_owner_condition_digest_drift_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-digest-audit");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create digest audit temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into digest audit temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into digest audit temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let expected_digest = "2568b029a170043114b68ea0d35f9061617cc969ea2f9c8b7288d3d7f4f5e40c";
    let tampered_script = source_script.replacen(
        expected_digest,
        "0000000000000000000000000000000000000000000000000000000000000000",
        1,
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write tampered digest audit script");
    let output = std::process::Command::new("bash")
        .arg(&script_path)
        .current_dir(&temp_root)
        .output()
        .expect("run tampered digest audit script");
    let repeated = std::process::Command::new("bash")
        .arg(&script_path)
        .current_dir(&temp_root)
        .output()
        .expect("repeat tampered digest audit script");
    assert_eq!(
        output.status.code(),
        repeated.status.code(),
        "tampered condition digest exit code drifted across repeats"
    );
    assert_eq!(
        output.stdout, repeated.stdout,
        "tampered condition digest stdout drifted across repeats"
    );
    assert_eq!(
        output.stderr, repeated.stderr,
        "tampered condition digest stderr drifted across repeats"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "tampered condition digest must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains(
            "owner_audit_error=CodegenLegacyRemainder owner_deletion_condition_digest expected=0000000000000000000000000000000000000000000000000000000000000000"
        ),
        "tampered condition digest omitted expected-value diagnostic:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("actual={expected_digest}")),
        "tampered condition digest omitted actual-value diagnostic:\n{stderr}"
    );
    std::fs::remove_dir_all(&temp_root).expect("remove digest audit temp root");
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
    observed_errors.sort();
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
