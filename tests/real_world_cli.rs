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
    std::env::var_os("CARGO_BIN_EXE_mimi")
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root().join("target").join("debug").join("mimi"))
}

fn can_link() -> bool {
    Command::new("cc").arg("--version").output().is_ok()
}

/// Files that are expected to fail because they exercise known
/// language or codegen gaps. Keep this list minimal and aligned with
/// `tests/real_world/RESULTS.md`.
const KNOWN_GAPS: &[&str] = &[];

fn is_known_gap(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    KNOWN_GAPS.contains(&name)
}

fn run_mimi_run(src: &Path) -> Result<(), String> {
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(src)
        .output()
        .map_err(|e| format!("failed to spawn mimi run: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("mimi run failed\n{stderr}"))
    }
}

fn run_mimi_build_and_exec(src: &Path) -> Result<(), String> {
    let dir = src.parent().expect("src has parent");
    let stem = src.file_stem().expect("src has stem").to_string_lossy();
    let binary = dir.join(&*stem);

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
        return Err(format!("mimi build failed\n{stderr}"));
    }

    let exec_output = Command::new(&binary)
        .output()
        .map_err(|e| format!("failed to run compiled binary: {e}"))?;
    if exec_output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "compiled binary exited with {}",
            exec_output.status
        ))
    }
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

        let interp = run_mimi_run(src);
        let codegen = if can_link() {
            Some(run_mimi_build_and_exec(src))
        } else {
            eprintln!("SKIP build for {name}: cc not available");
            None
        };

        let any_failed = interp.is_err() || codegen.as_ref().is_some_and(|r| r.is_err());
        if any_failed {
            let mut details = String::new();
            if let Err(e) = &interp {
                details.push_str(&format!("[interp] {e}\n"));
            }
            if let Some(Err(e)) = &codegen {
                details.push_str(&format!("[codegen] {e}\n"));
            }
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
