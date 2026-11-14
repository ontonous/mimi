use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{self, OrdinaryMapOwnedAnyV2Admission};
use mimi::core::resolved::CheckedProgram;
use mimi::core::resolved::NodeId;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

const SOURCE: &str = include_str!("fixtures/mir_map_empty_root_root_queries_v1.mimi");
const PROFILE: &str = mir::MAP_EMPTY_ROOT_ROOT_QUERIES_V1_PROFILE;

fn replace_once(label: &str, from: &str, to: &str) -> String {
    let matches = SOURCE.match_indices(from).count();
    assert_eq!(
        matches, 1,
        "{label} mutation anchor must occur exactly once"
    );
    SOURCE.replacen(from, to, 1)
}

fn replace_once_in(source: &str, label: &str, from: &str, to: &str) -> String {
    let matches = source.match_indices(from).count();
    assert_eq!(
        matches, 1,
        "{label} mutation anchor must occur exactly once"
    );
    source.replacen(from, to, 1)
}

fn check_source(source: &str) -> CheckedProgram {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("source lexes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("source parses");
    mimi::core::check_program(&file).expect("source checks")
}

fn near_misses() -> Vec<(&'static str, String)> {
    vec![
        ("set", replace_once("set", "let pair", "let extra = map_set(root, \"key\", 9)\n    drop(extra)\n    let pair")),
        ("second-get", replace_once("second-get", "let snapshot", "let extra = map_get(root, \"other\")\n    drop(extra)\n    let snapshot")),
        ("second-root", replace_once("second-root", "let pair", "let extra = map_new()\n    drop(extra)\n    let pair")),
        (
            "nonempty-values",
            {
                let source = replace_once(
                    "nonempty-values",
                    "let root = map_new()",
                    "let initial = map_new()\n    let root = map_set(initial, \"present\", 9)\n    drop(initial)",
                );
                let source = replace_once_in(&source, "nonempty-values", "    drop(count)\n", "");
                replace_once_in(&source, "nonempty-values", "    0\n}", "    count\n}")
            },
        ),
        ("extra-operation", replace_once("extra-operation", "let pair", "let extra = map_size(root)\n    drop(extra)\n    let pair")),
        ("println", replace_once("println", "drop(pair)", "println(99)\n    drop(pair)")),
        ("observable-call", replace_once("observable-call", "drop(pair)", "print(99)\n    drop(pair)")),
        ("wrong-len-source", replace_once("wrong-len-source", "let count = snapshot.len()", "let count = [1, 2].len()")),
        ("missing-pair-drop", replace_once("missing-pair-drop", "    drop(pair)\n", "")),
        ("missing-len-drop", replace_once("missing-len-drop", "    drop(count)\n", "")),
        ("missing-snapshot-drop", replace_once("missing-snapshot-drop", "    drop(snapshot)\n", "")),
        ("missing-root-drop", replace_once("missing-root-drop", "    drop(root)\n", "")),
        (
            "dormant-extern",
            format!("extern \"C\" {{ func unused_external(x: i32) -> i32; }}\n{SOURCE}"),
        ),
        (
            "dormant-actor",
            format!("{SOURCE}\nactor Dormant {{ value: i32 = 0 }}\n"),
        ),
        (
            "dormant-flow",
            format!(
                "{SOURCE}\nflow Dormant {{ state Ready {{ value: i32 }} transition tick(Ready) -> Ready {{ return Ready {{ value: self.value + 1 }} }} }}\n"
            ),
        ),
        (
            "dormant-constant",
            format!("const PROFILE_DECOY: i32 = 1\n{SOURCE}"),
        ),
        (
            "dormant-type",
            format!("type PROFILE_DECOY = i32\n{SOURCE}"),
        ),
        (
            "explicit-import",
            format!("use std::io\n{SOURCE}"),
        ),
        (
            "public-main",
            replace_once("public-main", "func main()", "pub func main()"),
        ),
    ]
}

#[test]
fn empty_root_v1_checker_reference_bytecode_native_verifier_receipt() {
    const {
        assert!(!mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let checked = check_source(SOURCE);
    let raw_program = mir::reference::MirProgram::from_checked_program(&checked)
        .expect("exact source materializes into raw canonical MIR");
    assert!(
        raw_program
            .proof_source_census()
            .admits_map_empty_root_root_queries_v1_source(),
        "exact source census must admit the valid profile:\n{}",
        raw_program.proof_source_census().canonical_text()
    );
    mir::validate_map_empty_root_root_queries_v1_mir_route(&raw_program)
        .expect("raw MIR source census and instruction proof");
    let admission = mir::classify_canonical_mir_route_admission(&checked);
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1
    );
    assert!(!admission.ordinary_map_owned_any_v2.is_complete());
    let route =
        mir::materialize_canonical_mir_route(&checked, None).expect("exact profile materializes");
    assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);
    let receipt = route
        .map_empty_root_root_queries_v1_route_receipt
        .as_ref()
        .expect("dedicated receipt");
    assert_eq!(receipt.profile, PROFILE);
    assert_eq!(*receipt, route.program.route_receipt(PROFILE));
    assert_eq!(
        *receipt,
        receipt
            .manifest_round_trip()
            .expect("sealed manifest replays")
    );
    mir::validate_map_empty_root_root_queries_v1_route(&checked, &route.program)
        .expect("source/MIR proof");
    let observation = MirReferenceInterpreter::new(&route.program)
        .with_route_receipt(receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("reference executes");
    assert_eq!(observation.value, MirRuntimeValue::Int(0));
    assert_eq!(observation.output, "");
    let bytecode =
        mimi::interp::bytecode::compile_mir_program_with_empty_root_root_queries_v1_route(
            &checked,
            &route.program,
        )
        .expect("private VM anchor");
    let value = mimi::interp::bytecode::BytecodeVM::new(bytecode.clone())
        .run()
        .expect("production VM executes");
    assert_eq!(value, 0);
    let mut changed = (*bytecode).clone();
    changed.functions[changed.entry as usize]
        .name
        .push_str("-tampered");
    assert!(
        mimi::interp::bytecode::BytecodeVM::new(Arc::new(changed))
            .run()
            .is_err(),
        "full bytecode-table tampering fails before execution"
    );
    assert!(
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&route.program, receipt)
            .is_err(),
        "raw receipt cannot mint private production VM anchor"
    );
    let broad = route
        .program
        .route_receipt(mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
    let broad_code =
        mimi::interp::bytecode::compile_mir_program_with_route_receipt(&route.program, &broad)
            .expect("diagnostic graph remains inspectable");
    assert!(
        mimi::interp::bytecode::BytecodeVM::new(broad_code)
            .run()
            .is_err(),
        "broad V2 never becomes production-ready"
    );
    let context = inkwell::context::Context::create();
    let mut raw = mimi::codegen::CodeGenerator::new(&context, "empty-root-raw");
    assert!(
        raw.compile_mir_native_with_route_receipt(&route.program, receipt)
            .is_err(),
        "raw native receipt cannot authorize source profile"
    );
    let mut native = mimi::codegen::CodeGenerator::new(&context, "empty-root-exact");
    native
        .compile_checked_mir_native_with_empty_root_root_queries_v1_route(
            &checked,
            &route.program,
            receipt,
        )
        .expect("native exact entry emits");
    let mut public_native = mimi::codegen::CodeGenerator::new(&context, "empty-root-public");
    public_native
        .compile_checked(&checked)
        .expect("public native API consumes same exact route");
    mimi::verifier::validate_mir_capabilities(&route.program)
        .expect("independent profile capability gate");
    let raw_error =
        mimi::verifier::verify_mir_with_route_receipt(&route.program, receipt, String::new())
            .expect_err("raw MIR has no Checker authority for a local Values operation");
    assert!(
        raw_error.contains("exact Checker-aware verifier route"),
        "raw Values verification stays fail-closed: {raw_error}"
    );
    let raw_auto_error = mimi::verifier::verify_mir(&route.program, String::new())
        .expect_err("raw MIR route selection has no Checker authority for local Values");
    assert!(
        raw_auto_error.contains("exact Checker-aware verifier route"),
        "the public raw verifier stays fail-closed too: {raw_auto_error}"
    );
    let results = mimi::verifier::verify_mir_with_map_empty_root_root_queries_v1_route(
        &checked,
        &route.program,
        receipt,
        String::new(),
    )
    .expect("verifier consumes the exact Checker/MIR EmptyRoot route");
    assert!(mimi::verifier::canonical_execution_route_verifier_ready(
        &results, false, false
    ));
    for result in &results {
        if let Some(artifact) = &result.artifact {
            assert_eq!(artifact.mir_route_receipt.as_ref(), Some(receipt));
        }
    }
    let wrong_profile = route
        .program
        .route_receipt(mir::ORDINARY_MAP_OWNED_ANY_V2_ISLAND);
    assert!(
        mimi::verifier::verify_mir_with_map_empty_root_root_queries_v1_route(
            &checked,
            &route.program,
            &wrong_profile,
            String::new(),
        )
        .is_err(),
        "the dedicated verifier rejects a broad owner-v2 receipt"
    );
    mimi::verifier::verify_checked(&checked, String::new()).expect("public checked verifier");
    mimi::verifier::verify_checked_dual(&checked, String::new()).expect("public dual verifier");
    let foreign = check_source(&SOURCE.replace("missing", "different"));
    assert!(
        mir::validate_map_empty_root_root_queries_v1_route(&foreign, &route.program).is_err(),
        "foreign Checker cannot authorize the same instruction shape"
    );
    assert!(
        mimi::verifier::verify_mir_with_map_empty_root_root_queries_v1_route(
            &foreign,
            &route.program,
            receipt,
            String::new(),
        )
        .is_err(),
        "the dedicated verifier rejects a foreign CheckedProgram"
    );
}

#[test]
fn empty_root_v1_source_near_misses_have_no_profile_authority() {
    for (label, source) in near_misses() {
        let checked = check_source(&source);
        let admission = mir::classify_canonical_mir_route_admission(&checked);
        assert_ne!(
            admission.ordinary_map_owned_any_v2,
            OrdinaryMapOwnedAnyV2Admission::MapEmptyRootRootQueriesV1,
            "{label}"
        );
        if label == "nonempty-values" {
            assert_eq!(
                admission.ordinary_map_owned_any_v2,
                OrdinaryMapOwnedAnyV2Admission::OutsideProfile,
                "populated primitive Map is outside the EmptyRoot/AnyOwner-v2 profile"
            );
            assert!(admission.ordinary_map_owned_get_i32_string_v1_complete());
            let route = mir::materialize_canonical_mir_route(&checked, None)
                .expect("the independent owned-get-v1 route remains available");
            assert!(route.materialized_ordinary_map_owned_get_i32_string_v1_candidate);
            assert!(route.map_empty_root_root_queries_v1_route_receipt.is_none());
            assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);
        } else {
            assert!(
                mir::materialize_canonical_mir_route(&checked, None).is_err(),
                "{label} cannot materialize a canonical route"
            );
        }
        let raw_mir = mir::reference::MirProgram::from_checked_program(&checked);
        if matches!(label, "dormant-constant" | "dormant-type") {
            let mir = raw_mir
                .as_ref()
                .expect("dormant declarations still produce a canonical raw MIR graph");
            assert!(
                mir::validate_map_empty_root_root_queries_v1_mir_route(mir).is_err(),
                "{label} raw MIR cannot claim the closed-world EmptyRoot profile"
            );
        }
        if let Ok(mir) = raw_mir {
            let fresh = mir.route_receipt(PROFILE);
            assert!(
                fresh.validate_against_program(&mir).is_err(),
                "fresh generic digest must not forge {label} source authority"
            );
            assert!(
                mimi::verifier::verify_mir_with_route_receipt(&mir, &fresh, String::new()).is_err(),
                "{label} verifier fails closed"
            );
        }
    }
}

fn evidence(label: &str) -> PathBuf {
    let root = std::env::var_os("MIMI_EMPTY_ROOT_V1_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "mimi-empty-root-v1-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
        });
    std::fs::create_dir_all(&root).expect("evidence parent");
    let path = root.join(label);
    std::fs::create_dir(&path).expect("unique test evidence");
    path
}

fn record_command(dir: &Path, label: &str, command: &mut Command) -> Output {
    let argv = format!(
        "program={:?}\nargs={:?}\ncwd={:?}\nenv={:?}\n",
        command.get_program(),
        command.get_args().collect::<Vec<_>>(),
        command.get_current_dir(),
        command.get_envs().collect::<Vec<_>>()
    );
    std::fs::write(dir.join(format!("{label}.argv-env.txt")), argv).unwrap();
    let output = command.output().expect("command starts");
    std::fs::write(dir.join(format!("{label}.stdout")), &output.stdout).unwrap();
    std::fs::write(dir.join(format!("{label}.stderr")), &output.stderr).unwrap();
    std::fs::write(
        dir.join(format!("{label}.exit")),
        format!("{:?}\n", output.status.code()),
    )
    .unwrap();
    output
}

fn cli(
    dir: &Path,
    label: &str,
    mode: &str,
    source: &Path,
    explicit: bool,
    output: Option<&Path>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mimi"));
    command
        .arg(mode)
        .arg(source)
        .env("MIMI_ROUTE_CENSUS", "1")
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if explicit {
        command.arg("--mir");
    }
    if let Some(output) = output {
        command.arg("-o").arg(output);
    }
    record_command(dir, label, &mut command)
}

fn route_identity_for_profile(
    output: &Output,
    consumer: &str,
    explicit: bool,
    expected_profile: &str,
) -> (String, String, String) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{stderr}"
    );
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    let [line] = lines.as_slice() else {
        panic!(
            "expected one route provenance line, got {}: {stderr}",
            lines.len()
        );
    };
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|p| p.strip_prefix(name))
            .unwrap()
            .to_owned()
    };
    assert_eq!(field("consumer="), consumer);
    assert_eq!(
        field("mode="),
        if explicit { "explicit-mir" } else { "default" }
    );
    let identity = (
        field("checker_profile="),
        field("mir_digest="),
        field("receipt_identity_digest="),
    );
    assert_eq!(identity.0, expected_profile);
    assert_eq!(identity.1.len(), 64, "canonical MIR digest is present");
    assert_eq!(
        identity.2.len(),
        64,
        "complete semantic route receipt is present"
    );
    identity
}

fn route_identity(output: &Output, consumer: &str, explicit: bool) -> (String, String, String) {
    route_identity_for_profile(output, consumer, explicit, PROFILE)
}

#[test]
fn empty_root_v1_cli_default_and_explicit_share_all_consumer_receipts() {
    let dir = evidence("cli-positive");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mir_map_empty_root_root_queries_v1.mimi");
    let mut expected = None;
    for explicit in [false, true] {
        for mode in ["run", "build", "verify"] {
            let label = format!("{mode}-{}", if explicit { "mir" } else { "default" });
            let binary = dir.join(format!("native-{explicit}"));
            let output = cli(
                &dir,
                &label,
                mode,
                &source,
                explicit,
                (mode == "build").then_some(binary.as_path()),
            );
            assert!(
                output.status.success(),
                "{label}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let identity = route_identity(&output, mode, explicit);
            if let Some(expected) = &expected {
                assert_eq!(&identity, expected, "{label}");
            } else {
                expected = Some(identity);
            }
            if mode == "run" {
                assert!(output.stdout.is_empty(), "profile is silent");
            }
            if mode == "build" {
                let result = record_command(
                    &dir,
                    &format!("execute-{label}"),
                    Command::new(&binary).current_dir(&dir),
                );
                assert_eq!(result.status.code(), Some(0));
                assert!(result.stdout.is_empty());
                assert!(result.stderr.is_empty());
            }
        }
    }
    const {
        assert!(!mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
}

#[test]
fn empty_root_v1_cli_near_misses_fail_closed_without_legacy() {
    let dir = evidence("cli-negative");
    for (label, text) in near_misses() {
        if label == "nonempty-values" {
            // This source is not admitted as EmptyRoot. It is independently
            // covered by the existing typed owned-get-v1 route and has its
            // own all-consumer proof below.
            continue;
        }
        let source = dir.join(format!("{label}.mimi"));
        std::fs::write(&source, text).unwrap();
        for explicit in [false, true] {
            for mode in ["run", "build", "verify"] {
                let name = format!("{label}-{mode}-{explicit}");
                let binary = dir.join(format!("{name}.native"));
                let output = cli(
                    &dir,
                    &name,
                    mode,
                    &source,
                    explicit,
                    (mode == "build").then_some(binary.as_path()),
                );
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(!output.status.success(), "{name} unexpectedly accepted");
                assert!(
                    stderr.contains(mir::MIR_ROUTE_COVERAGE_ERROR_CODE)
                        || stderr.contains("MIR-MATERIALIZATION-001"),
                    "{name} must fail closed at MIR route coverage or materialization: {stderr}"
                );
                assert!(
                    !stderr.contains("canonical route disposition: legacy"),
                    "{name}: {stderr}"
                );
                assert!(
                    !stderr.contains(&format!("checker_profile={PROFILE}")),
                    "{name} obtained exact authority"
                );
                assert!(!binary.exists(), "{name} emitted a rejected artifact");
                assert!(
                    !output.stdout.windows(2).any(|w| w == b"99"),
                    "observable call ran before rejection"
                );
            }
        }
    }
}

#[test]
fn empty_root_v1_populated_values_near_miss_uses_owned_get_v1_route() {
    let dir = evidence("cli-owned-get-v1");
    let (label, text) = near_misses()
        .into_iter()
        .find(|(label, _)| *label == "nonempty-values")
        .expect("populated Map near-miss fixture");
    let source = dir.join(format!("{label}.mimi"));
    std::fs::write(&source, &text).unwrap();
    let checked = check_source(&text);
    let admission = mir::classify_canonical_mir_route_admission(&checked);
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        OrdinaryMapOwnedAnyV2Admission::OutsideProfile
    );
    assert!(admission.ordinary_map_owned_get_i32_string_v1_complete());
    let materialized = mir::materialize_canonical_mir_route(&checked, None)
        .expect("independent typed owned-get route materializes");
    assert!(materialized.materialized_ordinary_map_owned_get_i32_string_v1_candidate);
    assert!(materialized
        .map_empty_root_root_queries_v1_route_receipt
        .is_none());

    let owned_get_receipt = materialized
        .program
        .route_receipt(mir::ORDINARY_MAP_OWNED_GET_I32_STRING_V1_ISLAND);
    let reference = MirReferenceInterpreter::new(&materialized.program)
        .with_route_receipt(&owned_get_receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("owned-get V1 reference executes the populated Values snapshot");
    assert_eq!(reference.value, MirRuntimeValue::Int(1));
    assert!(reference.output.is_empty());
    let bytecode = mimi::interp::bytecode::compile_mir_program_with_route_receipt(
        &materialized.program,
        &owned_get_receipt,
    )
    .expect("owned-get V1 receipt seals AST-free bytecode");
    assert_eq!(
        mimi::interp::bytecode::BytecodeVM::new(bytecode)
            .run()
            .expect("owned-get V1 VM executes the populated Values snapshot"),
        1
    );

    let mut expected: Option<(String, String, String)> = None;
    for explicit in [false, true] {
        for mode in ["run", "build", "verify"] {
            let name = format!("{mode}-{}", if explicit { "mir" } else { "default" });
            let binary = dir.join(format!("native-{explicit}"));
            let output = cli(
                &dir,
                &name,
                mode,
                &source,
                explicit,
                (mode == "build").then_some(binary.as_path()),
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            let expected_status = if mode == "run" { Some(1) } else { Some(0) };
            assert_eq!(
                output.status.code(),
                expected_status,
                "{name} should preserve the program's populated Values length result: {stderr}"
            );
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{name} must use the separately admitted canonical owner route: {stderr}"
            );
            assert!(!stderr.contains(&format!("checker_profile={PROFILE}")));
            let identity = route_identity_for_profile(
                &output,
                mode,
                explicit,
                &format!("{mode}-canonical-v1"),
            );
            if let Some(expected) = &expected {
                assert_eq!(identity.1, expected.1, "{name} MIR digest");
                assert_eq!(identity.2, expected.2, "{name} semantic receipt digest");
            } else {
                expected = Some(identity);
            }
            if !explicit {
                assert!(
                    stderr.contains(
                        "canonical route disposition: canonical (ordinary-map-owned-get-i32-string-v1)"
                    ),
                    "{name} must identify the independent owned-get-v1 route: {stderr}"
                );
            }
            if mode == "run" {
                assert!(
                    output.stdout.is_empty(),
                    "the fixture has no observable output"
                );
            }
            if mode == "build" {
                assert!(
                    binary.exists(),
                    "{name} emitted the canonical native artifact"
                );
                assert!(stderr.contains("0 fell back"), "{name}: {stderr}");
                let result = record_command(
                    &dir,
                    &format!("execute-{name}"),
                    Command::new(&binary).current_dir(&dir),
                );
                assert_eq!(result.status.code(), Some(1));
                assert!(result.stdout.is_empty());
                assert!(result.stderr.is_empty());
            }
        }
    }
    const {
        assert!(!mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
}

#[test]
#[ignore = "strict serial native Memcheck L3; run by the sealed profile gate"]
fn empty_root_v1_strict_memcheck_l3() {
    let dir = evidence("strict-l3");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mir_map_empty_root_root_queries_v1.mimi");
    for explicit in [false, true] {
        let label = format!("native-{explicit}");
        let binary = dir.join(&label);
        let built = cli(
            &dir,
            &format!("build-{label}"),
            "build",
            &source,
            explicit,
            Some(&binary),
        );
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        route_identity(&built, "build", explicit);
        let report = dir.join(format!("{label}.memcheck.log"));
        let mut command = Command::new("valgrind");
        command
            .env_remove("VALGRIND_OPTS")
            .args([
                "--tool=memcheck",
                "--leak-check=full",
                "--show-leak-kinds=all",
                "--errors-for-leak-kinds=all",
                "--error-exitcode=97",
            ])
            .arg(format!("--log-file={}", report.display()))
            .arg(&binary)
            .current_dir(&dir);
        let result = record_command(&dir, &format!("memcheck-{label}"), &mut command);
        assert_eq!(
            result.status.code(),
            Some(0),
            "{}",
            std::fs::read_to_string(&report).unwrap()
        );
        assert!(result.stdout.is_empty());
        let report = std::fs::read_to_string(report).unwrap();
        assert!(report.contains("ERROR SUMMARY: 0 errors"), "{report}");
        assert!(
            report.contains("All heap blocks were freed -- no leaks are possible")
                || report.contains("in use at exit: 0 bytes in 0 blocks"),
            "strict profile leaves no process-exit owners: {report}"
        );
    }
}
