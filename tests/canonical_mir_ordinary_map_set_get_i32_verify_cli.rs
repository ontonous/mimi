use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROFILE: &str = "ordinary-map-set-get-i32-l2-verifier-v1";
const FIXTURE: &str =
    include_str!("fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi");

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
        .expect("Cargo must provide the mimi binary for Set/Get-i32 verifier CLI evidence")
}

struct TempSource(PathBuf);

impl TempSource {
    fn new(label: &str, source: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("mimi-{label}-{}-{nonce}.mimi", std::process::id()));
        std::fs::write(&path, source).expect("write temporary Set/Get-i32 verifier source");
        Self(path)
    }
}

impl Drop for TempSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RouteIdentity {
    profile: String,
    mir_digest: String,
    receipt_identity_digest: String,
}

fn source_with_ensures(predicate: &str) -> String {
    let (prefix, body) = FIXTURE
        .split_once("func main() -> i32 {\n")
        .expect("exact fixture has a main function");
    format!("{prefix}func main() -> i32 {{\n    ensures: {predicate}\n{body}")
}

fn verify_command(mimi: &Path, explicit_mir: bool, source: &Path) -> Output {
    let mut command = Command::new(mimi);
    command.env("MIMI_ROUTE_CENSUS", "1").arg("verify");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg(source)
        .output()
        .expect("run the Set/Get-i32 verifier CLI")
}

fn census_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(key))
}

fn route_identity(stderr: &str, mode: &str) -> RouteIdentity {
    assert!(
        stderr.contains(&format!(
            "canonical route disposition: canonical ({PROFILE})"
        )),
        "{mode} did not select the exact Set/Get-i32 L2 verifier route: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} fell back to a legacy verifier route: {stderr}"
    );
    let line = stderr
        .lines()
        .find(|line| {
            line.contains("MIMI_ROUTE_CENSUS route-admission consumer=verify")
                && line.contains(PROFILE)
        })
        .unwrap_or_else(|| panic!("{mode} has no source-bound verifier route census: {stderr}"));
    RouteIdentity {
        profile: census_value(line, "checker_profile=")
            .expect("route census has the Checker profile")
            .to_owned(),
        mir_digest: census_value(line, "mir_digest=")
            .filter(|digest| !digest.is_empty())
            .expect("route census has the MIR digest")
            .to_owned(),
        receipt_identity_digest: census_value(line, "receipt_identity_digest=")
            .filter(|digest| !digest.is_empty())
            .expect("route census has the receipt identity digest")
            .to_owned(),
    }
}

#[test]
fn ordinary_map_set_get_i32_verify_uses_same_default_and_explicit_l2_route() {
    let mimi = mimi_bin();
    let positive = TempSource::new(
        "set-get-i32-l2-positive",
        &source_with_ensures("result == 42 || result == 2 || result == 3"),
    );
    let false_claim = TempSource::new(
        "set-get-i32-l2-false",
        &source_with_ensures("result == 42 || result == 3"),
    );
    let mut positive_routes = Vec::new();
    let mut negative_routes = Vec::new();

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit --mir"
        } else {
            "default"
        };

        let output = verify_command(&mimi, explicit_mir, &positive.0);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{mode} L2 positive contract did not verify:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains("canonical MIR ensures contract proven")
                && stdout.contains("[8 constraints]"),
            "{mode} did not produce a constrained positive proof: {stdout}"
        );
        positive_routes.push(route_identity(&stderr, &format!("{mode} positive")));

        let output = verify_command(&mimi, explicit_mir, &false_claim.0);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{mode} accepted a false Result-status claim:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stderr.contains("counterexample: result = 2"),
            "{mode} false claim did not preserve the reachable Set Err(2) counterexample: {stderr}"
        );
        negative_routes.push(route_identity(&stderr, &format!("{mode} negative")));
    }

    assert_eq!(positive_routes[0], positive_routes[1]);
    assert_eq!(negative_routes[0], negative_routes[1]);
    assert_ne!(
        positive_routes[0].mir_digest, negative_routes[0].mir_digest,
        "the positive and false source contracts must have different MIR identities"
    );
}
