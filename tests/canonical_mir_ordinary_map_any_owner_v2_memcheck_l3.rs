use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "map-any-owner-v2";
const EXPECTED_STDOUT: &str = "owned label\n";
const RECEIPT_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];

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
        .expect("Cargo must provide mimi for MapAnyOwner v2 Memcheck evidence")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_record.mimi")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_success(output: &Output, mode: &str) {
    assert!(
        output.status.success(),
        "{mode} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn assert_no_legacy_fallback(stderr: &str, mode: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} selected a legacy route: {stderr}"
    );
}

fn assert_default_route(stderr: &str, expected_digest: &str, mode: &str) {
    let marker = format!("canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest=");
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one MapAnyOwner v2 route receipt: {stderr}"
    );
    let observed_digest = matching[0]
        .split_once("mir_digest=")
        .expect("canonical route record contains a MIR digest")
        .1
        .trim();
    assert_eq!(
        observed_digest, expected_digest,
        "{mode} MIR digest differs"
    );
    assert_no_legacy_fallback(stderr, mode);
}

fn parse_manifest(output: &Output) -> BTreeMap<String, String> {
    assert_success(output, "mimi mir --receipt");
    let fields = text(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some("cli-mir-v1"),
        "the CLI receipt schema changed"
    );
    for field in RECEIPT_FIELDS {
        let digest = fields
            .get(*field)
            .unwrap_or_else(|| panic!("MIR receipt omitted {field}"));
        assert!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "MIR receipt has malformed {field}: {digest}"
        );
    }
    fields
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mimi-map-any-owner-v2-l3-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("integration")
        ));
        std::fs::create_dir_all(&path).expect("create isolated MapAnyOwner v2 L3 directory");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_strict_memcheck(binary: &Path, mode: &str) -> Vec<u8> {
    let output = Command::new("valgrind")
        .args([
            "--leak-check=full",
            "--show-leak-kinds=all",
            "--errors-for-leak-kinds=definite,indirect,possible",
            "--error-exitcode=97",
        ])
        .arg(binary)
        .output()
        .expect("run MapAnyOwner v2 native binary under strict Valgrind Memcheck");
    let report = text(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{mode} changed exit status or Memcheck found an error:\n{report}"
    );
    assert_eq!(
        output.stdout.as_slice(),
        EXPECTED_STDOUT.as_bytes(),
        "{mode} native stdout"
    );
    let all_freed = report.contains("All heap blocks were freed -- no leaks are possible");
    for kind in ["definitely lost", "indirectly lost", "possibly lost"] {
        assert!(
            all_freed || report.contains(&format!("{kind}: 0 bytes in 0 blocks")),
            "{mode} reported a {kind} owner leak:\n{report}"
        );
    }
    assert!(
        report.contains("ERROR SUMMARY: 0 errors"),
        "{mode} native execution had Memcheck errors:\n{report}"
    );
    eprintln!("[mimi-test] strict MapAnyOwner v2 Memcheck {mode} report:\n{report}");
    output.stdout
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn map_any_owner_v2_default_and_explicit_native_builds_are_strict_memcheck_clean() {
    assert!(
        Command::new("valgrind")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success()),
        "strict MapAnyOwner v2 L3 requires Valgrind"
    );

    let mimi = mimi_bin();
    let source = fixture();
    let receipt = Command::new(&mimi)
        .arg("mir")
        .arg("--all")
        .arg("--receipt")
        .arg(&source)
        .output()
        .expect("materialize MapAnyOwner v2 MIR receipt for L3");
    let manifest = parse_manifest(&receipt);
    let expected_digest = manifest
        .get("mir_digest")
        .expect("validated MIR receipt contains its graph digest")
        .clone();

    let directory = TemporaryDirectory::new();
    let mut outputs = Vec::new();
    // Exercise the explicit canonical emitter first so a closed default-route
    // gate cannot suppress the independent L3 evidence for this staged slice.
    for direct in [true, false] {
        let mode = if direct {
            "explicit MIR native binary"
        } else {
            "default native binary"
        };
        let binary = directory.0.join(if direct { "direct" } else { "default" });
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("build {mode}: {error}"));
        assert_success(&build, mode);
        let stderr = text(&build.stderr);
        assert_no_legacy_fallback(&stderr, mode);
        if !direct {
            assert_default_route(&stderr, &expected_digest, mode);
        }

        outputs.push(assert_strict_memcheck(&binary, mode));
    }
    assert_eq!(outputs[0], outputs[1], "default/direct native output");
}
