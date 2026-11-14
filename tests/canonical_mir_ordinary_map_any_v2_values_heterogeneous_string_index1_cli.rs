use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const PROFILE: &str = "map-any-owner-v2-values-heterogeneous-string-index1-v1";
const POSITIVE: &str = "mir_ordinary_map_any_owner_v2_values_heterogeneous_string_index1.mimi";
const WRONG_INDEX: &str =
    "mir_ordinary_map_any_owner_v2_values_heterogeneous_string_index0_fail_closed.mimi";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
        .expect("Cargo must provide mimi for heterogeneous Values route coverage")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn command(mimi: &Path, consumer: &str, direct: bool, source: &Path) -> Output {
    let mut command = Command::new(mimi);
    command
        .env("MIMI_ROUTE_CENSUS", "1")
        .env("MIMI_VERBOSE", "1")
        .arg(consumer);
    if direct {
        command.arg("--mir");
    }
    command
        .arg(source)
        .output()
        .unwrap_or_else(|error| panic!("{consumer} {}: {error}", source.display()))
}

fn route_fields(
    output: &Output,
    consumer: &str,
    direct: bool,
    expected_status: i32,
) -> BTreeMap<String, String> {
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{consumer} status; stdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{consumer} route receipt:\n{stderr}");
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route receipt prefix")
        .split_whitespace()
        .map(|field| {
            field
                .split_once('=')
                .unwrap_or_else(|| panic!("{consumer} malformed route field {field:?}"))
        })
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer").map(String::as_str), Some(consumer));
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(if direct { "explicit-mir" } else { "default" })
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(PROFILE)
    );
    for key in ["mir_digest", "receipt_identity_digest"] {
        let digest = fields
            .get(key)
            .unwrap_or_else(|| panic!("{consumer} omitted {key}"));
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{consumer} malformed {key}: {digest}"
        );
    }
    assert_no_fallback(&stderr, consumer);
    fields
}

fn assert_no_fallback(stderr: &str, label: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} fell back to the legacy route:\n{stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: canonical")
            || stderr.contains(&format!(
                "canonical route disposition: canonical ({PROFILE})"
            )),
        "{label} selected another canonical profile:\n{stderr}"
    );
}

fn assert_same_identity(
    left: &BTreeMap<String, String>,
    right: &BTreeMap<String, String>,
    label: &str,
) {
    for key in ["checker_profile", "mir_digest", "receipt_identity_digest"] {
        assert_eq!(left.get(key), right.get(key), "{label} changed {key}");
    }
}

fn assert_wrong_index_checker_rejection(output: &Output, label: &str) {
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded:\n{diagnostic}"
    );
    assert_eq!(
        diagnostic.matches("error[E0200]").count(),
        1,
        "{label} must be rejected by the exact Checker diagnostic:\n{diagnostic}"
    );
    assert!(
        diagnostic.contains("direct List<Any> indexed cast requires one exact Values unpack receipt with payload disposition and explicit snapshot Drop")
            && diagnostic.contains("snapshot[0] as string"),
        "{label} changed the fail-closed index-zero Checker rejection:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("MIMI_ROUTE_CENSUS route-admission "),
        "{label} emitted a route receipt before Checker rejection:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: canonical"),
        "{label} announced a canonical route before Checker rejection:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: legacy"),
        "{label} fell back to a compatibility route:\n{diagnostic}"
    );
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mimi-values-heterogeneous-index1-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated native output directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn heterogeneous_values_index1_runs_builds_and_verifies_on_both_cli_routes() {
    const {
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let mimi = mimi_bin();
    let source = fixture(POSITIVE);
    let mut identity = None;

    for direct in [false, true] {
        let output = command(&mimi, "run", direct, &source);
        assert_eq!(text(&output.stdout), "true\n", "run output");
        let route = route_fields(&output, "run", direct, 0);
        if let Some(previous) = &identity {
            assert_same_identity(previous, &route, "run route");
        } else {
            identity = Some(route);
        }
    }

    let directory = TempDir::new();
    for direct in [false, true] {
        let binary = directory
            .0
            .join(if direct { "explicit-mir" } else { "default" });
        let mut build = Command::new(&mimi);
        build
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if direct {
            build.arg("--mir");
        }
        let output = build.output().expect("spawn native build");
        assert!(
            output.status.success(),
            "build failed:\n{}",
            text(&output.stderr)
        );
        assert!(binary.is_file(), "build did not produce the native binary");
        let route = route_fields(&output, "build", direct, 0);
        if let Some(previous) = &identity {
            assert_same_identity(previous, &route, "build route");
        }
        let native = Command::new(&binary).output().expect("run native binary");
        assert_eq!(native.status.code(), Some(0), "native exit status");
        assert_eq!(text(&native.stdout), "true\n", "native output");
    }

    for direct in [false, true] {
        let output = command(&mimi, "verify", direct, &source);
        assert!(
            output.status.success(),
            "verify failed:\n{}",
            text(&output.stderr)
        );
        assert!(
            text(&output.stdout).contains("No contracts to verify in"),
            "verify output changed: {}",
            text(&output.stdout)
        );
        let route = route_fields(&output, "verify", direct, 0);
        if let Some(previous) = &identity {
            assert_same_identity(previous, &route, "verify route");
        }
    }
}

#[test]
fn heterogeneous_values_wrong_string_index_fails_closed_on_all_cli_routes() {
    let mimi = mimi_bin();
    let source = fixture(WRONG_INDEX);
    let directory = TempDir::new();
    for direct in [false, true] {
        let output = command(&mimi, "run", direct, &source);
        assert_wrong_index_checker_rejection(&output, "wrong selected payload run");

        let binary = directory.0.join(if direct {
            "wrong-explicit"
        } else {
            "wrong-default"
        });
        let mut build = Command::new(&mimi);
        build
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if direct {
            build.arg("--mir");
        }
        let output = build.output().expect("spawn rejected native build");
        assert_wrong_index_checker_rejection(&output, "wrong selected payload build");
        assert!(!binary.exists(), "rejected build left a native binary");

        let output = command(&mimi, "verify", direct, &source);
        assert_wrong_index_checker_rejection(&output, "wrong selected payload verify");
    }
}
