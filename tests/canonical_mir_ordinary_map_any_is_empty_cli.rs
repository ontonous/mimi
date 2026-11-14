use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{self, materialize_canonical_mir_route};
use mimi::core::resolved::NodeId;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "map-any-owner-v2-is-empty-v1";
const EXPECTED_STDOUT: &[u8] = b"true\nfalse\n";
const RECEIPT_DIGEST_FIELDS: &[&str] = &[
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
                .map(|directory| directory.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide mimi for the ordinary Map/Any IsEmpty CLI evidence")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_v2_is_empty.mimi")
}

fn map_set_drop_control_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_v2_map_set_drop_control.mimi")
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "mimi-map-any-v2-is-empty-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap_or_else(|error| {
            panic!(
                "create isolated Map/Any IsEmpty CLI directory {}: {error}",
                directory.display()
            )
        });
        Self(directory)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed with {:?}:\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn remove_mimi_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("MIMI_") {
            command.env_remove(key);
        }
    }
    command.env_remove("LD_PRELOAD");
}

fn cli(mode: &str, explicit_mir: bool, source: &Path, binary: Option<&Path>) -> Output {
    let mut command = Command::new(mimi_bin());
    remove_mimi_environment(&mut command);
    command
        .arg(mode)
        .arg(source)
        .env("MIMI_ROUTE_CENSUS", "1")
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if explicit_mir {
        command.arg("--mir");
    }
    if let Some(binary) = binary {
        command.arg("--output").arg(binary);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("invoke mimi {mode}: {error}"))
}

fn parse_key_values<'a>(fields: impl Iterator<Item = &'a str>) -> BTreeMap<String, String> {
    fields
        .filter_map(|field| field.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

fn assert_digest(value: &str, label: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} is not a lowercase SHA-256-shaped digest: {value}"
    );
}

fn route_identity(
    output: &Output,
    consumer: &str,
    explicit_mir: bool,
    expected_profile: &str,
) -> (String, String, String) {
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{consumer} selected a legacy fallback:\n{stderr}"
    );

    let route_lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    let [route_line] = route_lines.as_slice() else {
        panic!(
            "{consumer} must emit exactly one route-admission record; got {}:\n{stderr}",
            route_lines.len()
        );
    };
    let fields = parse_key_values(route_line.split_whitespace().skip(2));
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some(consumer),
        "{consumer} reported a different route consumer: {route_line}"
    );
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{consumer} reported the wrong route mode: {route_line}"
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(expected_profile),
        "{consumer} did not report the requested narrow Map/Any route: {route_line}"
    );
    let mir_digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{consumer} omitted mir_digest: {route_line}"));
    let receipt_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{consumer} omitted receipt_identity_digest: {route_line}"));
    assert_digest(mir_digest, "canonical MIR digest");
    assert_digest(receipt_digest, "semantic route receipt digest");

    if !explicit_mir {
        let expected = format!(
            "canonical route disposition: canonical ({expected_profile}) mir_digest={mir_digest}"
        );
        assert_eq!(
            stderr
                .lines()
                .filter(|line| *line == expected.as_str())
                .count(),
            1,
            "default {consumer} must report its dedicated canonical disposition:\n{stderr}"
        );
    } else if consumer == "run" {
        let expected = format!(
            "canonical route disposition: explicit canonical ({expected_profile}) mir_digest={mir_digest}"
        );
        assert_eq!(
            stderr
                .lines()
                .filter(|line| *line == expected.as_str())
                .count(),
            1,
            "explicit MIR run must report its dedicated canonical disposition:\n{stderr}"
        );
    }
    if consumer == "build" {
        let native_build = stderr
            .lines()
            .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
            .collect::<Vec<_>>();
        let [native_build] = native_build.as_slice() else {
            panic!("{consumer} build omitted its canonical native receipt:\n{stderr}");
        };
        let native_fields = parse_key_values(native_build.split_whitespace().skip(2));
        assert_eq!(
            native_fields.get("consumer").map(String::as_str),
            Some(if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            })
        );
        assert_eq!(
            native_fields.get("route_id").map(String::as_str),
            Some(expected_profile),
            "{consumer} build must retain the dedicated route id: {native_build}"
        );
        assert_eq!(
            native_fields.get("profile").map(String::as_str),
            Some(expected_profile),
            "{consumer} build must retain the dedicated receipt profile: {native_build}"
        );
        assert_eq!(
            native_fields.get("mir_digest"),
            Some(mir_digest),
            "{consumer} build native receipt must match the route census digest"
        );
        assert_eq!(
            native_fields.get("route_identity_digest"),
            Some(receipt_digest),
            "{consumer} build native receipt must match the route identity"
        );
    }

    (
        fields["checker_profile"].clone(),
        mir_digest.clone(),
        receipt_digest.clone(),
    )
}

fn assert_native_census(stderr: &str, expected_digest: &str, label: &str) {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    let [line] = lines.as_slice() else {
        panic!("{label} must report one native MIR census:\n{stderr}");
    };
    let (counts, digest) = line
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("native MIR census prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("native MIR census records zero fallbacks and a digest");
    let (emitted, total) = counts.split_once('/').expect("emitted/total counts");
    let emitted = emitted.parse::<usize>().expect("numeric emitted count");
    let total = total.parse::<usize>().expect("numeric total count");
    assert!(total > 0, "{label} compiled an empty MIR graph");
    assert_eq!(emitted, total, "{label} omitted a MIR function");
    assert_eq!(digest, expected_digest, "{label} native digest differs");
}

#[test]
fn ordinary_map_any_v2_is_empty_has_one_receipt_across_all_cli_consumers() {
    let source = include_str!("fixtures/mir_ordinary_map_any_v2_is_empty.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("IsEmpty fixture tokenizes");
    let file = mimi::parser::Parser::new(tokens)
        .parse_file()
        .expect("IsEmpty fixture parses");
    let checked = mimi::core::check_program(&file).expect("IsEmpty fixture passes Checker");
    let route = materialize_canonical_mir_route(&checked, None)
        .expect("the dedicated Map/Any IsEmpty profile materializes");
    assert!(
        route.materialized_ordinary_map_owned_any_v2_is_empty_candidate,
        "IsEmpty must materialize its dedicated candidate"
    );
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "the narrow IsEmpty route must not claim broad owner-v2 materialization"
    );
    const {
        assert!(
            !mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow IsEmpty route must not mark global owner-v2 production readiness"
        );
    };
    let receipt = route
        .ordinary_map_owned_any_v2_is_empty_route_receipt
        .as_ref()
        .expect("dedicated Checker-bound IsEmpty route receipt");
    assert_eq!(receipt.profile, ROUTE_PROFILE);
    assert_eq!(
        receipt,
        &route.program.route_receipt(ROUTE_PROFILE),
        "the dedicated receipt must identify the exact materialized MIR graph"
    );
    assert_digest(&receipt.mir_digest, "materialized MIR digest");
    assert_digest(
        &receipt.route_identity_digest(),
        "materialized route receipt digest",
    );
    for field in RECEIPT_DIGEST_FIELDS {
        let digest = match *field {
            "mir_digest" => &receipt.mir_digest,
            "type_desc_digest" => &receipt.type_desc_digest,
            "abi_digest" => &receipt.abi_digest,
            "ffi_digest" => &receipt.ffi_digest,
            "ownership_digest" => &receipt.ownership_digest,
            "flow_transition_digest" => &receipt.flow_transition_digest,
            _ => unreachable!(),
        };
        assert_digest(digest, field);
    }
    let reference = MirReferenceInterpreter::new(&route.program)
        .with_route_receipt(receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("the route-receipted reference MIR executes");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output.as_bytes(), EXPECTED_STDOUT);

    let evidence = TemporaryDirectory::new();
    let mut identities = BTreeMap::new();
    let mut first_identity: Option<(String, String, String)> = None;
    for explicit_mir in [false, true] {
        for consumer in ["run", "build", "verify"] {
            let label = format!(
                "{consumer}-{}",
                if explicit_mir { "mir" } else { "default" }
            );
            let binary = evidence.path().join(format!("{label}.native"));
            let output = cli(
                consumer,
                explicit_mir,
                &fixture(),
                (consumer == "build").then_some(binary.as_path()),
            );
            assert_success(&output, &label);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{label} must exit successfully"
            );
            let identity = route_identity(&output, consumer, explicit_mir, ROUTE_PROFILE);
            if let Some(expected) = &first_identity {
                assert_eq!(
                    &identity, expected,
                    "{label} must use the same CLI-materialized MIR and semantic receipt as every consumer"
                );
            } else {
                first_identity = Some(identity.clone());
            }
            if consumer == "run" {
                assert_eq!(
                    output.stdout.as_slice(),
                    EXPECTED_STDOUT,
                    "{label} must print both IsEmpty results in order"
                );
            } else if consumer == "verify" {
                assert!(
                    text(&output.stdout).contains("No contracts to verify"),
                    "{label} did not complete the contract-verification consumer:\n{}",
                    text(&output.stdout)
                );
            } else {
                assert!(binary.is_file(), "{label} did not emit a native artifact");
                assert_native_census(&text(&output.stderr), &identity.1, &label);
                let mut native_command = Command::new(&binary);
                remove_mimi_environment(&mut native_command);
                let native = native_command
                    .current_dir(evidence.path())
                    .output()
                    .unwrap_or_else(|error| panic!("execute {label} native artifact: {error}"));
                assert_eq!(
                    native.status.code(),
                    Some(0),
                    "{label} native program returned the wrong status: {}",
                    text(&native.stderr)
                );
                assert_eq!(
                    native.stdout.as_slice(),
                    EXPECTED_STDOUT,
                    "{label} native output differs from the bytecode run"
                );
                assert!(
                    native.stderr.is_empty(),
                    "{label} native program emitted diagnostics: {}",
                    text(&native.stderr)
                );
            }
            identities.insert(label, identity);
        }
    }
    assert_eq!(identities.len(), 6);
}

#[test]
fn ordinary_map_owned_any_v2_map_set_drop_control_reaches_all_cli_consumers() {
    const CONTROL_PROFILE: &str = "map-any-owner-v2-map-set-drop-control-v1";
    const CONTROL_STDOUT: &[u8] = b"";

    let source_path = map_set_drop_control_fixture();
    let source = include_str!("fixtures/mir_ordinary_map_any_v2_map_set_drop_control.mimi");
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .expect("MapSetDropControl fixture tokenizes");
    let mut file = mimi::loader::parser_for_path(tokens, &source_path)
        .expect("construct the same path-registered parser as the CLI")
        .parse_file()
        .expect("MapSetDropControl fixture parses");
    mimi::loader::merge_prelude_into(&mut file);
    let checked = mimi::core::check_program(&file).expect("MapSetDropControl passes Checker");
    let route = materialize_canonical_mir_route(&checked, None)
        .expect("the dedicated no-query MapSetDropControl profile materializes");
    assert!(route.materialized_ordinary_map_owned_any_v2_map_set_drop_control_candidate);
    assert!(!route.materialized_ordinary_map_owned_any_v2_candidate);
    const {
        assert!(!mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
    let receipt = route
        .ordinary_map_owned_any_v2_map_set_drop_control_route_receipt
        .as_ref()
        .expect("dedicated Checker-bound MapSetDropControl receipt");
    assert_eq!(receipt.profile, CONTROL_PROFILE);
    assert_eq!(receipt, &route.program.route_receipt(CONTROL_PROFILE));

    let target_source = include_str!("fixtures/mir_ordinary_map_any_v2_is_empty.mimi");
    let target_tokens = mimi::lexer::Lexer::new(target_source)
        .tokenize()
        .expect("IsEmpty fixture tokenizes");
    let target_path = fixture();
    let mut target_file = mimi::loader::parser_for_path(target_tokens, &target_path)
        .expect("construct the IsEmpty path-registered parser")
        .parse_file()
        .expect("IsEmpty fixture parses");
    mimi::loader::merge_prelude_into(&mut target_file);
    let target_checked =
        mimi::core::check_program(&target_file).expect("IsEmpty fixture passes Checker");
    let target_route = materialize_canonical_mir_route(&target_checked, None)
        .expect("the dedicated IsEmpty profile materializes");
    let control_selection = route
        .program
        .ordinary_map_owned_any_v2_payload_selection_receipt()
        .expect("MapSetDropControl payload selection receipt derives")
        .expect("MapSetDropControl selects its Set payload");
    let target_selection = target_route
        .program
        .ordinary_map_owned_any_v2_payload_selection_receipt()
        .expect("IsEmpty payload selection receipt derives")
        .expect("IsEmpty selects its Set payload");
    let mut control_desc = route
        .program
        .type_catalog()
        .get(&control_selection.selected_types[0].type_id)
        .expect("MapSetDropControl payload TypeDesc")
        .clone();
    let mut target_desc = target_route
        .program
        .type_catalog()
        .get(&target_selection.selected_types[0].type_id)
        .expect("IsEmpty payload TypeDesc")
        .clone();
    let normalize_source_field_ids = |desc: &mut mimi::core::mir::types::MirTypeDesc| {
        if let mimi::core::mir::types::MirLayout::Record { fields, .. } = &mut desc.layout {
            for (index, field) in fields.iter_mut().enumerate() {
                field.id = NodeId(format!("field:{index}"));
            }
        }
        if let Some(native_abi) = &mut desc.native_record_abi {
            for (index, field) in native_abi.fields.iter_mut().enumerate() {
                field.id = NodeId(format!("field:{index}"));
            }
        }
    };
    normalize_source_field_ids(&mut control_desc);
    normalize_source_field_ids(&mut target_desc);
    assert_eq!(
        control_selection.selected_types[0].type_id, target_selection.selected_types[0].type_id,
        "IsEmpty and MapSetDropControl must select the same nominal payload TypeId"
    );
    assert_eq!(
        control_desc, target_desc,
        "IsEmpty and MapSetDropControl must agree on the complete payload TypeDesc after removing only source-specific field NodeIds"
    );
    assert_ne!(
        receipt.mir_digest,
        target_route
            .ordinary_map_owned_any_v2_is_empty_route_receipt
            .as_ref()
            .expect("dedicated IsEmpty route receipt")
            .mir_digest,
        "the no-query control must have a distinct MIR graph"
    );

    let reference = MirReferenceInterpreter::new(&route.program)
        .with_route_receipt(receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .expect("MapSetDropControl reference MIR executes");
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output.as_bytes(), CONTROL_STDOUT);

    let evidence = TemporaryDirectory::new();
    let mut identities = BTreeMap::new();
    let mut first_identity: Option<(String, String, String)> = None;
    for explicit_mir in [false, true] {
        for consumer in ["run", "build", "verify"] {
            let label = format!(
                "{consumer}-{}",
                if explicit_mir { "mir" } else { "default" }
            );
            let binary = evidence.path().join(format!("{label}.native"));
            let output = cli(
                consumer,
                explicit_mir,
                &source_path,
                (consumer == "build").then_some(binary.as_path()),
            );
            assert_success(&output, &label);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{label} must exit successfully"
            );
            let identity = route_identity(&output, consumer, explicit_mir, CONTROL_PROFILE);
            assert_eq!(identity.1, receipt.mir_digest);
            assert_eq!(identity.2, receipt.route_identity_digest());
            if let Some(expected) = &first_identity {
                assert_eq!(
                    &identity, expected,
                    "{label} must use the same MapSetDropControl MIR and receipt as every consumer"
                );
            } else {
                first_identity = Some(identity.clone());
            }
            if consumer == "run" {
                assert_eq!(output.stdout.as_slice(), CONTROL_STDOUT, "{label} output");
            } else if consumer == "verify" {
                assert!(
                    text(&output.stdout).contains("No contracts to verify"),
                    "{label} did not complete the verifier consumer:\n{}",
                    text(&output.stdout)
                );
            } else {
                assert!(binary.is_file(), "{label} did not emit a native artifact");
                assert_native_census(&text(&output.stderr), &identity.1, &label);
                let mut native_command = Command::new(&binary);
                remove_mimi_environment(&mut native_command);
                let native = native_command
                    .current_dir(evidence.path())
                    .output()
                    .unwrap_or_else(|error| panic!("execute {label} native artifact: {error}"));
                assert_eq!(native.status.code(), Some(0), "{label} native exit");
                assert_eq!(
                    native.stdout.as_slice(),
                    CONTROL_STDOUT,
                    "{label} native output"
                );
                assert!(
                    native.stderr.is_empty(),
                    "{label} native stderr: {}",
                    text(&native.stderr)
                );
            }
            identities.insert(label, identity);
        }
    }
    assert_eq!(identities.len(), 6);
}
