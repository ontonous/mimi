// ============================================================
// Real-world Mimi programs — CLI-driven MCDD regression suite
// ============================================================
//
// This integration test discovers every `.mimi` program under
// `tests/real_world/` (plus `projects/consumer/main.mimi`) and runs it
// through the actual `mimi run` and `mimi build` CLI paths. It is the
// Cargo-facing counterpart to `tests/real_world/run_suite.py`.
//
// Programs whose `main()` returns 0 are considered passing. Known gaps
// are listed in `KNOWN_GAPS`; failures there are reported but do not
// fail the test, so the suite can be used as a CI gate while still
// documenting real-world limitations.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return PathBuf::from(path);
    }

    // Cargo does not expose CARGO_BIN_EXE_mimi for every custom-target-dir
    // invocation.  The integration test itself still lives beside the
    // matching target directory, so prefer that binary over a stale
    // workspace target/debug/mimi.
    if let Ok(test_exe) = std::env::current_exe() {
        if let Some(target_debug) = test_exe.parent().and_then(Path::parent) {
            let candidate = target_debug.join("mimi");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    project_root().join("target").join("debug").join("mimi")
}

fn can_link() -> bool {
    static CAN_LINK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN_LINK.get_or_init(|| Command::new("cc").arg("--version").output().is_ok())
}

/// Files that are expected to fail because they exercise known
/// language or codegen gaps. Keep this list minimal and aligned with
/// `tests/real_world/RESULTS.md`.
/// Generic List construction with managed or nested elements is intentionally
/// fail-closed while the Canonical MIR construction island only proves the
/// single Copy-scalar shape (S105). Keep this fixture visible as a known gap so
/// the suite records the boundary instead of allowing a legacy fallback.
const KNOWN_GAPS: &[&str] = &["core_generics_return_abi.mimi"];

/// Programs whose feature contract is intentionally interpreter-only. Keep in
/// lockstep with `tests/real_world/run_suite.py`.
const INTERPRETER_ONLY: &[&str] = &["flow_test_macros.mimi"];

fn is_known_gap(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    KNOWN_GAPS.contains(&name)
}

fn normalize_run_output(s: &str) -> String {
    let mut lines: Vec<&str> = s.lines().collect();
    if lines.last().is_some_and(|l| l.starts_with("-> ")) {
        lines.pop();
    }
    lines.join("\n")
}

fn run_mimi_run_out(src: &Path) -> Result<String, String> {
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(src)
        .output()
        .map_err(|e| format!("failed to spawn mimi run: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(format!("mimi run failed\n{stderr}\n{stdout}"));
    }
    Ok(normalize_run_output(&stdout))
}

fn run_mimi_build_and_exec(src: &Path) -> Result<String, String> {
    let dir = std::env::temp_dir();
    let stem = src.file_stem().expect("src has stem").to_string_lossy();
    let binary = dir.join(format!("mimi_rw_{}_{}", std::process::id(), stem));

    let build_output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(src)
        .arg("-o")
        .arg(&binary)
        .output()
        .map_err(|e| format!("failed to spawn mimi build: {e}"))?;
    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        let _ = fs::remove_file(&binary);
        return Err(format!("mimi build failed\n{stderr}"));
    }

    let exec_output = Command::new(&binary)
        .output()
        .map_err(|e| format!("failed to run compiled binary: {e}"))?;
    let _ = fs::remove_file(&binary);
    if exec_output.status.success() {
        Ok(String::from_utf8_lossy(&exec_output.stdout)
            .trim_end()
            .to_string())
    } else {
        Err(format!(
            "compiled binary exited with {}",
            exec_output.status
        ))
    }
}

#[test]
fn std_mimispec_removed() {
    // 0.1.8 Phase E: the in-repo std/mimispec implementation and external
    // `mimispec` crate are removed. This test prevents regrowth of the old
    // sketch-parser surface in the standard library.
    let dir = project_root().join("std").join("mimispec");
    assert!(!dir.exists(), "std/mimispec must be removed in 0.1.8");
}

#[test]
fn canonical_mir_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_scalar.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mimi mir");
    assert!(
        output.status.success(),
        "mimi mir failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mir.type-catalog"));
    assert!(stdout.contains("mir.function function:main"));
    assert!(stdout.contains("binary"));
}

#[test]
fn canonical_mir_cli_all_uses_the_production_builder_for_imported_instances() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("std_set.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn imported canonical MIR inspection");
    assert!(
        output.status.success(),
        "mimi mir --all failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mir.function function:mir:instance:function:set_insert"),
        "canonical MIR inspection omitted the materialized Set facade instance:\n{stdout}"
    );
    assert!(
        stdout.contains(" list_op "),
        "canonical MIR inspection omitted the List.len operation:\n{stdout}"
    );
}

#[test]
fn canonical_mir_cli_all_rejects_unsupported_shapes_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn rejected canonical MIR inspection");
    assert!(
        !output.status.success(),
        "unsupported MIR inspection shape must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MIR inspection input rejected") && stderr.contains("Copy scalar"),
        "unexpected canonical MIR inspection rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("legacy") && !stderr.contains("bytecode runtime error"),
        "MIR inspection must not fall back to another consumer:\n{stderr}"
    );
}

#[test]
fn canonical_mir_run_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_scalar.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn mimi run --mir");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_native_build_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_scalar.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR reference bytecode run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-{}-{}",
        std::process::id(),
        fixture.file_stem().unwrap().to_string_lossy()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR native build");
    assert!(
        build.status.success(),
        "canonical MIR native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_scalar_list_len_closes_reference_native_and_verifier() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_len.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical MIR List.len dump");
    assert!(
        mir.status.success(),
        "canonical MIR List.len dump failed:\n{}\n{}",
        String::from_utf8_lossy(&mir.stderr),
        String::from_utf8_lossy(&mir.stdout)
    );
    assert!(String::from_utf8_lossy(&mir.stdout).contains("list_op"));

    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR List.len reference run");
    assert_eq!(
        reference.status.code(),
        Some(42),
        "canonical MIR List.len reference run failed:\n{}",
        String::from_utf8_lossy(&reference.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-len-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR List.len native build");
    assert!(
        build.status.success(),
        "canonical MIR List.len native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR List.len native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "canonical MIR List.len native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR List.len verifier");
    assert!(
        verification.status.success(),
        "canonical MIR List.len verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));

    // The same complete program is now a default production island. The
    // selector must choose one canonical graph for run/build/verify; the
    // presence of the canonical native adapter makes the route observable.
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default canonical List.len run");
    assert_eq!(
        default_run.status.code(),
        Some(42),
        "default canonical List.len run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default canonical List.len build");
    assert!(
        default_build_ir.status.success(),
        "default canonical List.len build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    assert!(
        String::from_utf8_lossy(&default_build_ir.stdout).contains("mimi_mir_list_len_scalar"),
        "default build did not select the canonical List.len island:\n{}",
        String::from_utf8_lossy(&default_build_ir.stdout)
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default canonical List.len verifier");
    assert!(
        default_verification.status.success(),
        "default canonical List.len verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stderr),
        String::from_utf8_lossy(&default_verification.stdout)
    );
    assert!(String::from_utf8_lossy(&default_verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_default_does_not_promote_non_copy_list_len() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_string_len_rejected.mimi");

    // The typed body contains List<string>. The MIR List.len contract is
    // scalar-Copy-only, so explicit MIR must reject it before any backend.
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical List<string>.len build");
    assert!(
        !explicit.status.success(),
        "unsupported List<string>.len unexpectedly entered canonical MIR:\n{}",
        String::from_utf8_lossy(&explicit.stderr)
    );

    // Default routing remains the explicit compatibility route for this
    // unsupported shape. It must not be mistaken for a canonical promotion.
    let default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn compatibility List<string>.len build");
    assert!(
        default.status.success(),
        "compatibility List<string>.len build failed:\n{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&default.stdout).contains("mimi_mir_list_len_scalar"),
        "unsupported List<string>.len was promoted to canonical MIR:\n{}",
        String::from_utf8_lossy(&default.stdout)
    );
}

#[test]
fn canonical_mir_test_uses_ast_free_bytecode_for_scalar_collection() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("test")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical scalar-collection mimi test");
    assert!(
        output.status.success(),
        "canonical scalar-collection mimi test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed, 0 failed"));
}

#[test]
fn canonical_mir_test_rejects_mixed_scalar_collection_without_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection_mixed.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("test")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected mixed scalar-collection mimi test");
    assert!(
        !output.status.success(),
        "mixed scalar collection unexpectedly used a compatibility test compiler:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("default Canonical MIR route rejected"));
    assert!(stderr.contains("S11 scalar collection candidate"));
}

#[test]
fn canonical_mir_disasm_uses_ast_free_bytecode_for_scalar_collection() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("disasm")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical scalar-collection mimi disasm");
    assert!(
        output.status.success(),
        "canonical scalar-collection disasm failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("function:test_list_len"),
        "canonical disasm must expose stable MIR function identity:\n{}",
        stdout
    );
    assert!(
        stdout.contains("MIR_LIST_LEN"),
        "canonical disasm must expose the MIR collection operation:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("__flow_Main_run_Single"),
        "canonical disasm unexpectedly retained compatibility-only Flow helpers:\n{}",
        stdout
    );
}

#[test]
fn canonical_mir_disasm_rejects_mixed_scalar_collection_without_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection_mixed.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("disasm")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected mixed scalar-collection mimi disasm");
    assert!(
        !output.status.success(),
        "mixed scalar collection unexpectedly used a compatibility disassembler:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("default Canonical MIR route rejected"));
    assert!(stderr.contains("S11 scalar collection candidate"));
}

#[test]
fn canonical_mir_native_owned_string_glue_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_owned_string.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR owned String reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(41),
        "canonical MIR owned String reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-owned-string-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR owned String native build");
    assert!(
        build.status.success(),
        "canonical MIR owned String native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR owned String native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(41),
        "canonical MIR owned String native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR owned String verifier");
    assert!(
        verification.status.success(),
        "canonical MIR owned String verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_verifier_proves_owned_string_result_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_result_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR owned String result verifier");
    assert!(
        output.status.success(),
        "verifier should prove the canonical owned String return"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_rejects_owned_string_return_branch_before_backend() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_return_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR owned String verifier");
    assert!(
        !output.status.success(),
        "branch-shaped return must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR verifier input rejected"),
        "{stderr}"
    );
    assert!(
        stderr.contains("owned String return contract requires one canonical MIR block"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("flow_ast"),
        "legacy verifier fallback leaked: {stderr}"
    );
}

#[test]
fn canonical_mir_verifier_proves_direct_owned_string_calls_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_call_return.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn direct owned String call verifier");
    assert!(
        output.status.success(),
        "direct owned String calls must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4/4 verified"), "{stdout}");
    assert!(
        stdout
            .matches("canonical MIR ensures contract proven")
            .count()
            >= 4
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_reports_nested_owned_string_call_boundary() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_call_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected nested owned String call verifier");
    assert!(
        output.status.success(),
        "trusted-subset rejection is a verifier result, not a process failure:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.contains(
            "direct owned String call target 'function:nested' rejected: owned String return contract only admits String constants and ownership glue"
        ),
        "{output}"
    );
    assert!(
        !output.contains("flow_ast"),
        "legacy verifier fallback leaked: {output}"
    );
}

#[test]
fn canonical_mir_verifier_proves_non_copy_record_move_projection_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_move_projection.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn record MoveProject verifier");
    assert!(
        output.status.success(),
        "record MoveProject must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_proves_non_copy_result_string_i32_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_i32.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn Result<string, i32> verifier");
    assert!(
        output.status.success(),
        "Result<string, i32> must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_proves_non_copy_result_string_i32_switch_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_i32_switch_move.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn Result SwitchMove verifier");
    assert!(
        output.status.success(),
        "Result SwitchMove must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_classifies_result_string_string_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_string_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected Result verifier");
    assert!(
        output.status.success(),
        "trusted-subset rejection must remain a verifier classification:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical non-Copy Result<string, i32> variant contract"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_native_recursive_tuple_glue_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_recursive_tuple.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR recursive tuple reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR recursive tuple reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-recursive-tuple-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR recursive tuple native build");
    assert!(
        build.status.success(),
        "canonical MIR recursive tuple native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR recursive tuple native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR recursive tuple native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_native_non_copy_record_glue_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_non_copy_record.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR non-Copy record reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR non-Copy record reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-non-copy-record-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR non-Copy record native build");
    assert!(
        build.status.success(),
        "canonical MIR non-Copy record native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR non-Copy record native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR non-Copy record native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_native_record_move_project_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_move_project.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record MoveProject reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR record MoveProject reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-move-project-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record MoveProject native build");
    assert!(
        build.status.success(),
        "canonical MIR record MoveProject native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record MoveProject native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR record MoveProject native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_record_move_project_rejects_non_copy_sibling_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_move_project_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-move-project-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR record MoveProject build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR build error"));
    assert!(stderr.contains("non-Copy") && stderr.contains("explicit move projection contract"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_abs_overflow_matches_mir_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_abs_overflow.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR trap oracle");
    assert_eq!(mir_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mir_run.stderr).contains("E0802"));

    let binary =
        std::env::temp_dir().join(format!("mimi-canonical-native-trap-{}", std::process::id()));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR trap build");
    assert!(
        build.status.success(),
        "canonical MIR trap build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR trap binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0802"));
}

#[test]
fn canonical_mir_native_builds_min_max_and_widening_convert() {
    for fixture_name in [
        "mir_builtin_min_max.mimi",
        "mir_convert_i32_to_i64_min_max.mimi",
    ] {
        let fixture = project_root()
            .join("tests")
            .join("fixtures")
            .join(fixture_name);
        let binary = std::env::temp_dir().join(format!(
            "mimi-canonical-native-numeric-{}-{}",
            std::process::id(),
            fixture_name
        ));
        let build = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("build")
            .arg(&fixture)
            .arg("--mir")
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("failed to spawn canonical MIR numeric build");
        assert!(
            build.status.success(),
            "canonical MIR numeric build failed for {fixture_name}:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let native_run = Command::new(&binary)
            .output()
            .expect("failed to execute canonical MIR numeric binary");
        let _ = fs::remove_file(&binary);
        assert_eq!(
            native_run.status.code(),
            Some(42),
            "canonical MIR numeric native run failed for {fixture_name}:\n{}",
            String::from_utf8_lossy(&native_run.stderr)
        );
    }
}

#[test]
fn canonical_mir_native_build_record_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_copy.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record native build");
    assert!(
        build.status.success(),
        "canonical MIR record native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_record_update_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_update.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record update reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-update-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record update native build");
    assert!(
        build.status.success(),
        "canonical MIR record update native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record update native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn default_copy_record_update_selects_canonical_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_update.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default record update run");
    assert_eq!(
        output.status.code(),
        Some(42),
        "default canonical record update run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default canonical record update build");
    assert!(
        default_build_ir.status.success(),
        "default canonical record update build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    let ir = String::from_utf8_lossy(&default_build_ir.stdout);
    assert!(
        ir.contains("define i32 @main()"),
        "default record update did not select the canonical MIR route:\n{ir}"
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-canonical-record-update-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default canonical record update native build");
    assert!(
        build.status.success(),
        "default canonical record update native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default canonical record update binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "default canonical record update native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default canonical record update verifier");
    assert!(
        verification.status.success(),
        "default canonical record update verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
}

#[test]
fn default_silent_local_flow_transition_selects_one_canonical_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_flow_transition.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Flow transition run");
    assert_eq!(
        default_run.status.code(),
        Some(42),
        "default Flow transition run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default Flow transition LLVM emission");
    assert!(
        default_build_ir.status.success(),
        "default Flow transition build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    let ir = String::from_utf8_lossy(&default_build_ir.stdout);
    assert!(
        ir.contains("@__mimi_transition_Counter__inc__Zero"),
        "default build did not select the canonical Flow transition island:\n{ir}"
    );

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical Flow transition inspection");
    assert!(mir.status.success());
    assert!(String::from_utf8_lossy(&mir.stdout)
        .contains("mir.transition transition:Counter::inc::Zero"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-canonical-flow-transition-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Flow transition native build");
    assert!(
        build.status.success(),
        "default Flow transition native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default Flow transition native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "default Flow transition native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Flow transition verifier");
    assert!(
        verification.status.success(),
        "default Flow transition verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
}

#[test]
fn default_flow_candidate_never_falls_back_to_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_flow_transition_rejected_builtin.mimi");
    let expected = "default Canonical MIR route rejected";

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected Flow candidate run");
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains(expected));

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn rejected Flow candidate build");
    assert!(!build.status.success());
    assert!(String::from_utf8_lossy(&build.stderr).contains(expected));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected Flow candidate verifier");
    assert!(!verify.status.success());
    assert!(String::from_utf8_lossy(&verify.stderr).contains(expected));
}

#[test]
fn canonical_default_does_not_promote_non_copy_record_program() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_non_copy_record.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default non-Copy record build");
    assert!(
        output.status.success(),
        "default non-Copy record compatibility build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ir = String::from_utf8_lossy(&output.stdout);
    assert!(
        ir.contains("define i32 @main(i32 %0, ptr %1)"),
        "non-Copy record was promoted to the flat Copy canonical island:\n{ir}"
    );
}

#[test]
fn canonical_mir_native_borrow_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_borrow_scalar.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR borrow reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR borrow reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-borrow-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR borrow native build");
    assert!(
        build.status.success(),
        "canonical MIR borrow native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR borrow native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR borrow native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_native_record_update_preserves_checked_trap() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_update_overflow.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record update trap oracle");
    assert_eq!(mir_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mir_run.stderr).contains("E0802"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-update-trap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record update trap build");
    assert!(
        build.status.success(),
        "canonical MIR record update trap build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record update trap binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0802"));
}

#[test]
fn canonical_mir_native_builds_copy_option_and_result_variants() {
    for (fixture_name, expected_status) in [
        ("mir_native_option_bool.mimi", 42),
        ("mir_native_option_copy.mimi", 42),
        ("mir_native_result_copy.mimi", 8),
    ] {
        let fixture = project_root()
            .join("tests")
            .join("fixtures")
            .join(fixture_name);
        let mir_run = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("run")
            .arg(&fixture)
            .arg("--mir")
            .output()
            .expect("failed to spawn canonical MIR variant reference run");
        assert_eq!(mir_run.status.code(), Some(expected_status));

        let binary = std::env::temp_dir().join(format!(
            "mimi-canonical-native-variant-{}-{}",
            std::process::id(),
            fixture_name
        ));
        let build = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("build")
            .arg(&fixture)
            .arg("--mir")
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("failed to spawn canonical MIR variant native build");
        assert!(
            build.status.success(),
            "canonical MIR variant native build failed for {fixture_name}:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let native_run = Command::new(&binary)
            .output()
            .expect("failed to execute canonical MIR variant native binary");
        let _ = fs::remove_file(&binary);
        assert_eq!(native_run.status.code(), Some(expected_status));
    }
}

#[test]
fn canonical_default_copy_option_i32_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i32_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i32> reference run");
    assert_eq!(default_run.status.code(), Some(41));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<i32> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i32> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<i32> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-i32-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<i32> native build");
    assert!(
        build.status.success(),
        "default Copy Option<i32> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<i32> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_option_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option projection run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option projection verifier");
    assert!(
        verify.status.success(),
        "default generic Option projection verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-unwrap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Option projection native build");
    assert!(
        build.status.success(),
        "default generic Option projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Option projection native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_option_unwrap_or_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default generic Option unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Option unwrap_or native build");
    assert!(
        build.status.success(),
        "default generic Option unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Option unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));

    let none_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_none.mimi");
    let none_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&none_fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or None run");
    assert_eq!(none_run.status.code(), Some(7));

    let bool_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_bool.mimi");
    let bool_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&bool_fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or bool run");
    assert_eq!(bool_run.status.code(), Some(0));

    let i64_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_i64.mimi");
    let i64_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&i64_fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or i64 run");
    assert_eq!(i64_run.status.code(), Some(7));

    let i64_binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-unwrap-or-i64-{}",
        std::process::id()
    ));
    let i64_build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&i64_fixture)
        .arg("-o")
        .arg(&i64_binary)
        .output()
        .expect("failed to spawn default generic Option unwrap_or i64 native build");
    assert!(
        i64_build.status.success(),
        "default generic Option unwrap_or i64 native build failed:\n{}",
        String::from_utf8_lossy(&i64_build.stderr)
    );
    let i64_native_run = Command::new(&i64_binary)
        .output()
        .expect("failed to execute default generic Option unwrap_or i64 native binary");
    let _ = fs::remove_file(&i64_binary);
    assert_eq!(i64_native_run.status.code(), Some(7));
}

#[test]
fn canonical_default_generic_option_projection_rejects_unmigrated_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn unsupported generic Option projection run");
    assert!(!run.status.success());
    let run_stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run_stderr.contains("generic Option projection") && !run_stderr.contains("legacy"),
        "unsupported generic Option fallback must fail closed:\n{run_stderr}"
    );

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn unsupported generic Option projection verifier");
    assert!(!verify.status.success());
    let verify_stderr = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify_stderr.contains("generic Option projection"),
        "verifier must reject before AST compatibility fallback:\n{verify_stderr}"
    );

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .output()
        .expect("failed to spawn unsupported generic Option projection build");
    assert!(!build.status.success());
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build_stderr.contains("generic Option projection"),
        "native build must reject the unmigrated fallback shape:\n{build_stderr}"
    );
}

#[test]
fn canonical_default_generic_result_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap.mimi");
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result projection run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result projection verifier");
    assert!(
        verify.status.success(),
        "default generic Result projection verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-result-unwrap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Result projection native build");
    assert!(
        build.status.success(),
        "default generic Result projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Result projection native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_result_unwrap_or_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or.mimi");
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or run");
    assert_eq!(default_run.status.code(), Some(48));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default generic Result unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-result-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Result unwrap_or native build");
    assert!(
        build.status.success(),
        "default generic Result unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Result unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));

    let i64_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or_i64.mimi");
    let i64_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&i64_fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or i64 run");
    assert_eq!(i64_run.status.code(), Some(7));

    let bool_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or_bool.mimi");
    let bool_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&bool_fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or bool run");
    assert_eq!(bool_run.status.code(), Some(0));
}

#[test]
fn canonical_default_generic_distinct_result_unwrap_or_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_distinct_unwrap_or.mimi");
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default heterogeneous Result unwrap_or run");
    assert_eq!(default_run.status.code(), Some(50));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default heterogeneous Result unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default heterogeneous Result unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-distinct-result-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default heterogeneous Result unwrap_or native build");
    assert!(
        build.status.success(),
        "default heterogeneous Result unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default heterogeneous Result unwrap_or native binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(50));
}

#[test]
fn canonical_default_generic_result_unwrap_or_rejects_unmigrated_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture)
            .output()
            .expect("failed to spawn unsupported generic Result unwrap_or command");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("generic Result") && !stderr.contains("legacy"),
            "unsupported generic Result unwrap_or must fail closed for {command}:\n{stderr}"
        );
    }
}

#[test]
fn canonical_default_generic_result_projection_trap_and_rejection_are_fail_closed() {
    let trap_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_none.mimi");
    let trap_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&trap_fixture)
        .output()
        .expect("failed to spawn generic Result Err projection run");
    assert!(!trap_run.status.success());
    assert!(String::from_utf8_lossy(&trap_run.stderr).contains("E0800"));

    let rejected_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&rejected_fixture)
            .output()
            .expect("failed to spawn unsupported generic Result projection command");
        assert!(!output.status.success());
        assert!(
            (String::from_utf8_lossy(&output.stderr).contains("generic Result projection")
                || String::from_utf8_lossy(&output.stderr)
                    .contains("generic-result-projection-v1")),
            "unsupported generic Result projection must fail closed for {command}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn canonical_default_generic_result_distinct_projection_matches_reference_and_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_distinct_unwrap.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic distinct Result projection run");
    assert_eq!(
        run.status.code(),
        Some(41),
        "reference/bytecode run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let binary = std::env::temp_dir().join(format!(
        "mimi-generic-result-distinct-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn generic distinct Result projection native build");
    assert!(
        build.status.success(),
        "native MIR build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute generic distinct Result projection native binary");
    assert_eq!(native.status.code(), Some(41));
}

#[test]
fn canonical_default_copy_option_bool_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_bool_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<bool> reference run");
    assert_eq!(default_run.status.code(), Some(42));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<bool> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(42));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<bool> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<bool> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-bool-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<bool> native build");
    assert!(
        build.status.success(),
        "default Copy Option<bool> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<bool> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_default_copy_option_i64_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i64_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i64> reference run");
    assert_eq!(default_run.status.code(), Some(41));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<i64> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i64> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<i64> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-i64-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<i64> native build");
    assert!(
        build.status.success(),
        "default Copy Option<i64> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<i64> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_copy_option_f64_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_f64_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<f64> reference run");
    assert_eq!(default_run.status.code(), Some(42));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<f64> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(42));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<f64> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<f64> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("No contracts to verify"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-f64-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<f64> native build");
    assert!(
        build.status.success(),
        "default Copy Option<f64> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<f64> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_f64_unary_negate_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_negate.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR f64 negate run");
    assert_eq!(
        run.status.code(),
        Some(42),
        "explicit MIR run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-explicit-mir-f64-negate-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR f64 negate build");
    assert!(
        build.status.success(),
        "explicit MIR f64 negate native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR f64 negate native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_f64_add_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_add.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR f64 add run");
    assert_eq!(
        run.status.code(),
        Some(42),
        "explicit MIR f64 add run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary =
        std::env::temp_dir().join(format!("mimi-explicit-mir-f64-add-{}", std::process::id()));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR f64 add build");
    assert!(
        build.status.success(),
        "explicit MIR f64 add native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR f64 add native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_f64_subtract_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_subtract.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR f64 subtract run");
    assert_eq!(
        run.status.code(),
        Some(42),
        "explicit MIR f64 subtract run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-explicit-mir-f64-subtract-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR f64 subtract build");
    assert!(
        build.status.success(),
        "explicit MIR f64 subtract native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR f64 subtract native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_result_i32_i32_unwrap_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Result<i32, i32>.unwrap run");
    assert_eq!(
        run.status.code(),
        Some(41),
        "explicit MIR Result unwrap run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-explicit-mir-result-i32-i32-unwrap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR Result unwrap build");
    assert!(
        build.status.success(),
        "explicit MIR Result unwrap native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR Result unwrap native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_explicit_mir_rejects_result_i64_i32_unwrap_before_backend() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i64_i32_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn unsupported explicit MIR Result unwrap run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains(
            "Option/Result unwrap shape is outside the canonical variant projection contract"
        ),
        "unsupported Result unwrap must fail closed before a backend:\n{stderr}"
    );
}

#[test]
fn canonical_default_result_i32_i32_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result<i32, i32> reference run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result<i32, i32> verifier");
    assert!(
        verify.status.success(),
        "default Copy Result<i32, i32> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i32-i32-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Result<i32, i32> native build");
    assert!(
        build.status.success(),
        "default Copy Result<i32, i32> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Result<i32, i32> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_rejects_result_i64_i32_unwrap_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i64_i32_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default unsupported Copy Result run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("Copy Result<i32, i32> projection candidate is outside complete coverage"),
        "unsupported Result projection must fail closed before legacy:\n{stderr}"
    );

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default unsupported Copy Result verifier");
    assert!(!verify.status.success());
    let verify_stderr = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify_stderr
            .contains("Copy Result<i32, i32> projection candidate is outside complete coverage"),
        "unsupported Result projection verifier must fail closed before legacy:\n{verify_stderr}"
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i64-i32-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default unsupported Copy Result native build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build_stderr.contains("Copy Result<i32, i32> variant MIR island")
            || build_stderr.contains("Copy Result<i32, i32> projection candidate"),
        "unsupported Result projection native build must fail closed before LLVM:\n{build_stderr}"
    );
}

#[test]
fn canonical_default_result_i32_i32_err_unwrap_preserves_active_tag_trap() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap_err.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result Err unwrap run");
    assert_eq!(default_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&default_run.stderr).contains("E0800"));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result Err unwrap verifier");
    assert!(!verify.status.success());
    let verify_stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(verify_stdout.contains("canonical MIR"), "{verify_stdout}");
    assert!(verify_stdout.contains("E0800"), "{verify_stdout}");

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i32-i32-err-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Result Err unwrap native build");
    assert!(
        build.status.success(),
        "default Copy Result Err unwrap native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Result Err unwrap native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0800"));
}

#[test]
fn canonical_default_result_i32_i32_unwrap_or_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap_or.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result unwrap_or reference run");
    assert_eq!(default_run.status.code(), Some(14));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default Copy Result unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i32-i32-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Result unwrap_or native build");
    assert!(
        build.status.success(),
        "default Copy Result unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Result unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(14));
}

#[test]
fn canonical_default_option_i32_unwrap_or_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i32_unwrap_or.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option unwrap_or reference run");
    assert_eq!(default_run.status.code(), Some(14));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default Copy Option unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-i32-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option unwrap_or native build");
    assert!(
        build.status.success(),
        "default Copy Option unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(14));
}

#[test]
fn canonical_default_rejects_option_i64_unwrap_or_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i64_unwrap_or_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let binary = std::env::temp_dir().join(format!(
            "mimi-default-copy-option-i64-unwrap-or-rejected-{}",
            std::process::id()
        ));
        let mut invocation = Command::new(mimi_bin());
        invocation
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture);
        if command == "build" {
            invocation.arg("-o").arg(&binary);
        }
        let output = invocation
            .output()
            .expect("failed to spawn unsupported Copy Option unwrap_or command");
        let _ = fs::remove_file(&binary);
        assert!(!output.status.success(), "{command} must fail closed");
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains("Copy Option<i64> variant candidate is not eligible")
                || diagnostics.contains("Copy Option<i64>"),
            "{command} must reject before legacy/backend:\n{diagnostics}"
        );
    }
}

#[test]
fn canonical_default_rejects_result_i64_i32_unwrap_or_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i64_i32_unwrap_or_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let binary = std::env::temp_dir().join(format!(
            "mimi-default-copy-result-i64-i32-unwrap-or-rejected-{}",
            std::process::id()
        ));
        let mut invocation = Command::new(mimi_bin());
        invocation
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture);
        if command == "build" {
            invocation.arg("-o").arg(&binary);
        }
        let output = invocation
            .output()
            .expect("failed to spawn unsupported Copy Result unwrap_or command");
        let _ = fs::remove_file(&binary);
        assert!(!output.status.success(), "{command} must fail closed");
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains(
                "Copy Result<i32, i32> projection candidate is outside complete coverage"
            ),
            "{command} must reject before legacy/backend:\n{diagnostics}"
        );
    }
}

#[test]
fn canonical_default_rejects_mixed_copy_option_bool_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_bool_mixed_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mixed Copy Option<bool> default run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("Copy Option<bool>"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_default_rejects_mixed_copy_option_i64_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i64_mixed_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mixed Copy Option<i64> default run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("S116 Copy Option<i64>"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_default_rejects_mixed_copy_option_f64_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_f64_mixed_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mixed Copy Option<f64> default run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("S117 Copy Option<f64>"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_mir_native_builds_non_copy_option_string_glue() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Option<string> reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-option-string-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Option<string> native build");
    assert!(
        build.status.success(),
        "canonical MIR Option<string> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Option<string> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_option_string_switch_move_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string_switch_move.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Option<string> SwitchMove reference run");
    assert_eq!(mir_run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-option-string-switch-move-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Option<string> SwitchMove native build");
    assert!(
        build.status.success(),
        "canonical MIR Option<string> SwitchMove native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Option<string> SwitchMove binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));

    // The same exact island must be selected by the production defaults after
    // all-consumer preflight; this is the switch-over proof, not merely an
    // explicit `--mir` smoke test.
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Canonical MIR Option<string> run");
    assert_eq!(default_run.status.code(), Some(48));

    let default_binary = std::env::temp_dir().join(format!(
        "mimi-default-option-string-switch-move-{}",
        std::process::id()
    ));
    let default_build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&default_binary)
        .output()
        .expect("failed to spawn default Canonical MIR Option<string> build");
    assert!(
        default_build.status.success(),
        "default Canonical MIR Option<string> build failed:\n{}",
        String::from_utf8_lossy(&default_build.stderr)
    );
    let default_native_run = Command::new(&default_binary)
        .output()
        .expect("failed to execute default Canonical MIR Option<string> binary");
    let _ = fs::remove_file(&default_binary);
    assert_eq!(default_native_run.status.code(), Some(48));

    let default_verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Canonical MIR Option<string> verifier");
    assert!(
        default_verify.status.success(),
        "default Canonical MIR Option<string> verify failed:\n{}",
        String::from_utf8_lossy(&default_verify.stderr)
    );
    let verify_output = format!(
        "{}{}",
        String::from_utf8_lossy(&default_verify.stdout),
        String::from_utf8_lossy(&default_verify.stderr)
    );
    assert!(verify_output.contains("consume"), "{verify_output}");
    assert!(verify_output.contains("contract proven"), "{verify_output}");
}

#[test]
fn canonical_mir_native_result_string_i32_switch_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_i32_switch_move.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-switch-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> native build");
    assert!(
        build.status.success(),
        "canonical MIR Result<string, i32> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result<string, i32> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_result_string_i32_clone_drop_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_string_i32_glue.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> glue reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-glue-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> glue native build");
    assert!(
        build.status.success(),
        "canonical MIR Result<string, i32> glue native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result<string, i32> glue native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_result_string_i32_call_return_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_string_i32_call_return.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result call/return reference run");
    assert_eq!(mir_run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-call-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result call/return native build");
    assert!(
        build.status.success(),
        "canonical MIR Result call/return native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result call/return native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));
}

#[test]
fn canonical_mir_native_result_string_i32_call_return_multipath_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_string_i32_call_return_multipath.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result multi-path reference run");
    assert_eq!(mir_run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-call-return-multipath-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result multi-path native build");
    assert!(
        build.status.success(),
        "canonical MIR Result multi-path native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result multi-path native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));
}

#[test]
fn canonical_mir_native_option_overflow_matches_mir_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_overflow.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR variant trap oracle");
    assert_eq!(mir_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mir_run.stderr).contains("E0802"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-variant-trap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR variant trap build");
    assert!(
        build.status.success(),
        "canonical MIR variant trap build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR variant trap binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0802"));
}

#[test]
fn canonical_mir_native_rejects_mixed_variant_payload_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_variant_mixed_payload_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-variant-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR variant build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR native backend rejected"));
    assert!(stderr.contains("flat Copy variant contract"));
    assert!(stderr.contains("mixed payload ABI"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_rejects_non_copy_variant_outside_promoted_contract_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-option-string-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR Option<string> build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR native backend rejected"));
    assert!(stderr.contains("native non-Copy Option<string> variant contract"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn default_route_rejects_non_exhaustive_option_string_switch_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string_default_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-default-option-string-switch-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected default Option<string> build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(
        stderr.contains("S30 non-Copy Option<string> variant candidate"),
        "{stderr}"
    );
    assert!(!stderr.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_mir_native_rejects_record_with_unsupported_child_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_noncopy_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR record build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR native backend rejected"));
    assert!(stderr.contains("outside the scalar/String/tuple ABI"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_rejects_recursive_tuple_with_list_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_recursive_tuple_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-recursive-tuple-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR recursive tuple build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR native backend rejected"));
    assert!(stderr.contains("outside the scalar/String/tuple ABI"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_build_rejects_unsupported_shape_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR native build");
    let _ = fs::remove_file(&binary);
    assert!(
        !build.status.success(),
        "unsupported native MIR must fail closed"
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("canonical MIR native backend rejected")
            && stderr.contains("binary operator")
            && stderr.contains("finite-only Copy f64 contract"),
        "unexpected canonical native rejection:\n{stderr}"
    );
    assert!(
        stderr.contains("canonical MIR native backend capability check failed"),
        "rejection must identify the canonical MIR gate:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "native MIR rejection must not fall back to another backend:\n{stderr}"
    );
}

#[test]
fn canonical_mir_builtin_abs_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_abs.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR abs fixture");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR abs fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_builtin_abs_rejects_unsupported_width_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_abs_i32_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR abs fixture");
    assert!(
        !output.status.success(),
        "unsupported abs width must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error")
            && stderr.contains("builtin 'abs'")
            && stderr.contains("canonical contract accepts signed i64 or f64"),
        "unexpected canonical abs rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical abs rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_builtin_min_max_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_min_max.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR min/max fixture");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR min/max fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_builtin_min_rejects_unsupported_abi_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_min_f64_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR min fixture");
    assert!(
        !output.status.success(),
        "unsupported min ABI must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error")
            && stderr.contains("builtin 'min'")
            && stderr.contains("canonical contract accepts signed i64"),
        "unexpected canonical min rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical min rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_convert_i32_to_i64_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_convert_i32_to_i64_min_max.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR conversion fixture");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR conversion fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_convert_i32_to_f64_rejects_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_convert_i32_to_f64_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical conversion fixture");
    assert!(
        !output.status.success(),
        "i32 to f64 conversion must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error")
            && stderr.contains("conversion")
            && stderr.contains("accepted: same Copy scalar type"),
        "unexpected canonical conversion rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical conversion rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_run_cli_executes_imported_user_call_graph() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("projects")
        .join("consumer")
        .join("main.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn imported canonical MIR program");
    assert_eq!(
        output.status.code(),
        Some(0),
        "imported canonical MIR run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_run_cli_executes_list_index_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("core_list_index.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List index program");
    assert!(
        output.status.success(),
        "canonical List index program failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_native_build_list_index_matches_reference_and_bytecode() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_index.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-index-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List native build");
    assert!(
        build.status.success(),
        "canonical List native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_set_to_list_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_to_list.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical Set.to_list reference run");
    assert_eq!(
        reference.status.code(),
        Some(42),
        "canonical Set.to_list reference run failed:\n{}",
        String::from_utf8_lossy(&reference.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-set-to-list-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical Set.to_list native build");
    assert!(
        build.status.success(),
        "canonical Set.to_list native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical Set.to_list native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "canonical Set.to_list native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );
}

#[test]
fn canonical_mir_native_set_function_contains_matches_reference_and_default() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_contains_function.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical Set function-form MIR dump");
    assert!(
        mir.status.success(),
        "canonical Set function-form MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    assert!(
        String::from_utf8_lossy(&mir.stdout).contains("set_op")
            && String::from_utf8_lossy(&mir.stdout).contains("Contains"),
        "bare contains(Set, T) did not materialize as the canonical SetOp:\n{}",
        String::from_utf8_lossy(&mir.stdout)
    );

    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical Set function-form reference run");
    assert_eq!(
        reference.status.code(),
        Some(42),
        "canonical Set function-form reference run failed:\n{}",
        String::from_utf8_lossy(&reference.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-set-contains-function-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical Set function-form native build");
    assert!(
        build.status.success(),
        "canonical Set function-form native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical Set function-form native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set function-form run");
    assert_eq!(
        default_run.status.code(),
        Some(42),
        "default Set function-form run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default Set function-form native build");
    assert!(default_ir.status.success());
    assert!(
        String::from_utf8_lossy(&default_ir.stdout).contains("define i32 @main()"),
        "default build did not select canonical Set contains:\n{}",
        String::from_utf8_lossy(&default_ir.stdout)
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set function-form verifier");
    assert!(
        default_verification.status.success(),
        "default Set function-form verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stdout),
        String::from_utf8_lossy(&default_verification.stderr)
    );
    assert!(
        String::from_utf8_lossy(&default_verification.stdout)
            .contains("canonical MIR ensures contract proven"),
        "default verifier did not consume the canonical Set program:\n{}",
        String::from_utf8_lossy(&default_verification.stdout)
    );
}

#[test]
fn canonical_mir_set_contains_println_bool_matches_all_production_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_contains_println.mimi");
    let expected = "true\nfalse\ntrue\n";

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical Set/println MIR dump");
    assert!(mir.status.success());
    let mir_stdout = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_stdout.contains("SetOp::Contains") || mir_stdout.contains("set_op"));
    assert!(
        mir_stdout.contains("PrintlnBool"),
        "MIR omitted println(bool):\n{mir_stdout}"
    );

    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical Set/println reference run");
    assert_eq!(reference.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&reference.stdout), expected);

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set/println run");
    assert_eq!(default_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), expected);

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-set-contains-println-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical Set/println native build");
    assert!(
        build.status.success(),
        "canonical Set/println native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical Set/println native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default Set/println native IR build");
    assert!(default_ir.status.success());
    let ir = String::from_utf8_lossy(&default_ir.stdout);
    assert!(ir.contains("define i32 @main("));
    assert!(ir.contains("@printf"));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set/println verifier");
    assert!(
        verification.status.success(),
        "default Set/println verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_rejects_unsupported_println_before_any_backend() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_println_non_bool_rejected.mimi");
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical println build");
    assert!(!explicit.status.success());
    let stderr = String::from_utf8_lossy(&explicit.stderr);
    assert!(
        stderr.contains("canonical contract accepts signed i32 or i64"),
        "unsupported println lost its stable canonical diagnostic:\n{stderr}"
    );

    let default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn compatibility non-bool println run");
    assert!(default.status.success());
    assert_eq!(String::from_utf8_lossy(&default.stdout), "true\nlegacy\n");
}

#[test]
fn canonical_mir_standalone_bool_println_uses_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_println_bool_standalone.mimi");
    let expected = "true\nfalse\n";

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone println MIR dump");
    assert!(mir.status.success());
    let mir_stdout = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_stdout.contains("PrintlnBool"));
    assert!(!mir_stdout.contains("SetOp::") && !mir_stdout.contains("ListOp::"));

    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn standalone canonical run");
    assert_eq!(explicit.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&explicit.stdout), expected);

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone default run");
    assert_eq!(default_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), expected);

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn standalone default native build");
    assert!(
        default_ir.status.success(),
        "standalone default native build failed:\n{}",
        String::from_utf8_lossy(&default_ir.stderr)
    );
    let ir = String::from_utf8_lossy(&default_ir.stdout);
    assert!(ir.contains("define i32 @main("));
    assert!(ir.contains("@printf"));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone verifier");
    assert!(verification.status.success());
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_standalone_integer_println_matches_all_production_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_println_int.mimi");
    let expected = "-7\n9223372036854775806\n";

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone integer println MIR dump");
    assert!(mir.status.success());
    let mir_stdout = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_stdout.contains("PrintlnInt"));
    assert!(!mir_stdout.contains("SetOp::") && !mir_stdout.contains("ListOp::"));

    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn standalone integer canonical run");
    assert_eq!(explicit.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&explicit.stdout), expected);

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone integer default run");
    assert_eq!(default_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), expected);

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-println-int-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn standalone integer default native build");
    assert!(
        build.status.success(),
        "standalone integer default native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute standalone integer native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn standalone integer native IR build");
    assert!(default_ir.status.success());
    let ir = String::from_utf8_lossy(&default_ir.stdout);
    assert!(ir.contains("define i32 @main("));
    assert!(ir.contains("@printf") && ir.contains("c\"%ld\\00"));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone integer verifier");
    assert!(
        verification.status.success(),
        "standalone integer verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_default_does_not_promote_list_function_contains() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_contains_rejected.mimi");

    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected List function-form contains build");
    assert!(
        !explicit.status.success(),
        "List contains unexpectedly entered canonical MIR:\n{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(
        String::from_utf8_lossy(&explicit.stderr).contains("not a materialized MIR function")
            || String::from_utf8_lossy(&explicit.stderr).contains("canonical MIR"),
        "List contains rejection lost its stable canonical boundary:\n{}",
        String::from_utf8_lossy(&explicit.stderr)
    );

    let default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn compatibility List function-form contains build");
    assert!(
        default.status.success(),
        "compatibility List contains build failed:\n{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&default.stdout).contains("mimi_mir_set"),
        "List contains was promoted to the canonical Set backend:\n{}",
        String::from_utf8_lossy(&default.stdout)
    );
}

#[test]
fn canonical_mir_std_set_generic_facade_is_atomic_across_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("std_set.mimi");

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical std::set reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(0),
        "canonical std::set reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-std-set-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical std::set native build");
    assert!(
        build.status.success(),
        "canonical std::set native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical std::set native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(0),
        "canonical std::set native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical std::set verifier");
    assert!(
        verification.status.success(),
        "canonical std::set verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );

    // The typed scalar Set facade is now a complete default-switch island.
    // All three default entry points must select the same canonical program;
    // there is no per-consumer fallback after selection.
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default std::set run");
    assert_eq!(
        default_run.status.code(),
        Some(0),
        "default std::set run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default std::set native emit-ir");
    assert!(
        default_build_ir.status.success(),
        "default std::set native build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    let default_ir = String::from_utf8_lossy(&default_build_ir.stdout);
    assert!(
        default_ir.contains("mimi_mir_set_to_list_scalar"),
        "default build did not select the canonical Set island:\n{default_ir}"
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default std::set verifier");
    assert!(
        default_verification.status.success(),
        "default std::set verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stderr),
        String::from_utf8_lossy(&default_verification.stdout)
    );
}

#[test]
fn canonical_mir_generic_list_concat_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_concat.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List.concat MIR dump");
    assert!(
        mir.status.success(),
        "canonical generic List.concat MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    let mir_text = String::from_utf8_lossy(&mir.stdout);
    assert!(
        mir_text.contains("Concat"),
        "MIR dump omitted ListOp::Concat:\n{mir_text}"
    );
    assert!(
        mir_text.contains("list_contract=MirListOperationContract"),
        "MIR dump omitted the canonical TypeDesc receipt:\n{mir_text}"
    );

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List.concat reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(5),
        "canonical generic List.concat reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-concat-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List.concat native build");
    assert!(
        build.status.success(),
        "canonical generic List.concat native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List.concat native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(5),
        "canonical generic List.concat native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List.concat verifier");
    assert!(
        verification.status.success(),
        "canonical generic List.concat verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List.concat run");
    assert_eq!(
        default_run.status.code(),
        Some(5),
        "default generic List.concat run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List.concat native emit-ir");
    assert!(
        default_ir.status.success(),
        "default generic List.concat build failed:\n{}",
        String::from_utf8_lossy(&default_ir.stderr)
    );
    assert!(
        String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_concat_scalar"),
        "default route did not select canonical List.concat native helper:\n{}",
        String::from_utf8_lossy(&default_ir.stdout)
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List.concat verifier");
    assert!(
        default_verification.status.success(),
        "default generic List.concat verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stderr),
        String::from_utf8_lossy(&default_verification.stdout)
    );
}

#[test]
fn canonical_mir_generic_list_construct_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_construct.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List construction MIR dump");
    assert!(
        mir.status.success(),
        "canonical generic List construction MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    let mir_text = String::from_utf8_lossy(&mir.stdout);
    assert!(
        mir_text.contains("construct_list"),
        "MIR dump omitted ConstructList"
    );
    assert!(
        mir_text.contains("list_construct_contract=MirListConstructContract"),
        "MIR dump omitted the canonical construction TypeDesc receipt:\n{mir_text}"
    );

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List construction reference run");
    assert_eq!(mir_run.status.code(), Some(1));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-construct-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List construction native build");
    assert!(
        build.status.success(),
        "canonical generic List construction native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List construction native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(1));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List construction verifier");
    assert!(verification.status.success());
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List construction run");
    assert_eq!(default_run.status.code(), Some(1));
    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List construction native emit-ir");
    assert!(default_ir.status.success());
    assert!(String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_new_scalar"));
}

#[test]
fn default_route_rejects_non_copy_generic_list_construct_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_construct_rejected.mimi");
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected generic List construction explicit build");
    assert!(!explicit.status.success());
    assert!(String::from_utf8_lossy(&explicit.stderr).contains("canonical MIR"));

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic List construction default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("generic List facade"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_mir_generic_list_projection_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_projection.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List projection MIR dump");
    assert!(
        mir.status.success(),
        "canonical generic List projection MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    let mir_text = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_text.contains("project"));
    assert!(mir_text.contains("list_index=MirListIndexProjectionContract"));

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List projection reference run");
    assert_eq!(mir_run.status.code(), Some(41));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-projection-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List projection native build");
    assert!(
        build.status.success(),
        "canonical generic List projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List projection native binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(41));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List projection verifier");
    assert!(verification.status.success());
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List projection run");
    assert_eq!(default_run.status.code(), Some(41));
    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List projection native emit-ir");
    assert!(default_ir.status.success());
    assert!(String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_get_scalar"));
}

#[test]
fn canonical_mir_generic_list_index_one_projection_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_projection_index_one.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List index-one MIR dump");
    assert!(mir.status.success());
    assert!(
        String::from_utf8_lossy(&mir.stdout).contains("list_index=MirListIndexProjectionContract")
    );

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List index-one reference run");
    assert_eq!(mir_run.status.code(), Some(41));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-index-one-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List index-one native build");
    assert!(build.status.success());
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List index-one native binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(41));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List index-one verifier");
    assert!(verification.status.success());
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List index-one run");
    assert_eq!(default_run.status.code(), Some(41));
    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List index-one native emit-ir");
    assert!(default_ir.status.success());
    assert!(String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_get_scalar"));
}

#[test]
fn default_route_rejects_non_copy_generic_list_projection_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_projection_rejected.mimi");
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected generic List projection explicit build");
    assert!(!explicit.status.success());
    assert!(String::from_utf8_lossy(&explicit.stderr).contains("canonical MIR"));

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic List projection default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("generic List facade"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn default_route_rejects_non_copy_generic_list_concat_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_concat_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic List.concat default run");
    assert!(
        !run.status.success(),
        "non-Copy generic concat must fail closed"
    );
    let run_stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run_stderr.contains("default Canonical MIR route rejected"),
        "unexpected default generic concat diagnostic:\n{run_stderr}"
    );
    assert!(run_stderr.contains("generic List facade"), "{run_stderr}");
    assert!(
        !run_stderr.contains("bytecode runtime error"),
        "{run_stderr}"
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-list-concat-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected generic List.concat default build");
    let _ = fs::remove_file(&binary);
    assert!(
        !build.status.success(),
        "non-Copy generic concat build must fail closed"
    );
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(build_stderr.contains("default Canonical MIR route rejected"));
    assert!(build_stderr.contains("generic List facade"));
    assert!(
        !build_stderr.contains("E0700"),
        "legacy compiler leaked into route failure"
    );
}

#[test]
fn canonical_default_does_not_promote_non_facade_set_program() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_to_list.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default non-facade Set build");
    assert!(
        output.status.success(),
        "default non-facade Set build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ir = String::from_utf8_lossy(&output.stdout);
    assert!(
        !ir.contains("mimi_mir_set_to_list_scalar"),
        "an unqualified Set program was promoted to the generic facade island:\n{ir}"
    );
}

#[test]
fn canonical_mir_native_build_bool_list_index_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_bool.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical bool List reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-bool-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical bool List native build");
    assert!(
        build.status.success(),
        "canonical bool List native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical bool List native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_list_drop_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_drop.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List drop reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-drop-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List drop native build");
    assert!(
        build.status.success(),
        "canonical List drop native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List drop native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_list_return_abi_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_return.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List return reference run");
    assert_eq!(reference.status.code(), Some(20));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List return native build");
    assert!(
        build.status.success(),
        "canonical List return native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List return native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(20));
}

#[test]
fn canonical_mir_native_list_index_oob_matches_mir_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_oob.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List OOB reference run");
    assert_eq!(reference.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&reference.stderr).contains("E0803"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-oob-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List OOB native build");
    assert!(
        build.status.success(),
        "canonical List OOB native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List OOB native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native.stderr).contains("E0803"));
}

#[test]
fn canonical_mir_native_rejects_string_list_before_llvm_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-string-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical string List build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR build error"));
    assert!(stderr.contains("Copy scalar"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_run_cli_rejects_unsupported_list_shape_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn unsupported canonical MIR program");
    assert!(
        !output.status.success(),
        "unsupported MIR shape must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error") && stderr.contains("Copy scalar"),
        "unexpected canonical rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_verifier_proves_branch_contract() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_branch_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(
        output.status.success(),
        "canonical MIR verifier failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_record_contract_matches_reference_native_and_verifier() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_contract.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-record-contract-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record native build");
    assert!(
        build.status.success(),
        "canonical MIR record native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record verifier");
    assert!(
        verification.status.success(),
        "canonical MIR record verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_variant_contract_matches_reference_native_and_verifier() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_variant_contract.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR variant reference run");
    assert_eq!(reference.status.code(), Some(0));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-variant-contract-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR variant native build");
    assert!(
        build.status.success(),
        "canonical MIR variant native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR variant native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR variant verifier");
    assert!(
        verification.status.success(),
        "canonical MIR variant verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    let stdout = String::from_utf8_lossy(&verification.stdout);
    assert_eq!(
        stdout
            .matches("canonical MIR ensures contract proven")
            .count(),
        3
    );
}

#[test]
fn canonical_mir_record_contract_rejects_move_owned_projection() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_noncopy_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR record verifier");
    assert!(
        !output.status.success(),
        "move-owned projection must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("canonical MIR verifier input rejected"));
    assert!(stderr.contains("outside the canonical Copy aggregate contract"));
    assert!(!stderr.contains("flow_ast"));
}

#[test]
fn canonical_mir_record_projection_preserves_checked_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_trap_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record trap verifier");
    assert!(
        !output.status.success(),
        "reachable record-field trap must fail"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("can reach trap 'E0802'"));
}

#[test]
fn canonical_mir_verifier_reports_ensures_counterexample() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_disproven_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(!output.status.success(), "disproven contract must fail CLI");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("canonical MIR ensures contract is disproven"));
    assert!(!stdout.contains("flow_ast"));
}

#[test]
fn canonical_mir_verifier_reports_reachable_checked_arithmetic_trap() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_trap_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(!output.status.success(), "reachable trap must fail CLI");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("can reach trap 'E0802'"));
}

#[test]
fn canonical_mir_verifier_rejects_unsupported_abi_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_f64_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(!output.status.success(), "unsupported ABI must fail closed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("canonical MIR verifier input rejected"));
    assert!(stderr.contains("outside the canonical scalar verifier contract"));
    assert!(!stderr.contains("flow_ast"));
}

#[test]
fn real_world_cli_suite() {
    let root = project_root().join("tests").join("real_world");
    let mut sources: Vec<PathBuf> = fs::read_dir(&root)
        .expect("read tests/real_world")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "mimi"))
        .collect();

    let consumer = root.join("projects").join("consumer").join("main.mimi");
    if consumer.exists() {
        sources.push(consumer);
    }

    let mut failures = Vec::new();
    let mut known_gap_failures = Vec::new();

    for src in &sources {
        let name = src.file_name().unwrap().to_string_lossy();
        eprintln!("real_world_cli: checking {name}");

        // Prefer stdout-aware run for dual-backend match (esp. flow_* MCDD).
        let interp_out = run_mimi_run_out(src);
        let requires_codegen = !INTERPRETER_ONLY.contains(&name.as_ref());
        let codegen = if requires_codegen && can_link() {
            Some(run_mimi_build_and_exec(src))
        } else {
            if requires_codegen {
                eprintln!("SKIP build for {name}: cc not available");
            } else {
                eprintln!("SKIP build for {name}: interpreter-only fixture");
            }
            None
        };

        let mut details = String::new();
        if let Err(e) = &interp_out {
            details.push_str(&format!("[interp] {e}\n"));
        }
        if let Some(Err(e)) = &codegen {
            details.push_str(&format!("[codegen] {e}\n"));
        }
        // TC-C5 / L1: require matching stdout for all dual successes, not only
        // flow_* programs. Known gaps still route to known_gap_failures.
        if let (Ok(i), Some(Ok(c))) = (&interp_out, &codegen) {
            let i_trim = i.trim_end();
            let c_trim = c.trim_end();
            if i_trim != c_trim {
                details.push_str(&format!(
                    "[L1 dual-backend mismatch]\ninterp:\n{i_trim}\ncodegen:\n{c_trim}\n"
                ));
            }
        }
        if !details.is_empty() {
            if is_known_gap(src) {
                known_gap_failures.push((name.to_string(), details));
            } else {
                failures.push((name.to_string(), details));
            }
        }
    }

    for (name, details) in &known_gap_failures {
        eprintln!("KNOWN GAP (not failing the suite): {name}\n{details}");
    }

    if !failures.is_empty() {
        let mut msg = format!("{} real-world CLI test(s) failed:\n", failures.len());
        for (name, details) in &failures {
            msg.push_str(&format!("\n=== {name} ===\n{details}"));
        }
        panic!("{msg}");
    }
}
