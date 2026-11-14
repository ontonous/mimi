#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const ARGUMENT: &str = "雪の世界🧪";
const EXPECTED_STDOUT: &[u8] =
    b"\xE9\x9B\xAA\xE3\x81\xAE\xE4\xB8\x96\xE7\x95\x8C\xF0\x9F\xA7\xAA\n";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

const INDEX_OUTLIVES_LIST: &str = r#"
func main() -> i32 {
    let mut xs = args()
    let first = xs[0]
    xs = []
    println(first)
    0
}
"#;

const INDEX_COPY_CLEANUP_PRESERVES_LIST: &str = r#"
func args_after_index_copy_cleanup() -> (List<string>, i32) {
    let xs = args()
    let first = xs[0]
    let first_len = len(first)
    (xs, first_len)
}

func main() -> i32 {
    let result = args_after_index_copy_cleanup()
    let second = result.0[0]
    if len(second) != result.1 { return 2 }
    println(second)
    0
}
"#;

const ARGS_LIST_ONLY_CONTROL: &str = r#"
func main() -> i32 {
    let xs = args()
    drop(xs)
    0
}
"#;

const CASES: [(&str, &str); 2] = [
    ("index-outlives-parent-list-return", INDEX_OUTLIVES_LIST),
    (
        "index-copy-cleanup-preserves-returned-list",
        INDEX_COPY_CLEANUP_PRESERVES_LIST,
    ),
];

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mimi-args-string-index-{}-{id}-{label}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated args index test directory {}: {error}",
                path.display()
            )
        });
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "args string index evidence retained at {}",
                self.0.display()
            );
        } else {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn clear_mimi_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("MIMI_") {
            command.env_remove(key);
        }
    }
    command.env_remove("LD_PRELOAD");
}

fn native_build_route_receipt(stderr: &[u8], label: &str) -> BTreeMap<String, String> {
    const PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";
    let stderr = text(stderr);
    let records = stderr
        .lines()
        .filter(|line| line.starts_with(PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        1,
        "{label} must emit one native route receipt, proving which emitter built it:\n{stderr}"
    );
    let mut fields = BTreeMap::new();
    for field in records[0].split_whitespace().skip(2) {
        let (name, value) = field
            .split_once('=')
            .unwrap_or_else(|| panic!("malformed {label} native route field {field:?}"));
        assert!(
            fields.insert(name.to_owned(), value.to_owned()).is_none(),
            "{label} repeated native route field {name}"
        );
    }
    assert_eq!(fields.get("consumer").map(String::as_str), Some("default"));
    assert_eq!(
        fields.get("route_id").map(String::as_str),
        Some("build-canonical-v1"),
        "{label} must use the canonical native build route"
    );
    assert_eq!(
        fields.get("profile"),
        fields.get("route_id"),
        "{label} route id and canonical profile must agree"
    );
    let emitter_line = stderr
        .lines()
        .find(|line| line.starts_with("info: resolved emitter compiled "))
        .unwrap_or_else(|| panic!("{label} omitted resolved emitter census:\n{stderr}"));
    assert!(
        emitter_line.ends_with(", 0 fell back to legacy"),
        "{label} must compile every callable without a hidden legacy function fallback: {emitter_line}"
    );
    for name in ["mir_digest", "route_identity_digest"] {
        let value = fields
            .get(name)
            .unwrap_or_else(|| panic!("{label} native route receipt omitted {name}"));
        assert!(
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{label} native route {name} must be a lowercase SHA-256 digest"
        );
    }
    fields
}

fn strict_valgrind() -> PathBuf {
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict L3 execution requires MIMI_REQUIRE_VALGRIND=1"
    );
    let configured = std::env::var_os("MIMI_L3_VALGRIND")
        .expect("MIMI_L3_VALGRIND must name the resolved Valgrind executable");
    let configured = PathBuf::from(configured);
    assert!(configured.is_absolute(), "Valgrind path must be absolute");
    let canonical = fs::canonicalize(&configured)
        .unwrap_or_else(|error| panic!("resolve Valgrind {}: {error}", configured.display()));
    assert_eq!(configured, canonical, "Valgrind path must be canonical");
    let metadata = fs::metadata(&canonical).expect("inspect resolved Valgrind");
    assert!(metadata.is_file(), "Valgrind must be a regular file");
    assert_ne!(
        metadata.permissions().mode() & 0o111,
        0,
        "Valgrind must be executable"
    );
    canonical
}

fn strict_evidence_directory() -> PathBuf {
    let configured = std::env::var_os("MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR")
        .expect("MIMI_ARGS_STRING_INDEX_ARTIFACT_DIR must name an external evidence directory");
    let configured = PathBuf::from(configured);
    assert!(
        configured.is_absolute(),
        "strict L3 evidence path must be absolute"
    );
    let parent = configured
        .parent()
        .expect("strict L3 evidence path must have a parent");
    let file_name = configured
        .file_name()
        .expect("strict L3 evidence path must have a final component");
    assert!(
        file_name != "." && file_name != "..",
        "invalid evidence directory name"
    );
    let parent = fs::canonicalize(parent)
        .unwrap_or_else(|error| panic!("resolve evidence parent {}: {error}", parent.display()));
    let evidence = parent.join(file_name);
    let repo = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).expect("resolve repository root");
    assert!(
        !evidence.starts_with(&repo),
        "strict L3 evidence must be outside the repository: {}",
        evidence.display()
    );
    fs::create_dir(&evidence).unwrap_or_else(|error| {
        panic!(
            "create fresh strict L3 evidence directory {} without overwriting: {error}",
            evidence.display()
        )
    });
    evidence
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed with {:?}:\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} must return exact process status 0"
    );
}

fn write_source(directory: &Path, label: &str, source: &str) -> PathBuf {
    let path = directory.join(format!("{label}.mimi"));
    fs::write(&path, source).expect("write args string index source");
    path
}

fn run_bytecode(source: &Path, label: &str) -> Output {
    let mut command = Command::new(mimi_bin());
    clear_mimi_environment(&mut command);
    let output = command
        .arg("run")
        .arg(source)
        .arg("--")
        .arg(ARGUMENT)
        .output()
        .unwrap_or_else(|error| panic!("start {label} Bytecode run: {error}"));
    assert_success(&output, &format!("{label} Bytecode run"));
    assert_eq!(output.stdout, EXPECTED_STDOUT, "{label} Bytecode output");
    output
}

fn build_native(source: &Path, directory: &Path, label: &str) -> PathBuf {
    let binary = directory.join(format!("{label}-native"));
    let mut command = Command::new(mimi_bin());
    clear_mimi_environment(&mut command);
    let output = command
        .arg("build")
        .arg(source)
        .arg("--output")
        .arg(&binary)
        .env("MIMI_ROUTE_CENSUS", "1")
        .env("MIMI_VERBOSE", "1")
        .output()
        .unwrap_or_else(|error| panic!("start {label} native build: {error}"));
    assert_success(&output, &format!("{label} native build"));
    assert!(binary.is_file(), "{label} native build produced no binary");
    let route = native_build_route_receipt(&output.stderr, label);
    fs::write(
        directory.join(format!("{label}.build.stdout")),
        &output.stdout,
    )
    .expect("retain native build stdout");
    fs::write(
        directory.join(format!("{label}.build.stderr")),
        &output.stderr,
    )
    .expect("retain native build stderr");
    let receipt = route
        .iter()
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect::<String>();
    fs::write(
        directory.join(format!("{label}.route-receipt.txt")),
        receipt,
    )
    .expect("retain native route receipt");
    binary
}

fn run_native(binary: &Path, label: &str) -> Output {
    let mut command = Command::new(binary);
    clear_mimi_environment(&mut command);
    let output = command
        .arg(ARGUMENT)
        .output()
        .unwrap_or_else(|error| panic!("start {label} native executable: {error}"));
    assert_success(&output, &format!("{label} native executable"));
    assert_eq!(output.stdout, EXPECTED_STDOUT, "{label} native output");
    output
}

fn assert_cross_backend(case_id: &str, source_text: &str) {
    let directory = TemporaryDirectory::new(case_id);
    let source = write_source(directory.path(), case_id, source_text);
    run_bytecode(&source, case_id);
    let binary = build_native(&source, directory.path(), case_id);
    run_native(&binary, case_id);
}

#[test]
fn args_string_index_value_outlives_list_and_matches_bytecode() {
    assert_cross_backend(CASES[0].0, CASES[0].1);
}

#[test]
fn cleanup_of_args_string_index_copy_preserves_live_list_and_matches_bytecode() {
    assert_cross_backend(CASES[1].0, CASES[1].1);
}

#[test]
#[ignore = "strict native List<String> index ownership Memcheck L3; run with MIMI_REQUIRE_VALGRIND=1"]
fn args_string_index_ownership_is_strict_memcheck_clean() {
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict L3 execution requires MIMI_REQUIRE_VALGRIND=1"
    );
    let evidence = strict_evidence_directory();
    let valgrind = strict_valgrind();
    let valgrind_version = Command::new(&valgrind)
        .arg("--version")
        .output()
        .expect("strict L3 requires a working Valgrind executable");
    assert_success(&valgrind_version, "Valgrind version probe");
    fs::write(
        evidence.join("valgrind-version.stdout"),
        &valgrind_version.stdout,
    )
    .expect("retain Valgrind version output");
    fs::write(
        evidence.join("valgrind-version.stderr"),
        &valgrind_version.stderr,
    )
    .expect("retain Valgrind version diagnostics");
    fs::write(
        evidence.join("tool-paths.txt"),
        format!("valgrind={}\n", valgrind.display()),
    )
    .expect("retain strict L3 tool identity");

    let control_source = write_source(&evidence, "args-list-only-control", ARGS_LIST_ONLY_CONTROL);
    let control_binary = build_native(&control_source, &evidence, "args-list-only-control");
    let control_log = evidence.join("args-list-only-control.memcheck.log");
    let mut control_command = Command::new(&valgrind);
    clear_mimi_environment(&mut control_command);
    let control_output = control_command
        .arg("--tool=memcheck")
        .arg("--leak-check=full")
        .arg("--show-leak-kinds=all")
        .arg("--errors-for-leak-kinds=definite,indirect,possible")
        .arg("--error-exitcode=97")
        .arg(format!("--log-file={}", control_log.display()))
        .arg(&control_binary)
        .arg(ARGUMENT)
        .output()
        .expect("run args-list-only Memcheck control");
    assert_success(&control_output, "args-list-only Memcheck control");
    fs::write(
        evidence.join("args-list-only-control.memcheck.stdout"),
        &control_output.stdout,
    )
    .expect("retain args-list-only Memcheck stdout");
    fs::write(
        evidence.join("args-list-only-control.memcheck.stderr"),
        &control_output.stderr,
    )
    .expect("retain args-list-only Memcheck stderr");
    assert!(
        control_output.stdout.is_empty(),
        "args-list-only Memcheck control must be silent"
    );
    let control_report = fs::read_to_string(&control_log).expect("read control Memcheck report");
    let control_summary = memcheck_summary::parse(&control_report);
    assert_eq!(
        (
            control_summary.still_reachable_bytes,
            control_summary.still_reachable_blocks
        ),
        (0, 0),
        "args-list-only control must leave no reachable allocations:\n{}",
        control_summary.rendered
    );
    eprintln!(
        "[mimi-test] args-list-only strict Memcheck control:\n{}",
        control_summary.rendered
    );
    fs::write(
        evidence.join("args-list-only-control.memcheck-summary.txt"),
        control_summary.rendered.as_bytes(),
    )
    .expect("retain args-list-only Memcheck summary");

    for (case_id, source_text) in CASES {
        let source = write_source(&evidence, case_id, source_text);
        let binary = build_native(&source, &evidence, case_id);
        let log = evidence.join(format!("{case_id}.memcheck.log"));
        let mut command = Command::new(&valgrind);
        clear_mimi_environment(&mut command);
        command
            .arg("--tool=memcheck")
            .arg("--leak-check=full")
            .arg("--show-leak-kinds=all")
            .arg("--errors-for-leak-kinds=definite,indirect,possible")
            .arg("--error-exitcode=97")
            .arg(format!("--log-file={}", log.display()))
            .arg(&binary)
            .arg(ARGUMENT);
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("start {case_id} strict Memcheck: {error}"));
        assert_success(&output, &format!("{case_id} strict Memcheck"));
        fs::write(
            evidence.join(format!("{case_id}.memcheck.stdout")),
            &output.stdout,
        )
        .expect("retain strict Memcheck stdout");
        fs::write(
            evidence.join(format!("{case_id}.memcheck.stderr")),
            &output.stderr,
        )
        .expect("retain strict Memcheck stderr");
        assert_eq!(output.stdout, EXPECTED_STDOUT, "{case_id} Memcheck output");
        let report = fs::read_to_string(&log).expect("read raw strict Memcheck report");
        let summary = memcheck_summary::parse(&report);
        assert_eq!(
            (
                summary.still_reachable_bytes,
                summary.still_reachable_blocks
            ),
            (0, 0),
            "{case_id} retained reachable allocations:\n{}",
            summary.rendered
        );
        assert_eq!(
            (summary.still_reachable_bytes, summary.still_reachable_blocks),
            (
                control_summary.still_reachable_bytes,
                control_summary.still_reachable_blocks
            ),
            "{case_id} retained extra reachable allocations beyond the args-list-only control:\ncontrol:\n{}\ncase:\n{}",
            control_summary.rendered,
            summary.rendered
        );
        eprintln!(
            "[mimi-test] {case_id} strict Memcheck:\n{}",
            summary.rendered
        );
        fs::write(
            evidence.join(format!("{case_id}.memcheck-summary.txt")),
            summary.rendered.as_bytes(),
        )
        .expect("retain strict Memcheck summary");
    }
    fs::write(evidence.join("expected-output.bin"), EXPECTED_STDOUT)
        .expect("retain expected native output");
    fs::write(
        evidence.join("test-input.txt"),
        format!("argument={ARGUMENT}\n"),
    )
    .expect("retain strict test input");
}
