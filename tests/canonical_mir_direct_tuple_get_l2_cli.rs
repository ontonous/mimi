use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const SOURCE: &str = r#"
func main() -> i32 {
    match map_new() {
        Ok(empty) => match map_set(empty, "stored", 41) {
            Ok(root) => {
                drop(empty)
                let pair = map_get(root, "stored")
                drop(root)
                let decoded = unpack_i32(pair.1)
                drop(pair)
                drop(decoded)
                0
            },
            Err(status) => { drop(empty); status },
        },
        Err(status) => status,
    }
}
"#;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows UNIX epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-direct-tuple-get-l1-cli-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated direct tuple L2 directory {}: {error}",
                path.display()
            )
        });
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_source(directory: &TempDir) -> PathBuf {
    let source = directory.path().join("direct_tuple_get_l1.mimi");
    fs::write(&source, SOURCE).expect("write direct tuple L2 source");
    source
}

fn run_cli(command_name: &str, explicit_mir: bool, source: &Path, output: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mimi"));
    for key in std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("MIMI_"))
        .collect::<Vec<_>>()
    {
        command.env_remove(key);
    }
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg(command_name);
    if explicit_mir {
        command.arg("--mir");
    }
    command.arg(source);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("start Mimi {command_name}: {error}"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn route_census_value<'a>(stderr: &'a str, field_name: &str) -> &'a str {
    let line = stderr
        .lines()
        .find(|line| line.contains("MIMI_ROUTE_CENSUS route-admission"))
        .unwrap_or_else(|| panic!("missing route-admission census in stderr: {stderr}"));
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{field_name}=")))
        .unwrap_or_else(|| panic!("missing {field_name} in route census: {line}"))
}

#[test]
fn direct_tuple_get_l1_cli_runs_and_builds_while_verify_keeps_l2_projection() {
    let directory = TempDir::new();
    let source = write_source(&directory);
    let mut receipt_identity = None;

    for (command, explicit_mir) in [
        ("run", false),
        ("run", true),
        ("build", false),
        ("build", true),
    ] {
        let binary = directory.path().join(format!(
            "direct-tuple-{}-{}",
            command,
            if explicit_mir { "mir" } else { "default" }
        ));
        let output = (command == "build").then_some(binary.as_path());
        let result = run_cli(command, explicit_mir, &source, output);
        let stderr = text(&result.stderr);
        assert!(
            result.status.success(),
            "{command} {} failed to execute the direct L1 route:\nstdout:\n{}\nstderr:\n{stderr}",
            if explicit_mir { "--mir" } else { "by default" },
            text(&result.stdout),
        );
        if command == "run" {
            assert!(
                result.stdout.is_empty(),
                "the exact source returns zero without printing: {}",
                text(&result.stdout),
            );
        }
        let checker_profile = route_census_value(&stderr, "checker_profile");
        assert!(
            checker_profile
                == mimi::core::mir::ORDINARY_MAP_DIRECT_TUPLE_GET_UNPACK_L1_CORE_PROFILE,
            "execution must use the exact Checker-bound L1 profile, got {checker_profile}: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("canonical route disposition: compatibility"),
            "execution selected a Legacy/compatibility route: {stderr}"
        );
        assert!(
            !stderr.contains("VerifierOnly"),
            "execution tried to use the verifier-only L2 semantic projection: {stderr}"
        );
        let identity = route_census_value(&stderr, "receipt_identity_digest").to_owned();
        if let Some(expected) = &receipt_identity {
            assert_eq!(
                &identity, expected,
                "default/explicit run and build must consume the same route receipt"
            );
        } else {
            receipt_identity = Some(identity);
        }
        if command == "build" {
            assert!(
                binary.is_file(),
                "successful native build did not create {}",
                binary.display()
            );
            let native = Command::new(&binary)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap_or_else(|error| {
                    panic!(
                        "start direct tuple native executable {}: {error}",
                        binary.display()
                    )
                });
            assert!(
                native.status.success(),
                "direct tuple native executable failed: status={:?}, stderr={}",
                native.status.code(),
                text(&native.stderr),
            );
            assert!(native.stdout.is_empty(), "native executable printed output");
        }
    }

    for explicit_mir in [false, true] {
        let result = run_cli("verify", explicit_mir, &source, None);
        let stdout = text(&result.stdout);
        let stderr = text(&result.stderr);
        assert!(
            result.status.success(),
            "verify {} rejected the semantic projection:\nstdout:\n{stdout}\nstderr:\n{stderr}",
            if explicit_mir { "--mir" } else { "by default" },
        );
        assert_eq!(
            stdout.lines().count(),
            1,
            "verifier emits one explicit semantic disposition: {stdout:?}"
        );
        assert!(
            stdout.contains("L2SemanticProjectionValidated:")
                && stdout.contains(
                    "semantic projection validated; no runtime ownership theorem claimed"
                )
                && stdout
                    .contains("ordinary-map-direct-tuple-get-unpack-l2-semantic-projection-v1"),
            "verifier must expose the non-contract L2 disposition and exact profile: {stdout}"
        );
        assert!(
            !stdout.contains("Proven") && !stdout.contains("NoObligations"),
            "semantic projection must not be reported as a contract verdict: {stdout}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("canonical route disposition: compatibility"),
            "verifier unexpectedly selected a compatibility route: {stderr}"
        );
    }
}
