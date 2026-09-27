use std::path::{Path, PathBuf};
use std::process::Command;

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide the mimi binary for CLI route evidence")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn default_map_root_run_build_verify_and_unsupported_cli_boundary() {
    let mimi = mimi_bin();
    let run_source = fixture("mir_map_root_default_run.mimi");

    // This source has no user requires/ensures. It exercises default run on
    // the Checker-receipted MIR path and makes the selected path observable in
    // CLI diagnostics without changing program stdout.
    let run = Command::new(&mimi)
        .env("MIMI_VERBOSE", "1")
        .arg("run")
        .arg(&run_source)
        .output()
        .expect("run mimi CLI");
    assert!(
        run.status.success(),
        "mimi run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n");
    assert!(String::from_utf8_lossy(&run.stderr)
        .contains("canonical route disposition: canonical (map-root-v1) mir_digest="));

    let built = std::env::temp_dir().join(format!(
        "mimi-map-root-route-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("cli")
    ));
    let build = Command::new(&mimi)
        .env("MIMI_VERBOSE", "1")
        .arg("build")
        .arg(&run_source)
        .arg("--output")
        .arg(&built)
        .output()
        .expect("build mimi CLI");
    assert!(
        build.status.success(),
        "mimi build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(String::from_utf8_lossy(&build.stderr)
        .contains("canonical route disposition: canonical (map-root-v1) mir_digest="));
    let native = Command::new(&built)
        .output()
        .expect("run built MapRoot binary");
    let _ = std::fs::remove_file(&built);
    assert!(native.status.success());
    assert_eq!(String::from_utf8_lossy(&native.stdout), "1\n");

    let verify = Command::new(&mimi)
        .env("MIMI_VERBOSE", "1")
        .arg("verify")
        .arg(fixture("mir_map_root_default_verify.mimi"))
        .output()
        .expect("verify mimi CLI");
    assert!(
        verify.status.success(),
        "mimi verify failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("contract proven"));
    assert!(String::from_utf8_lossy(&verify.stderr)
        .contains("canonical route disposition: canonical (map-root-v1) mir_digest="));

    let unsupported = Command::new(&mimi)
        .arg("run")
        .arg(fixture("mir_map_root_dynamic_key_rejected.mimi"))
        .output()
        .expect("run unsupported MapRoot CLI fixture");
    assert!(!unsupported.status.success());
    let stderr = String::from_utf8_lossy(&unsupported.stderr);
    assert!(stderr.contains("MIR-COVERAGE-001"), "{stderr}");
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{stderr}"
    );
}
