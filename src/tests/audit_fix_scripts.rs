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
