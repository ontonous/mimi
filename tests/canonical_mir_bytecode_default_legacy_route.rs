use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const SOURCE: &str = r#"func label(flag: bool) -> Option<string> {
    match flag {
        true => Some("yes"),
        false => None,
    }
}

func main() -> i32 {
    let mut acc = 1;
    acc = acc + 1;
    let value = label(true);
    let text = value.unwrap();
    println(text);
    println(acc);
    0
}

func test_legacy_route() -> bool {
    true
}
"#;

const EXPECTED_SOURCE_SHA256: &str =
    "8e17422f6c1a49cef9ee16864aff212b1e283cfd7a736317430a2175ed4bbbe6";
const LEGACY_REASON: &str =
    "canonical route disposition: legacy (mixed-coverage-without-materialized-candidate)";

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mimi-bytecode-default-legacy-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temporary legacy-route directory");
        Self(path)
    }

    fn source(&self) -> PathBuf {
        self.0.join("bytecode_default_legacy.mimi")
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg("--")
        .arg(path)
        .output()
        .expect("run sha256sum for the generated Mimi source");
    assert!(
        output.status.success(),
        "sha256sum command failed for {}: status={:?}, stdout={}, stderr={}",
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum prints a digest")
        .to_owned()
}

fn run_cli(args: &[&str], source: &Path) -> (Vec<String>, Output) {
    let mut argv = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    argv.push(source.display().to_string());
    let output = Command::new(mimi_bin())
        .args(&argv)
        .env("MIMI_VERBOSE", "1")
        .env_remove("MIMI_ROUTE_CENSUS")
        .env("NO_COLOR", "1")
        .output()
        .unwrap_or_else(|error| panic!("failed to spawn mimi {:?}: {error}", argv));
    (argv, output)
}

fn command_text(argv: &[String]) -> String {
    format!("{} {:?}", mimi_bin().display(), argv)
}

fn assert_legacy_route(
    label: &str,
    argv: &[String],
    output: &Output,
    expected_status: i32,
    source_sha256: &str,
) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{label}: source_sha256={source_sha256}; command={}; stdout={stdout:?}; stderr={stderr:?}",
        command_text(argv),
    );
    assert!(
        stderr.contains(LEGACY_REASON),
        "{label}: expected exact default Legacy disposition; source_sha256={source_sha256}; command={}; stdout={stdout:?}; stderr={stderr:?}",
        command_text(argv),
    );
}

fn assert_explicit_mir_rejected_by_cli(
    label: &str,
    argv: &[String],
    output: &Output,
    source_sha256: &str,
) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{label}: expected clap option rejection; source_sha256={source_sha256}; command={}; stdout={stdout:?}; stderr={stderr:?}",
        command_text(argv),
    );
    assert!(
        stderr.contains("unexpected argument '--mir'"),
        "{label}: expected explicit unknown-option diagnostic; source_sha256={source_sha256}; command={}; stdout={stdout:?}; stderr={stderr:?}",
        command_text(argv),
    );
    assert!(
        !stdout.contains("canonical route disposition:")
            && !stderr.contains("canonical route disposition:"),
        "{label}: CLI option rejection must precede routing and cannot report a fallback; source_sha256={source_sha256}; command={}; stdout={stdout:?}; stderr={stderr:?}",
        command_text(argv),
    );
}

#[test]
fn bytecode_default_legacy_run_test_disasm_and_explicit_mir_boundaries() {
    let directory = TemporaryDirectory::new();
    let source = directory.source();
    fs::write(&source, SOURCE).expect("write temporary route-matrix Mimi source");
    let source_sha256 = sha256(&source);
    assert_eq!(
        source_sha256, EXPECTED_SOURCE_SHA256,
        "generated source changed; update the source-bound hash only with reviewed source changes"
    );

    // All three default commands select the same compatibility disposition.
    let (run_argv, run) = run_cli(&["run"], &source);
    assert_legacy_route("default run", &run_argv, &run, 0, &source_sha256);
    assert_eq!(
        run.stdout,
        b"yes\n2\n",
        "default run: source_sha256={source_sha256}; command={}; stderr={}",
        command_text(&run_argv),
        String::from_utf8_lossy(&run.stderr),
    );

    let (test_argv, test) = run_cli(&["test"], &source);
    assert_legacy_route("default test", &test_argv, &test, 0, &source_sha256);
    assert_eq!(
        test.stdout,
        "Running 1 test(s)...\n\n  ✓ test_legacy_route\n\n1 passed, 0 failed\n".as_bytes(),
        "default test: source_sha256={source_sha256}; command={}; stderr={}",
        command_text(&test_argv),
        String::from_utf8_lossy(&test.stderr),
    );

    let (disasm_argv, disasm) = run_cli(&["disasm"], &source);
    assert_legacy_route("default disasm", &disasm_argv, &disasm, 0, &source_sha256);
    let disasm_stdout = String::from_utf8_lossy(&disasm.stdout);
    assert!(
        disasm_stdout.contains("; function:main ")
            && disasm_stdout.contains("; test_legacy_route ")
            && disasm_stdout.contains("CALL_BUILTIN"),
        "default disasm: expected source functions and bytecode instructions; source_sha256={source_sha256}; command={}; stdout={disasm_stdout:?}; stderr={}",
        command_text(&disasm_argv),
        String::from_utf8_lossy(&disasm.stderr),
    );

    // `run` has an explicit canonical route and must reject this unsupported
    // source before execution without retrying the Bytecode Legacy compiler.
    let (run_mir_argv, run_mir) = run_cli(&["run", "--mir"], &source);
    let run_mir_stdout = String::from_utf8_lossy(&run_mir.stdout);
    let run_mir_stderr = String::from_utf8_lossy(&run_mir.stderr);
    assert_eq!(
        run_mir.status.code(),
        Some(1),
        "explicit MIR run: source_sha256={source_sha256}; command={}; stdout={run_mir_stdout:?}; stderr={run_mir_stderr:?}",
        command_text(&run_mir_argv),
    );
    assert!(
        run_mir_stdout.is_empty()
            && run_mir_stderr.contains("canonical MIR run rejected")
            && !run_mir_stderr.contains("canonical route disposition: legacy"),
        "explicit MIR run must reject before execution and never fall back; source_sha256={source_sha256}; command={}; stdout={run_mir_stdout:?}; stderr={run_mir_stderr:?}",
        command_text(&run_mir_argv),
    );

    // Test and disasm currently have no `--mir` option. Their explicit flag
    // attempts must stop at clap parsing, before route selection or fallback.
    let (test_mir_argv, test_mir) = run_cli(&["test", "--mir"], &source);
    assert_explicit_mir_rejected_by_cli(
        "test --mir is not a supported CLI route",
        &test_mir_argv,
        &test_mir,
        &source_sha256,
    );

    let (disasm_mir_argv, disasm_mir) = run_cli(&["disasm", "--mir"], &source);
    assert_explicit_mir_rejected_by_cli(
        "disasm --mir is not a supported CLI route",
        &disasm_mir_argv,
        &disasm_mir,
        &source_sha256,
    );
}
