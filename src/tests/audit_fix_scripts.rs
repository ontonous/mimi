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
    for (owner, dependency_class, evidence_marker, accessor_context, guard_name, guard_context) in [
        (
            "CodegenLegacyRemainder",
            "legacy-codegen-remainder",
            "LegacyBodyConsumer::CodegenLegacyRemainder",
            "src/codegen/compile.rs::fn compile_file_with_resolved(",
            "try_compile_exact_migrated_mir_island",
            "src/codegen/compile.rs::pub fn compile_checked(",
        ),
        (
            "FlowVerifierCompatibility",
            "flow-body-compatibility",
            "LegacyBodyConsumer::FlowVerifierCompatibility",
            "src/verifier/mod.rs::pub fn verify_checked(",
            "verify_closed_mir_program",
            "src/verifier/mod.rs::pub fn verify_checked(",
        ),
        (
            "FfiVerifierCompatibility",
            "ffi-declaration-compatibility",
            "LegacyBodyConsumer::FfiVerifierCompatibility",
            "src/verifier/mod.rs::fn verify_ffi_checked_with_source_hash(",
            "materialize_closed_mir_island",
            "src/verifier/mod.rs::fn verify_ffi_checked_with_source_hash(",
        ),
        (
            "DualVerifierCompatibility",
            "secondary-flow-vir-compatibility",
            "LegacyBodyConsumer::DualVerifierCompatibility",
            "src/verifier/mod.rs::pub fn verify_checked_dual(",
            "verify_closed_mir_program",
            "src/verifier/mod.rs::pub fn verify_checked_dual(",
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
        let context = format!("owner={owner} accessor_context={accessor_context}");
        assert!(
            stdout.lines().any(|line| line == context),
            "owner {owner} accessor context drifted: expected {context}"
        );
        let guard =
            format!("owner={owner} closed_route_guard={guard_name} context={guard_context}");
        assert!(
            stdout.lines().any(|line| line == guard),
            "owner {owner} closed route guard drifted: expected {guard}"
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
        "closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]",
        "closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent",
        "production_legacy_body_file_call_sites=4",
        "production_raw_ast_call_sites=0",
        "production_compile_func_legacy_call_sites=8",
        "scalar_ffi_direct_expression_legacy_refs=0",
        "scalar_route_receipt_binding=CanonicalMirRouteProfile::ScalarFfi->scalar-ffi-v1 consumer=src/main/canonical_dispatch.rs::fn select_scalar_ffi_route(",
        "consumer_receipt_binding=native-direct-v1 consumer=src/codegen/compile.rs::pub fn compile_checked(",
        "consumer_receipt_binding=verify-ffi-v1 consumer=src/verifier/mod.rs::fn verify_ffi_checked_with_source_hash(",
        "consumer_receipt_provenance_binding=native-direct-v1 graph=canonical provenance=canonical-mir-graph consumer=src/codegen/compile.rs::pub fn compile_checked(",
        "consumer_receipt_provenance_binding=verify-ffi-v1 graph=canonical provenance=canonical-mir-graph+source-hash consumer=src/verifier/mod.rs::fn verify_ffi_checked_with_source_hash(",
        "consumer_receipt_profile_binding=verify-ffi-v1 profile=CanonicalMirRouteProfile::ScalarFfi consumer=src/verifier/mod.rs::fn verify_ffi_checked_with_source_hash(",
        "source_hash_entry_binding=blake3-source-hash consumer=src/verifier/mod.rs::pub fn verify_ffi_source(",
        "source_hash_parameter_binding=declared-and-forwarded consumer=src/verifier/mod.rs::fn verify_ffi_checked_with_source_hash(",
        "mir_verifier_source_provenance_binding=single-source-hash consumer=src/verifier/mod.rs::pub fn verify_ffi_mir_with_source_hash(",
        "mir_result_identity_binding=source-hash+mir-hash+route-receipt consumer=src/verifier/mod.rs::fn validate_mir_result_provenance(",
        "mir_route_receipt_validation_binding=validate-against-program consumer=src/verifier/mod.rs::pub fn verify_mir_with_route_receipt(",
        "mir_route_receipt_validation_binding=validate-against-program consumer=src/verifier/mod.rs::pub fn verify_ffi_mir_with_route_receipt(",
        "mir_route_manifest_replay_binding=manifest-parse+direct-adapter consumer=src/verifier/mod.rs::pub fn verify_mir_with_route_manifest(",
        "mir_route_manifest_replay_binding=manifest-parse+direct-adapter consumer=src/verifier/mod.rs::pub fn verify_ffi_mir_with_route_manifest(",
        "mir_ffi_capability_order_binding=capability-before-execution consumer=src/verifier/mod.rs::pub fn verify_ffi_mir_with_source_hash(",
        "mir_route_receipt_order_binding=receipt-before-adapter consumer=src/verifier/mod.rs::pub fn verify_mir_with_route_receipt(",
        "mir_route_receipt_order_binding=receipt-before-adapter consumer=src/verifier/mod.rs::pub fn verify_ffi_mir_with_route_receipt(",
        "mir_cli_verifier_route_binding=receipt-bearing-adapter consumer=src/main/verify.rs",
        "mir_cli_error_boundary_binding=shared-format-cli-error consumer=src/main.rs",
        "mir_cli_disasm_route_binding=shared-format-cli-error consumer=src/main/disasm_cmd.rs",
        "mir_cli_route_formatter_provenance_binding=registry-code+mir.route-origin consumer=src/main.rs",
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
fn legacy_owner_accessor_and_closed_route_guard_drift_fail_closed() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp_root = unique_legacy_owner_audit_temp_root("mimi-legacy-owner-context");
    let cleanup = LegacyOwnerAuditTempRootCleanup(temp_root.clone());
    std::fs::create_dir_all(temp_root.join("scripts")).expect("create context audit temp root");
    std::os::unix::fs::symlink(root.join("src"), temp_root.join("src"))
        .expect("link source tree into context audit temp root");
    std::os::unix::fs::symlink(root.join("tests"), temp_root.join("tests"))
        .expect("link integration tests into context audit temp root");
    let source_script = std::fs::read_to_string(root.join("scripts/audit-mir-legacy-owners.sh"))
        .expect("read legacy owner audit script");
    let tampered_script = source_script.replacen(
        "    'fn compile_file_with_resolved(' \\\n",
        "    'fn missing_compile_context(' \\\n",
        1,
    );
    assert!(
        source_script != tampered_script,
        "context fixture must replace the expected function marker"
    );
    let script_path = temp_root.join("scripts/audit-mir-legacy-owners.sh");
    std::fs::write(&script_path, tampered_script).expect("write tampered context audit script");
    let output = std::process::Command::new("bash")
        .arg(&script_path)
        .current_dir(&temp_root)
        .output()
        .expect("run tampered context audit");
    assert!(
        !output.status.success(),
        "tampered accessor context must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("owner_audit_error=CodegenLegacyRemainder accessor_context_missing="),
        "context drift omitted fail-closed diagnostic:\n{}",
        String::from_utf8_lossy(&output.stderr)
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
        "    ScalarFfi\n\n# The AST-free bytecode adapter must retain the route receipt that admitted",
        "    ScalarCollection\n\n# The AST-free bytecode adapter must retain the route receipt that admitted",
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
        "    'validate_mir_capabilities(program)' \\\n",
        "    'missing_mir_capability_gate(program)' \\\n",
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
        "    '.validate_against_program(program)' \\\n    'let mut results = verify_mir(program, source_hash.clone())?'",
        "    'let mut results = verify_mir(program, source_hash.clone())?' \\\n    '.validate_against_program(program)'",
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
        .arg(root.join("scripts/audit-mir-legacy-owners.sh"))
        .current_dir(&root)
        .output()
        .expect("rerun restored context audit");
    assert!(
        restored.status.success(),
        "restored owner audit failed after context probe:\n{}",
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
fn legacy_owner_audit_pins_bytecode_route_receipt_provenance() {
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
    assert!(
        stdout.contains(
            "bytecode_route_receipt_binding=compile_mir_program_inner(program, Some(receipt))"
        ),
        "legacy owner audit must pin route receipt hand-off from the canonical bytecode entry"
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
            "bytecode_route_receipt_vm_guard=program-anchor+identity-consistency+manifest-replay consumer=src/interp/bytecode/vm.rs"
        ),
        "legacy owner audit must pin the VM boundary receipt guard"
    );
    assert!(
        stdout.contains(
            "native_route_receipt_anchor=program-anchor+semantic-replay consumer=src/codegen/mir/eligibility.rs"
        ),
        "legacy owner audit must pin the native program-level route receipt anchor"
    );
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
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n    for explicit_mir in [false, true] {}\n    assert!(!run_stderr.contains(\"canonical route disposition: legacy\"));\n    let _ = [\"mir_ffi_i32\", \"mir_ffi_i64\", \"mir_ffi_bool\", \"mir_ffi_f64\"];\n}\n#[test]\n",
    )
    .expect("write CLI fixture missing ABI symbol");
    let missing_abi = std::process::Command::new("bash")
        .arg(&script_path)
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
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n    for explicit_mir in [false, true] {}\n    assert!(!run_stderr.contains(\"canonical route disposition: legacy\"));\n    let _ = [\"mir_ffi_i32\", \"mir_ffi_i64\", \"mir_ffi_bool\", \"mir_ffi_f64\"];\n}\n#[test]\nfn forged_abi_outside_cli_test() { let _ = \"mir_ffi_store\"; }\n",
    )
    .expect("write CLI fixture with forged ABI symbol outside test");
    let forged_abi = std::process::Command::new("bash")
        .arg(&script_path)
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
        "fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {\n    for contracts in [true, false] {}\n    for explicit_mir in [false, true] {}\n    let _ = [\"mir_ffi_i32\", \"mir_ffi_i64\", \"mir_ffi_bool\", \"mir_ffi_f64\", \"mir_ffi_store\"];\n}\n#[test]\n",
    )
    .expect("write CLI fixture missing route assertion");
    let missing_route = std::process::Command::new("bash")
        .arg(&script_path)
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
        "closed_scalar_zero_owner_evidence_marker=test_legacy_body_access().is_empty()".to_owned(),
        "closed_scalar_cli_evidence=tests/real_world_cli.rs::canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts".to_owned(),
        "closed_scalar_cli_matrix_marker=contracts:[true, false];explicit_mir:[false, true]".to_owned(),
        "closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent".to_owned(),
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
    let abi_call = "            emit_closed_scalar_marker 'closed_scalar_cli_abi_marker=mir_ffi_i32,mir_ffi_i64,mir_ffi_bool,mir_ffi_f64,mir_ffi_store;legacy_route_asserted_absent'\n";
    let run_drift = |tampered_script: String, label: &str| {
        std::fs::write(&script_path, tampered_script).expect("write marker drift script");
        let output = std::process::Command::new("bash")
            .arg(&script_path)
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
fn legacy_owner_scalar_marker_recovery_preserves_condition_summary() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/audit-mir-legacy-owners.sh");
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
                line.contains("owner_deletion_condition_digest=")
                    || line.starts_with("owner_deletion_condition_set_digest=")
                    || line.starts_with("closed_scalar_")
                    || line.starts_with("scalar_evidence_marker_sequence_status=")
                    || line.starts_with("production_legacy_body_file_call_sites=")
                    || line.starts_with("production_raw_ast_call_sites=")
                    || line.starts_with("production_compile_func_legacy_call_sites=")
                    || line.starts_with("owner_count=")
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
    assert!(
        baseline_summary
            .iter()
            .any(|line| line.starts_with("owner_deletion_condition_set_digest=")),
        "baseline owner audit omitted condition set digest"
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
            "owner condition summary drifted after recovery batch {batch}"
        );
        assert_no_legacy_owner_temp_roots(&prefix);
    }
    assert_no_legacy_owner_temp_roots(&prefix);
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
