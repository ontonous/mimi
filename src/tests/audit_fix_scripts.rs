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
    let temp_root = std::env::temp_dir().join(format!(
        "mimi-legacy-owner-audit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos()
    ));
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
}

#[test]
fn legacy_owner_condition_digest_drift_fails_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = std::env::temp_dir().join(format!(
        "mimi-legacy-owner-digest-audit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos()
    ));
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
