use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_static_generic_trait_specialization.mimi")
}

fn native_build_route_census(stderr: &[u8]) -> BTreeMap<String, String> {
    let stderr = String::from_utf8_lossy(stderr);
    let records = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        1,
        "successful canonical native build must emit exactly one route receipt:\n{stderr}"
    );
    let mut fields = BTreeMap::new();
    for field in records[0].split_whitespace().skip(2) {
        let (key, value) = field
            .split_once('=')
            .expect("canonical native route census key/value field");
        assert!(
            fields.insert(key.to_owned(), value.to_owned()).is_none(),
            "duplicate native build route census field {key}"
        );
    }
    fields
}

#[test]
fn static_generic_trait_specialization_runs_through_default_and_explicit_mir_cli_routes() {
    let mimi = mimi_bin();
    let source = fixture();
    let expected_stdout = "22\n11\n";

    let default = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("run")
        .arg(&source)
        .output()
        .expect("run static generic trait fixture through the default CLI route");
    assert!(
        default.status.success(),
        "default run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&default.stdout),
        String::from_utf8_lossy(&default.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&default.stdout), expected_stdout);
    let default_stderr = String::from_utf8_lossy(&default.stderr);
    assert!(
        default_stderr.contains(
            "canonical route disposition: canonical (static-generic-trait-specialization-v1) mir_digest="
        ),
        "default CLI must report the checked specialized MIR route: {default_stderr}"
    );
    assert!(!default_stderr.contains("canonical route disposition: legacy"));

    let explicit = Command::new(&mimi)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("run")
        .arg("--mir")
        .arg(&source)
        .output()
        .expect("run static generic trait fixture through explicit --mir");
    assert!(
        explicit.status.success(),
        "explicit MIR run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&explicit.stdout),
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert_eq!(explicit.stdout, default.stdout);
    let explicit_stderr = String::from_utf8_lossy(&explicit.stderr);
    assert!(
        explicit_stderr.contains(
            "canonical route disposition: explicit canonical (run-canonical-v1) mir_digest="
        ),
        "explicit CLI must report the MIR receipt used for bytecode compilation: {explicit_stderr}"
    );
    assert!(!explicit_stderr.contains("canonical route disposition: legacy"));
}

#[test]
fn static_generic_trait_build_receipt_tracks_the_native_artifact_on_both_routes() {
    let mimi = mimi_bin();
    let source = fixture();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let artifact_dir = std::env::temp_dir().join(format!(
        "mimi-static-generic-trait-build-census-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&artifact_dir).expect("create native build census directory");
    let default_binary = artifact_dir.join("default");
    let explicit_binary = artifact_dir.join("explicit-mir");

    let default = Command::new(&mimi)
        .arg("build")
        .arg(&source)
        .arg("-o")
        .arg(&default_binary)
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("build static generic trait fixture through default route");
    assert!(
        default.status.success(),
        "default native build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&default.stdout),
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        default_binary.is_file(),
        "default route produced no native artifact"
    );
    let default_receipt = native_build_route_census(&default.stderr);

    let explicit = Command::new(&mimi)
        .arg("build")
        .arg(&source)
        .arg("--mir")
        .arg("-o")
        .arg(&explicit_binary)
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("build static generic trait fixture through explicit --mir");
    assert!(
        explicit.status.success(),
        "explicit MIR native build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&explicit.stdout),
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(
        explicit_binary.is_file(),
        "explicit MIR route produced no native artifact"
    );
    let explicit_receipt = native_build_route_census(&explicit.stderr);

    assert_eq!(default_receipt["consumer"], "default");
    assert_eq!(explicit_receipt["consumer"], "explicit-mir");
    assert_eq!(default_receipt["route_id"], "build-canonical-v1");
    assert_eq!(
        default_receipt["route_id"], default_receipt["profile"],
        "the route id is the profile carried by CanonicalMirRouteReceipt"
    );
    let mut default_identity = default_receipt;
    let mut explicit_identity = explicit_receipt;
    default_identity.remove("consumer");
    explicit_identity.remove("consumer");
    assert_eq!(
        explicit_identity, default_identity,
        "default and explicit native artifacts must report the same exact MIR receipt"
    );

    let failed_binary = artifact_dir.join("missing-parent").join("failed-link");
    let failed_link = Command::new(&mimi)
        .arg("build")
        .arg(&source)
        .arg("--mir")
        .arg("-o")
        .arg(&failed_binary)
        .env("MIMI_ROUTE_CENSUS", "1")
        .output()
        .expect("attempt explicit MIR build whose output directory is absent");
    assert!(
        !failed_link.status.success(),
        "link into a missing output directory unexpectedly succeeded"
    );
    assert!(
        !String::from_utf8_lossy(&failed_link.stderr)
            .contains("MIMI_ROUTE_CENSUS canonical-native-build "),
        "failed explicit native build must not publish an artifact route receipt:\n{}",
        String::from_utf8_lossy(&failed_link.stderr)
    );
    assert!(
        !failed_binary.exists(),
        "failed link left a native artifact"
    );

    for binary in [&default_binary, &explicit_binary] {
        let native = Command::new(binary)
            .output()
            .expect("run native static generic trait artifact");
        assert!(
            native.status.success(),
            "native artifact failed: {}",
            String::from_utf8_lossy(&native.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&native.stdout), "22\n11\n");
    }

    std::fs::remove_dir_all(&artifact_dir).expect("remove native build census directory");
}
