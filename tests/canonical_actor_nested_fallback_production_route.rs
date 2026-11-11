//! Source-bound production route contract for the two original Actor
//! nested-function regressions. This test intentionally exercises the same
//! Checker/MIR receipt through direct Rust consumers and the public CLI.

use mimi::core::mir::{self, reference::MirReferenceInterpreter};
use mimi::core::resolved::CheckedProgram;
use mimi::core::resolved::NodeId;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CAPTURE_PATH: &str = "tests/real_world/actor_nested_func_capture.mimi";
const CAPTURE_SHA256: &str = "81dca400a724bebdcdb3c89e0be8399fe5a44cf81bac021021750242cafa394e";
const SHADOW_PATH: &str = "tests/real_world/actor_nested_func_shadow.mimi";
const SHADOW_SHA256: &str = "7be9f2463ab0c593c29e3d456be636ff15d455a3a2a91e3a5d82757f0bba32ac";
const PROFILE: &str = mir::SCALAR_ACTOR_ISLAND;

struct Fixture {
    path: &'static str,
    sha256: &'static str,
    stdout: &'static [u8],
}

const FIXTURES: [Fixture; 2] = [
    Fixture {
        path: CAPTURE_PATH,
        sha256: CAPTURE_SHA256,
        stdout: b"15\n",
    },
    Fixture {
        path: SHADOW_PATH,
        sha256: SHADOW_SHA256,
        stdout: b"42\n101\n",
    },
];

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("coreutils sha256sum is required to verify pinned fixture bytes");
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum digest")
        .to_owned()
}

fn checked_source(source: &str, path: &Path) -> CheckedProgram {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("pinned Actor fixture lexes");
    let mut file = mimi::loader::parser_for_path(tokens, path)
        .expect("register the same stable workspace source identity as the CLI")
        .parse_file()
        .expect("pinned Actor fixture parses");
    mimi::loader::merge_prelude_into(&mut file);
    mimi::core::check_program(&file).expect("pinned Actor fixture checks")
}

fn field<'a>(line: &'a str, name: &str) -> &'a str {
    line.split_whitespace()
        .find_map(|item| item.strip_prefix(name)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("missing {name}= field in route census line: {line}"))
}

fn route_census(output: &Output, command: &str, explicit_mir: bool) -> (String, String) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mode = if explicit_mir {
        "explicit-mir"
    } else {
        "default"
    };
    let prefix = format!(
        "MIMI_ROUTE_CENSUS route-admission consumer={command} mode={mode} checker_profile="
    );
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{command} {mode} must emit one canonical receipt identity: {stderr}"
    );
    let line = lines[0];
    assert_eq!(field(line, "checker_profile"), PROFILE, "{stderr}");
    if command == "build" {
        assert_eq!(field(line, "build_route_profile"), PROFILE, "{stderr}");
    }
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "Actor route must never fall back to legacy: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: verifier-only"),
        "Actor route must be executable and shared, not verifier-only: {stderr}"
    );
    if explicit_mir && command == "run" {
        assert!(
            stderr.contains(&format!(
                "canonical route disposition: explicit canonical ({PROFILE})"
            )),
            "--mir must affirm canonical selection: {stderr}"
        );
    } else if !explicit_mir {
        assert!(
            stderr.contains(&format!(
                "canonical route disposition: canonical ({PROFILE})"
            )),
            "default route must affirm canonical selection: {stderr}"
        );
    }
    let digest = field(line, "mir_digest").to_owned();
    let receipt_identity = field(line, "receipt_identity_digest").to_owned();
    for (name, value) in [
        ("mir_digest", digest.as_str()),
        ("receipt_identity_digest", receipt_identity.as_str()),
    ] {
        assert_eq!(value.len(), 64, "{name}: {line}");
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "{name} must be lowercase SHA-256 hex: {line}"
        );
    }
    (digest, receipt_identity)
}

fn invoke(
    mimi: &Path,
    command: &str,
    source: &Path,
    explicit_mir: bool,
    output: Option<&Path>,
) -> Output {
    let mut child = Command::new(mimi);
    child
        .current_dir(project_root())
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg(command)
        .arg(source);
    if explicit_mir {
        child.arg("--mir");
    }
    if let Some(output_path) = output {
        child.arg("--output").arg(output_path);
    }
    child.output().unwrap_or_else(|error| {
        panic!(
            "failed to invoke mimi {command} for {}: {error}",
            source.display()
        )
    })
}

#[test]
fn original_nested_actor_capture_and_shadow_use_shared_canonical_production_routes() {
    let mimi = mimi_bin();
    for fixture in FIXTURES {
        let source_path = project_root().join(fixture.path);
        let source = std::fs::read_to_string(&source_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", source_path.display()));
        assert_eq!(
            sha256(&source_path),
            fixture.sha256,
            "fixture source changed; explicitly re-adjudicate its expected behavior and receipt"
        );

        // Direct Rust path: source bytes -> Checker -> canonical MIR receipt ->
        // reference and production Bytecode VM. This proves the Actor owner is
        // in the production graph before the CLI dispatch checks below.
        let checked = checked_source(&source, &source_path);
        let route = mir::materialize_canonical_mir_route(&checked, None)
            .expect("original Actor fixture materializes a canonical route");
        assert!(
            route.admission.scalar_actor,
            "Actor admission: {:?}",
            route.admission
        );
        assert!(
            mir::CanonicalMirRouteProfile::ScalarActor.is_materialized(&route),
            "the shared route must include the ScalarActor canonical profile"
        );
        let receipt = route.program.route_receipt(PROFILE);
        assert_eq!(receipt.profile, PROFILE);
        assert_eq!(
            receipt.manifest_round_trip().expect("sealed Actor receipt"),
            receipt
        );

        let reference = MirReferenceInterpreter::new(&route.program).with_route_receipt(&receipt);
        let observation = reference
            .execute_with_output(&NodeId("function:main".into()), &[])
            .expect("canonical Actor reference executes");
        assert_eq!(observation.value, mir::reference::MirRuntimeValue::Int(0));
        assert_eq!(observation.output.as_bytes(), fixture.stdout);

        let bytecode = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
            &route.program,
            &receipt,
        )
        .expect("production bytecode adapter accepts the same Actor receipt");
        assert!(
            bytecode.ast.is_none(),
            "Actor VM path must not retain a legacy AST"
        );
        assert!(
            bytecode.actor_defs.is_empty() && bytecode.actor_method_funcs.is_empty(),
            "Actor VM must execute MIR actor metadata, not legacy actor tables"
        );
        let mut vm = mimi::interp::bytecode::BytecodeVM::new(bytecode);
        vm.run().expect("canonical Actor production VM executes");
        assert_eq!(vm.stdout().as_bytes(), fixture.stdout);
        let counts = vm
            .canonical_actor_domain_counts()
            .expect("canonical Actor runtime accounting");
        assert_eq!(
            counts.live_roots, 0,
            "all canonical Actor roots close: {counts:?}"
        );
        assert_eq!(
            counts.live_workers, 0,
            "all canonical Actor workers join: {counts:?}"
        );
        assert_eq!(
            counts.joined_workers, 1,
            "one spawned worker is joined: {counts:?}"
        );
        assert_eq!(counts.live_states, 0, "Actor states close: {counts:?}");
        assert_eq!(counts.live_controls, 0, "Actor controls close: {counts:?}");
        assert_eq!(counts.live_tasks, 0, "Actor tasks close: {counts:?}");
        assert_eq!(
            counts.pending_replies, 0,
            "Actor replies settle: {counts:?}"
        );
        assert_eq!(
            counts.live_message_buffers, 0,
            "message buffers settle: {counts:?}"
        );
        assert_eq!(
            counts.live_reply_buffers, 0,
            "reply buffers settle: {counts:?}"
        );

        let inspection = Command::new(&mimi)
            .arg("mir")
            .arg(&source_path)
            .arg("--receipt")
            .output()
            .expect("inspect the same source-bound Actor receipt");
        assert!(
            inspection.status.success(),
            "Actor MIR receipt inspection failed: {}",
            String::from_utf8_lossy(&inspection.stderr)
        );
        let manifest = String::from_utf8_lossy(&inspection.stdout);
        let inspected_digest = manifest
            .lines()
            .find_map(|line| line.strip_prefix("mir_digest="))
            .expect("MIR inspection receipt digest");
        assert_eq!(inspected_digest, receipt.mir_digest);
        let expected_profile_line = "profile=cli-mir-v1";
        assert!(
            manifest.lines().any(|line| line == expected_profile_line),
            "MIR inspection must expose its generic inspection profile: {manifest}"
        );
        assert_eq!(receipt.profile, PROFILE);

        let temp = std::env::temp_dir().join(format!(
            "mimi-actor-fallback-contract-{}-{}",
            std::process::id(),
            fixture.sha256
        ));
        std::fs::create_dir_all(&temp).expect("create isolated Actor output directory");
        let mut route_identities: [[Option<String>; 2]; 3] =
            std::array::from_fn(|_| std::array::from_fn(|_| None));
        for explicit_mir in [false, true] {
            for (command_index, command) in ["run", "build", "verify"].into_iter().enumerate() {
                let binary = temp.join(format!(
                    "{}-{}",
                    if explicit_mir { "explicit" } else { "default" },
                    command
                ));
                let observed = invoke(
                    &mimi,
                    command,
                    &source_path,
                    explicit_mir,
                    (command == "build").then_some(binary.as_path()),
                );
                let stderr = String::from_utf8_lossy(&observed.stderr);
                assert!(
                    observed.status.success(),
                    "{} {command} explicit_mir={explicit_mir}: {stderr}",
                    fixture.path
                );
                let (cli_mir_digest, cli_receipt_identity) =
                    route_census(&observed, command, explicit_mir);
                assert_eq!(cli_mir_digest, receipt.mir_digest);
                assert_eq!(
                    cli_receipt_identity,
                    receipt.route_identity_digest(),
                    "CLI and direct Rust must carry the same source-bound receipt"
                );
                let census_line = stderr
                    .lines()
                    .find(|line| {
                        line.starts_with(&format!(
                            "MIMI_ROUTE_CENSUS route-admission consumer={command} mode={}",
                            if explicit_mir {
                                "explicit-mir"
                            } else {
                                "default"
                            }
                        ))
                    })
                    .expect("one Actor route census line");
                let identity = field(census_line, "receipt_identity_digest").to_owned();
                let mode_index = if explicit_mir { 1 } else { 0 };
                route_identities[command_index][mode_index] = Some(identity);

                if command == "run" {
                    assert_eq!(observed.stdout, fixture.stdout, "VM result: {stderr}");
                } else if command == "verify" {
                    let stdout = String::from_utf8_lossy(&observed.stdout);
                    assert!(stdout.contains("No contracts to verify"), "{stdout}");
                    assert!(
                        !stdout.contains("Proven"),
                        "NoContracts cannot claim proof: {stdout}"
                    );
                } else {
                    let native = Command::new(&binary)
                        .output()
                        .expect("run canonical native Actor build");
                    assert!(
                        native.status.success(),
                        "canonical native binary failed: {}",
                        String::from_utf8_lossy(&native.stderr)
                    );
                    assert_eq!(native.stdout, fixture.stdout, "native Actor result");
                }
            }
        }
        for command_index in 0..3 {
            assert_eq!(
                route_identities[command_index][0].as_deref(),
                route_identities[command_index][1].as_deref(),
                "default and explicit --mir command must share one exact route receipt"
            );
        }
        std::fs::remove_dir_all(&temp).expect("remove isolated Actor output directory");
    }
}
