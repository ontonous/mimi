use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROFILE: &str = "map-any-owner-v2-removed-key-checked-miss-v1";
const FIXTURE: &str = "tests/fixtures/mir_ordinary_map_any_owner_v2_removed_key_checked_miss.mimi";
const STDLIB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/std");

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide mimi for RemovedKeyCheckedMiss CLI evidence")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn route_identity(
    output: &Output,
    consumer: &str,
    mode: &str,
    expected_status: i32,
) -> (String, String) {
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{consumer} {mode} exit status:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{consumer} {mode} fell back to Legacy:\n{stderr}"
    );
    if consumer == "run" {
        assert!(
            output.stdout.is_empty(),
            "{mode} run wrote unexpected stdout"
        );
        assert!(
            !stderr.contains("runtime error") && !stderr.contains("[E080"),
            "{mode} returned the Boolean process code with no runtime failure: {stderr}"
        );
    }

    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{consumer} {mode} must emit one route-admission row:\n{stderr}"
    );
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route-admission row has its stable prefix")
        .split_whitespace()
        .map(|field| {
            field
                .split_once('=')
                .unwrap_or_else(|| panic!("{consumer} {mode} route field is malformed: {field:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("consumer"),
        Some(&consumer),
        "{consumer} consumer"
    );
    assert_eq!(fields.get("mode"), Some(&mode), "{consumer} {mode} mode");
    assert_eq!(
        fields.get("checker_profile"),
        Some(&PROFILE),
        "{consumer} {mode} Checker profile"
    );
    let digest = *fields.get("mir_digest").expect("route row has MIR digest");
    let receipt = *fields
        .get("receipt_identity_digest")
        .expect("route row has receipt identity digest");
    for (label, value) in [("MIR digest", digest), ("receipt identity digest", receipt)] {
        assert!(
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{consumer} {mode} has a malformed {label}: {value}"
        );
    }
    (digest.to_string(), receipt.to_string())
}

fn native_build_receipt(output: &Output, identity: &(String, String), mode: &str) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{mode} native receipt:\n{stderr}");
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS canonical-native-build ")
        .expect("native build receipt prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("native receipt key=value"))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&mode), "{mode} consumer");
    assert_eq!(fields.get("route_id"), Some(&PROFILE), "{mode} route id");
    assert_eq!(fields.get("profile"), Some(&PROFILE), "{mode} profile");
    assert_eq!(
        fields.get("mir_digest"),
        Some(&identity.0.as_str()),
        "{mode} native MIR digest"
    );
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&identity.1.as_str()),
        "{mode} native route identity digest"
    );
}

fn verifier_receipt(output: &Output, identity: &(String, String), mode: &str) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("canonical MIR verifier provenance: "))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{mode} verifier receipt:\n{stderr}");
    let fields = lines[0]
        .strip_prefix("canonical MIR verifier provenance: ")
        .expect("verifier provenance prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("verifier receipt key=value"))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("profile"), Some(&PROFILE), "{mode} profile");
    assert_eq!(
        fields.get("mir_digest"),
        Some(&identity.0.as_str()),
        "{mode} verifier MIR digest"
    );
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&identity.1.as_str()),
        "{mode} verifier route identity digest"
    );
}

fn temporary_directory() -> PathBuf {
    for attempt in 0..100_u32 {
        let path = std::env::temp_dir().join(format!(
            "mimi-removed-key-checked-miss-cli-{}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create temporary CLI directory {}: {error}", path.display()),
        }
    }
    panic!("cannot allocate a fresh RemovedKeyCheckedMiss CLI directory")
}

#[test]
fn removed_key_checked_miss_uses_default_and_explicit_mir_for_run_build_and_verify() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "RemovedKeyCheckedMiss must not open global Map/Any V2 readiness"
        );
    };
    let mimi = mimi_bin();
    let source = fixture();
    let temp = temporary_directory();
    let mut identities = Vec::with_capacity(6);

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .env("MIMI_STDLIB", STDLIB)
            .arg("run");
        if explicit_mir {
            command.arg("--mir");
        }
        let output = command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("start {mode} RemovedKeyCheckedMiss run: {error}"));
        identities.push((
            format!("{mode}-run"),
            route_identity(&output, "run", mode, 1),
        ));
    }

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let binary = temp.join(format!("removed-key-checked-miss-{mode}"));
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .env("MIMI_STDLIB", STDLIB)
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if explicit_mir {
            command.arg("--mir");
        }
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("start {mode} RemovedKeyCheckedMiss build: {error}"));
        assert!(
            output.status.success(),
            "{mode} build failed:\nstdout:\n{}\nstderr:\n{}",
            text(&output.stdout),
            text(&output.stderr)
        );
        assert!(binary.is_file(), "{mode} build produced no native binary");
        let identity = route_identity(&output, "build", mode, 0);
        native_build_receipt(&output, &identity, mode);
        let native = Command::new(&binary)
            .env_remove("MIMI_ROUTE_CENSUS")
            .env_remove("MIMI_VERBOSE")
            .output()
            .unwrap_or_else(|error| panic!("run {mode} native binary: {error}"));
        assert_eq!(native.status.code(), Some(1), "{mode} native exit code");
        assert!(native.stdout.is_empty(), "{mode} native stdout");
        assert!(native.stderr.is_empty(), "{mode} native stderr");
        identities.push((format!("{mode}-build"), identity));
    }

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .env("MIMI_STDLIB", STDLIB)
            .arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        let output = command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("start {mode} RemovedKeyCheckedMiss verify: {error}"));
        let identity = route_identity(&output, "verify", mode, 0);
        verifier_receipt(&output, &identity, mode);
        identities.push((format!("{mode}-verify"), identity));
    }

    assert_eq!(identities.len(), 6);
    let expected_modes = [
        "default-run",
        "explicit-mir-run",
        "default-build",
        "explicit-mir-build",
        "default-verify",
        "explicit-mir-verify",
    ];
    for ((observed_mode, identity), expected_mode) in identities.iter().zip(expected_modes) {
        assert_eq!(observed_mode, expected_mode, "route evidence order changed");
        assert_eq!(
            identity, &identities[0].1,
            "{observed_mode} changed RemovedKeyCheckedMiss MIR/receipt identity"
        );
    }
    const {
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    fs::remove_dir_all(&temp).unwrap_or_else(|error| {
        panic!("remove temporary CLI directory {}: {error}", temp.display())
    });
}
