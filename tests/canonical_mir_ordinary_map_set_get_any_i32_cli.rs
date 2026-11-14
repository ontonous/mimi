use mimi::core::check_program;
use mimi::core::mir::{
    materialize_ordinary_map_set_get_any_i32_route,
    ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE,
    ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_L2_VERIFIER_PROFILE,
};
use mimi::core::resolved::{CheckedProgram, NodeId};
use mimi::lexer::Lexer;
use mimi::parser::Parser;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str =
    include_str!("fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi");

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
        .expect("Cargo must provide the mimi binary for Set/GetAny-i32 CLI evidence")
}

struct TempPath(PathBuf);

impl TempPath {
    fn new(label: &str, extension: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "mimi-{label}-{}-{nonce}.{extension}",
            std::process::id()
        )))
    }

    fn write_source(label: &str, source: &str) -> Self {
        let path = Self::new(label, "mimi");
        std::fs::write(&path.0, source).expect("write temporary Set/GetAny-i32 source");
        path
    }
}

impl Drop for TempPath {
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

fn census_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(key))
}

fn route_identity(stderr: &str, consumer: &str, mode: &str, profile: &str) -> RouteIdentity {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} {consumer} entered a legacy route: {stderr}"
    );
    let line = stderr
        .lines()
        .find(|line| {
            line.contains(&format!(
                "MIMI_ROUTE_CENSUS route-admission consumer={consumer}"
            )) && line.contains(&format!("mode={mode}"))
                && line.contains(&format!("checker_profile={profile}"))
        })
        .unwrap_or_else(|| {
            panic!("{mode} {consumer} omitted the exact {profile} route receipt: {stderr}")
        });
    RouteIdentity {
        profile: census_value(line, "checker_profile=")
            .expect("route census reports the profile")
            .to_owned(),
        mir_digest: census_value(line, "mir_digest=")
            .filter(|digest| !digest.is_empty())
            .expect("route census reports the MIR digest")
            .to_owned(),
        receipt_identity_digest: census_value(line, "receipt_identity_digest=")
            .filter(|digest| !digest.is_empty())
            .expect("route census reports the route receipt identity")
            .to_owned(),
    }
}

fn mode(explicit_mir: bool) -> &'static str {
    if explicit_mir {
        "explicit-mir"
    } else {
        "default"
    }
}

fn run(mimi: &Path, explicit_mir: bool, source: &Path) -> Output {
    let mut command = Command::new(mimi);
    command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg(source)
        .output()
        .expect("run Set/GetAny-i32 fixture")
}

fn build(mimi: &Path, explicit_mir: bool, source: &Path, output: &Path) -> Output {
    let mut command = Command::new(mimi);
    command
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("build")
        .arg(source);
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg("--output")
        .arg(output)
        .output()
        .expect("build Set/GetAny-i32 fixture")
}

fn verify(mimi: &Path, explicit_mir: bool, source: &Path) -> Output {
    let mut command = Command::new(mimi);
    command.env("MIMI_ROUTE_CENSUS", "1").arg("verify");
    if explicit_mir {
        command.arg("--mir");
    }
    command
        .arg(source)
        .output()
        .expect("verify Set/GetAny-i32 fixture")
}

fn source_with_ensures(source: &str, predicate: &str) -> String {
    let (prefix, body) = source
        .split_once("func main() -> i32 {\n")
        .expect("fixture has a main function");
    format!("{prefix}func main() -> i32 {{\n    ensures: {predicate}\n{body}")
}

fn source_for_key(key: &str) -> String {
    FIXTURE.replace(
        "map_get(updated, \"answer\")",
        &format!("map_get(updated, \"{key}\")"),
    )
}

fn check_source(source: &str) -> CheckedProgram {
    let tokens = Lexer::new(source).tokenize().expect("lex fixture");
    let file = Parser::new(tokens).parse_file().expect("parse fixture");
    check_program(&file).expect("typecheck fixture")
}

fn reference_result(source: &str) -> i64 {
    let checked = check_source(source);
    let program = materialize_ordinary_map_set_get_any_i32_route(&checked)
        .expect("materialize exact public GetAny route for the reference executor");
    let route = program.route_receipt(ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE);
    let execution = mimi::core::mir::reference::MirReferenceInterpreter::new(&program)
        .with_route_receipt(&route)
        .execute(&NodeId("function:main".into()), &[])
        .expect("execute exact GetAny route in the reference interpreter");
    match execution {
        mimi::core::mir::reference::MirRuntimeValue::Int(value) => value,
        other => panic!("expected an i32 reference result, got {other:?}"),
    }
}

#[test]
fn ordinary_map_set_get_any_i32_hits_and_misses_share_default_and_explicit_routes() {
    let mimi = mimi_bin();
    let hit = TempPath::write_source("set-get-any-i32-hit", FIXTURE);
    let miss_source = source_for_key("absent");
    let miss = TempPath::write_source("set-get-any-i32-miss", &miss_source);
    assert_eq!(reference_result(FIXTURE), 42, "reference hit value");
    assert_eq!(reference_result(&miss_source), 0, "canonical miss sentinel");

    for (label, source, expected) in [("hit", &hit.0, 42), ("miss", &miss.0, 0)] {
        let mut runs = Vec::new();
        let mut builds = Vec::new();
        for explicit_mir in [false, true] {
            let mode = mode(explicit_mir);
            let run_output = run(&mimi, explicit_mir, source);
            let run_stderr = String::from_utf8_lossy(&run_output.stderr);
            assert_eq!(
                run_output.status.code(),
                Some(expected),
                "{mode} {label} returned the wrong GetAny result:\nstdout:\n{}\nstderr:\n{run_stderr}",
                String::from_utf8_lossy(&run_output.stdout)
            );
            runs.push(route_identity(
                &run_stderr,
                "run",
                mode,
                ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE,
            ));

            let binary = TempPath::new(&format!("set-get-any-i32-{label}-{mode}"), "bin");
            let build_output = build(&mimi, explicit_mir, source, &binary.0);
            let build_stderr = String::from_utf8_lossy(&build_output.stderr);
            assert!(
                build_output.status.success(),
                "{mode} {label} build failed:\nstdout:\n{}\nstderr:\n{build_stderr}",
                String::from_utf8_lossy(&build_output.stdout)
            );
            builds.push(route_identity(
                &build_stderr,
                "build",
                mode,
                ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE,
            ));
            let native = Command::new(&binary.0)
                .output()
                .expect("run the native Set/GetAny-i32 artifact");
            assert_eq!(
                native.status.code(),
                Some(expected),
                "{mode} {label} native binary returned the wrong result: {}",
                String::from_utf8_lossy(&native.stderr)
            );
        }
        assert_eq!(
            runs[0], runs[1],
            "default and explicit --mir {label} run must share route identity"
        );
        assert_eq!(
            builds[0], builds[1],
            "default and explicit --mir {label} build must share route identity"
        );
        assert_eq!(
            runs[0], builds[0],
            "run and build must share the {label} execution route identity"
        );
    }
}

#[test]
fn ordinary_map_set_get_any_i32_verifier_reports_nonrecoverable_capacity() {
    let mimi = mimi_bin();
    let hit = TempPath::write_source(
        "set-get-any-i32-hit-capacity",
        &source_with_ensures(
            FIXTURE,
            "result == 42 || result == 0 || result == 2 || result == 3",
        ),
    );
    let miss = TempPath::write_source(
        "set-get-any-i32-miss-capacity",
        &source_with_ensures(
            &source_for_key("absent"),
            "result == 0 || result == 2 || result == 3",
        ),
    );

    for (label, source) in [("hit", &hit.0), ("miss", &miss.0)] {
        let mut identities = Vec::new();
        for explicit_mir in [false, true] {
            let mode = mode(explicit_mir);
            let output = verify(&mimi, explicit_mir, source);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{mode} {label} must report the reachable non-recoverable GetAny capacity trap:\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
            assert!(
                stderr.contains("E0800"),
                "{mode} {label} omitted the reachable GetAny publication-capacity trap: {stderr}"
            );
            identities.push(route_identity(
                &stderr,
                "verify",
                mode,
                ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_L2_VERIFIER_PROFILE,
            ));
        }
        assert_eq!(
            identities[0], identities[1],
            "default and explicit --mir {label} verification must share route identity"
        );
    }
}

#[test]
fn ordinary_map_set_get_any_i32_near_miss_is_rejected_before_legacy_consumers() {
    let mimi = mimi_bin();
    let near_miss = FIXTURE.replace(
        "        Err(status) => status,",
        "        Err(status) => { println(1); status },",
    );
    assert_ne!(
        near_miss, FIXTURE,
        "the New Err mutation must change the fixture"
    );
    let source = TempPath::write_source("set-get-any-i32-new-err-extra-statement", &near_miss);

    for explicit_mir in [false, true] {
        let selected_mode = mode(explicit_mir);
        let binary = TempPath::new(&format!("set-get-any-i32-near-miss-{selected_mode}"), "bin");
        let outputs = [
            ("run", run(&mimi, explicit_mir, &source.0)),
            ("build", build(&mimi, explicit_mir, &source.0, &binary.0)),
            ("verify", verify(&mimi, explicit_mir, &source.0)),
        ];
        for (consumer, output) in outputs {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{selected_mode} {consumer} admitted a non-exact New Err block:\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
            assert!(
                stderr.contains("E0200")
                    || stderr.contains("MIR-COVERAGE-001")
                    || stderr.contains("MIR-ROUTE-RECEIPT-001"),
                "{selected_mode} {consumer} did not report a typed canonical route rejection:\n{stderr}"
            );
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{selected_mode} {consumer} fell back after the exact receipt failed:\n{stderr}"
            );
        }
        assert!(
            !binary.0.exists(),
            "{selected_mode} build emitted an artifact for a rejected near miss"
        );
    }
}
