use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str = "maps_from_list_roundtrip.mimi";
const FIXTURE_SHA256: &str = "b445c9ab6013142e5b92dae87455672970210db30bff3ab29239d7f8e72ab1e2";
const EXPECTED_STDOUT: &[u8] = b"3\n";
const EXPECTED_EXIT: i32 = 0;

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
        .expect("Cargo must provide mimi for the direct Map fixture contract")
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(FIXTURE)
}

fn assert_fixture_sha256(path: &Path) {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("hash Map roundtrip fixture {}: {error}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let digest = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum output contains the fixture digest")
        .to_owned();
    assert_eq!(
        digest,
        FIXTURE_SHA256,
        "Map roundtrip fixture bytes changed: {}",
        path.display()
    );
}

fn normalize_run_stdout(bytes: &[u8]) -> Vec<u8> {
    let stdout = String::from_utf8_lossy(bytes);
    let mut lines = stdout.lines().collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.starts_with("-> ")) {
        lines.pop();
    }
    if lines.is_empty() {
        Vec::new()
    } else {
        format!("{}\n", lines.join("\n")).into_bytes()
    }
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mimi-map-roundtrip-contract-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("integration")
        ));
        std::fs::create_dir_all(&path).expect("create isolated Map roundtrip output directory");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_success(output: &Output, command: &str) {
    assert_eq!(
        output.status.code(),
        Some(EXPECTED_EXIT),
        "{command} returned the wrong result; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn maps_from_list_roundtrip_fixture_keeps_its_output_and_result() {
    let fixture = fixture_path();
    assert_fixture_sha256(&fixture);
    let mimi = mimi_bin();

    let interpreted = Command::new(&mimi)
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("run the direct Map roundtrip fixture");
    assert_success(&interpreted, "mimi run");
    assert_eq!(
        normalize_run_stdout(&interpreted.stdout),
        EXPECTED_STDOUT,
        "mimi run changed the Map roundtrip's expected final size"
    );

    let output_directory = TemporaryDirectory::new();
    let native_binary = output_directory.0.join("maps_from_list_roundtrip");
    let build = Command::new(&mimi)
        .arg("build")
        .arg(&fixture)
        .arg("--output")
        .arg(&native_binary)
        .output()
        .expect("build the direct Map roundtrip fixture");
    assert!(
        build.status.success(),
        "mimi build failed: stdout={} stderr={}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let native = Command::new(&native_binary)
        .output()
        .expect("run the native Map roundtrip fixture");
    assert_success(&native, "native Map roundtrip");
    assert_eq!(
        native.stdout, EXPECTED_STDOUT,
        "native codegen changed the Map roundtrip's expected final size"
    );
}
