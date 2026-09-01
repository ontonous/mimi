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
/// Both former gaps (flow_order_system.mimi and flow_system_trace.mimi)
/// now pass in interpreter and codegen, so the list is empty.
const KNOWN_GAPS: &[&str] = &[];

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
fn canonical_mir_verifier_reports_owned_string_result_without_fallback() {
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
        "verifier should report a closed subset"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("owned String slice requires a Copy scalar result"),
        "{stdout}"
    );
    assert!(stdout.contains("0/1 verified"), "{stdout}");
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
            && stderr.contains("ABI Float")
            && stderr.contains("outside the Copy scalar native contract"),
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
