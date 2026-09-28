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
fn default_map_root_run_build_verify_and_legacy_map_compatibility() {
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
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n0\n1\n");
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
    assert_eq!(String::from_utf8_lossy(&native.stdout), "1\n0\n1\n");

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

    let legacy_map = Command::new(&mimi)
        .env("MIMI_VERBOSE", "1")
        .arg("run")
        .arg(fixture("mir_map_root_dynamic_key_legacy.mimi"))
        .output()
        .expect("run legacy dynamic Map key CLI fixture");
    assert!(
        legacy_map.status.success(),
        "legacy Map/Any compatibility run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&legacy_map.stdout),
        String::from_utf8_lossy(&legacy_map.stderr)
    );
    assert!(String::from_utf8_lossy(&legacy_map.stdout).is_empty());
    let stderr = String::from_utf8_lossy(&legacy_map.stderr);
    assert!(
        stderr.contains("canonical route disposition: legacy"),
        "{stderr}"
    );
}

#[test]
fn default_string_map_root_run_build_and_verify() {
    let mimi = mimi_bin();
    let run_source = fixture("mir_map_root_string_default_run.mimi");
    let run = Command::new(&mimi)
        .env("MIMI_VERBOSE", "1")
        .arg("run")
        .arg(&run_source)
        .output()
        .expect("run String MapRoot CLI");
    assert!(
        run.status.success(),
        "String MapRoot run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2\n");
    assert!(String::from_utf8_lossy(&run.stderr)
        .contains("canonical route disposition: canonical (map-root-v1) mir_digest="));

    let built = std::env::temp_dir().join(format!(
        "mimi-string-map-root-route-{}-{}",
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
        .expect("build String MapRoot CLI");
    assert!(
        build.status.success(),
        "String MapRoot build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(String::from_utf8_lossy(&build.stderr)
        .contains("canonical route disposition: canonical (map-root-v1) mir_digest="));
    let native = Command::new(&built)
        .output()
        .expect("run built String MapRoot binary");
    let _ = std::fs::remove_file(&built);
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&native.stdout), "2\n");

    let verify = Command::new(&mimi)
        .env("MIMI_VERBOSE", "1")
        .arg("verify")
        .arg(fixture("mir_map_root_string_default_verify.mimi"))
        .output()
        .expect("verify String MapRoot CLI");
    assert!(
        verify.status.success(),
        "String MapRoot verify failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("contract proven"));
    assert!(String::from_utf8_lossy(&verify.stderr)
        .contains("canonical route disposition: canonical (map-root-v1) mir_digest="));
}

#[test]
fn default_cli_rejects_map_root_scalar_ffi_composition_without_fallback() {
    let mimi = mimi_bin();
    let stem = format!(
        "mimi-map-root-ffi-conflict-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("cli")
    );
    let source_path = std::env::temp_dir().join(format!("{stem}.mimi"));
    let binary_path = std::env::temp_dir().join(format!("{stem}-built"));
    std::fs::write(
        &source_path,
        r#"
            extern "C" {
                func scalar_probe(x: i32) -> i32
                    requires: x >= 0
                    ensures: result == x + 1;
            }
            func main() -> i32 {
                let root = map_new()
                let updated = map_set(root, "answer", scalar_probe(41 as i32))
                let size = map_size(updated)
                drop(updated)
                size
            }
        "#,
    )
    .expect("write temporary MapRoot plus scalar FFI source");

    let commands: [(&str, Vec<std::ffi::OsString>); 3] = [
        (
            "run",
            vec!["run".into(), source_path.as_os_str().to_os_string()],
        ),
        (
            "build",
            vec![
                "build".into(),
                source_path.as_os_str().to_os_string(),
                "--output".into(),
                binary_path.as_os_str().to_os_string(),
            ],
        ),
        (
            "verify",
            vec!["verify".into(), source_path.as_os_str().to_os_string()],
        ),
    ];
    for (mode, args) in commands {
        let output = Command::new(&mimi)
            .args(args)
            .env("MIMI_VERBOSE", "1")
            .output()
            .unwrap_or_else(|error| panic!("start mimi {mode}: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "mimi {mode} must reject the unsupported route, got stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(
            stdout.is_empty(),
            "mimi {mode} executed source before rejection: {stdout:?}"
        );
        assert!(
            stderr.contains("MapRoot profile cannot be combined"),
            "mimi {mode} did not report the hard MIR route boundary: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "mimi {mode} silently selected the legacy fallback: {stderr}"
        );
    }

    let _ = std::fs::remove_file(&source_path);
    let _ = std::fs::remove_file(&binary_path);
}
