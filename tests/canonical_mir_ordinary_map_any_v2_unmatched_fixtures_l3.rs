#[path = "support/memcheck_summary.rs"]
mod strict_memcheck_summary;

use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{self, materialize_canonical_mir_route};
use mimi::core::resolved::NodeId;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ARTIFACT_ENV: &str = "MIMI_MAP_V2_UNMATCHED_FIXTURES_L3_ARTIFACT_DIR";
const COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE: &str =
    "ordinary-map-owned-any-v2-computed-string-key-owner-chain-v1";
const DIGEST_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];
const ROUTE_PROFILES: &[mir::CanonicalMirRouteProfile] = &[
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2DirectPrint,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultI32,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2GetOrDefaultString,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CheckedGet,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedKeyGet,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ZeroMissI32,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RemovedKeyCheckedMiss,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesFieldMove,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesLocalIndexI32,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2KeysSnapshot,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesSnapshotReplacement,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshot,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesMixedSnapshotRemove,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesProducerCaller,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCaller,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ValuesCrosscallStringCallerIndex1,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2ComputedStringKeyOwnerChain,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootProducerCaller,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapRootGetAnyCaller,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2RecordUnit,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2BranchLocalSetDiamond,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2CloneIsolation,
    mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2PersistentRootAliasReplacement,
];

struct FixtureCase {
    label: &'static str,
    filename: &'static str,
    expected_stdout: &'static [u8],
    expected_profile: Option<&'static str>,
    require_zero_reachable: bool,
}

const DYNAMIC_STRING_KEYS: FixtureCase = FixtureCase {
    label: "dynamic-string-keys",
    filename: "mir_ordinary_map_any_owner_v2_dynamic_string_keys.mimi",
    expected_stdout: b"true\ntrue\nfalse\ntrue\n",
    expected_profile: Some(COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE),
    require_zero_reachable: true,
};
const RECORD_UNIT: FixtureCase = FixtureCase {
    label: "record-unit",
    filename: "mir_ordinary_map_any_owner_v2_record_unit.mimi",
    expected_stdout: b"owned label\n",
    expected_profile: Some(mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE),
    require_zero_reachable: false,
};
const IS_EMPTY_CONTROL: FixtureCase = FixtureCase {
    label: "is-empty-control",
    filename: "mir_ordinary_map_any_v2_is_empty_control.mimi",
    expected_stdout: b"true\nfalse\n",
    expected_profile: Some(mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE),
    require_zero_reachable: false,
};
const CLONE: FixtureCase = FixtureCase {
    label: "clone",
    filename: "mir_ordinary_map_owned_any_v2_clone.mimi",
    expected_stdout: b"true\ntrue\n",
    expected_profile: Some(
        mir::ORDINARY_MAP_OWNED_ANY_V2_PERSISTENT_ROOT_ALIAS_REPLACEMENT_PROFILE,
    ),
    require_zero_reachable: false,
};

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
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

fn json_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character if character.is_control() => {
                encoded.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}

fn command_json(program: &OsStr, arguments: &[OsString]) -> String {
    let mut values = Vec::with_capacity(arguments.len() + 1);
    values.push(json_string(&program.to_string_lossy()));
    values.extend(
        arguments
            .iter()
            .map(|argument| json_string(&argument.to_string_lossy())),
    );
    format!("[{}]\n", values.join(", "))
}

fn process_status(output: &Output) -> String {
    match output.status.code() {
        Some(code) => format!("exit_code={code}\nsuccess={}\n", output.status.success()),
        None => format!(
            "exit_code=signal\nsuccess={}\nstatus={:?}\n",
            output.status.success(),
            output.status
        ),
    }
}

fn write_new(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap_or_else(|error| panic!("create evidence file {}: {error}", path.display()));
    file.write_all(bytes)
        .unwrap_or_else(|error| panic!("write evidence file {}: {error}", path.display()));
}

fn retain_command_output(
    directory: &Path,
    program: &OsStr,
    arguments: &[OsString],
    output: &Output,
) {
    write_new(
        &directory.join("command.argv.json"),
        command_json(program, arguments).as_bytes(),
    );
    write_new(&directory.join("stdout.bin"), &output.stdout);
    write_new(&directory.join("stderr.bin"), &output.stderr);
    write_new(
        &directory.join("process.status"),
        process_status(output).as_bytes(),
    );
}

fn create_directory(path: &Path) {
    fs::create_dir(path).unwrap_or_else(|error| {
        panic!(
            "create fresh Map/Any fixture evidence directory {}: {error}",
            path.display()
        )
    });
}

struct CaseDirectory {
    root: PathBuf,
    temporary: bool,
}

impl CaseDirectory {
    fn new(label: &str) -> Self {
        let (parent, temporary) = if let Some(configured) =
            std::env::var_os(ARTIFACT_ENV).filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let root = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for Map/Any fixture artifacts")
                    .join(configured)
            };
            if fs::symlink_metadata(&root).is_err() {
                fs::create_dir_all(&root).unwrap_or_else(|error| {
                    panic!(
                        "create Map/Any fixture artifact root {}: {error}",
                        root.display()
                    )
                });
            }
            let metadata = fs::symlink_metadata(&root).unwrap_or_else(|error| {
                panic!(
                    "inspect Map/Any fixture artifact root {}: {error}",
                    root.display()
                )
            });
            assert!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Map/Any fixture artifact root must be a real directory: {}",
                root.display()
            );
            (root, false)
        } else {
            let root = std::env::temp_dir().join(format!(
                "mimi-map-any-v2-unmatched-{}-{label}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap_or_else(|error| {
                panic!(
                    "create isolated Map/Any fixture L3 root {}: {error}",
                    root.display()
                )
            });
            (root, true)
        };
        let root = parent.join(label);
        create_directory(&root);
        let native = root.join("native");
        create_directory(&native);
        Self { root, temporary }
    }

    fn native(&self) -> PathBuf {
        self.root.join("native")
    }
}

impl Drop for CaseDirectory {
    fn drop(&mut self) {
        if self.temporary {
            if let Some(parent) = self.root.parent() {
                let _ = fs::remove_dir_all(parent);
            }
        }
    }
}

fn apply_controlled_runtime_environment(command: &mut Command) {
    command.env_clear();
    for name in [
        "PATH",
        "LD_LIBRARY_PATH",
        "LLVM_SYS_181_PREFIX",
        "MIMI_STDLIB",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed with {:?}; this is a product failure for the fixture\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn assert_digest(value: &str, label: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} is not a lowercase SHA-256 digest: {value}"
    );
}

fn parse_fields(line: &str, label: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for token in line.split_whitespace() {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{label} has malformed receipt token {token:?}"));
        assert!(
            fields.insert(key.to_owned(), value.to_owned()).is_none(),
            "{label} repeats receipt field {key}"
        );
    }
    fields
}

fn selected_profile(
    admission: mir::CanonicalMirRouteAdmission,
) -> Option<mir::CanonicalMirRouteProfile> {
    let mut matches = ROUTE_PROFILES
        .iter()
        .copied()
        .filter(|profile| profile.is_admitted(admission));
    let selected = matches.next()?;
    assert!(
        matches.next().is_none(),
        "Map/Any admission has multiple matching narrow route profiles: {admission:?}"
    );
    Some(selected)
}

fn reference_observation(source_path: &Path, source: &str, label: &str) -> (String, String) {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .unwrap_or_else(|error| panic!("{label} fixture tokenizes: {error:?}"));
    let mut file = mimi::loader::parser_for_path(tokens, source_path)
        .unwrap_or_else(|error| panic!("{label} fixture source path resolves: {error}"))
        .parse_file()
        .unwrap_or_else(|error| panic!("{label} fixture parses: {error:?}"));
    mimi::loader::merge_prelude_into(&mut file);
    let checked = mimi::core::check_program(&file)
        .unwrap_or_else(|error| panic!("{label} fixture passes Checker: {error:?}"));
    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let program = mir::reference::MirProgram::from_checked_program_excluding_sources(
        &checked,
        &excluded_sources,
    )
    .unwrap_or_else(|error| panic!("{label} reference MIR materializes: {error}"));
    let observation = MirReferenceInterpreter::new(&program)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference MIR executes: {error}"));
    assert_eq!(
        observation.value,
        MirRuntimeValue::Int(0),
        "{label} exit value"
    );
    let admission = mir::classify_canonical_mir_route_admission(&checked);
    (observation.output, format!("{admission:?}"))
}

fn canonical_route(
    source_path: &Path,
    source: &str,
    label: &str,
) -> Result<mir::CanonicalMirRouteMaterialization, String> {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .unwrap_or_else(|error| panic!("{label} fixture tokenizes for route receipt: {error:?}"));
    let mut file = mimi::loader::parser_for_path(tokens, source_path)
        .unwrap_or_else(|error| panic!("{label} fixture path resolves for route receipt: {error}"))
        .parse_file()
        .unwrap_or_else(|error| panic!("{label} fixture parses for route receipt: {error:?}"));
    mimi::loader::merge_prelude_into(&mut file);
    let checked = mimi::core::check_program(&file)
        .unwrap_or_else(|error| panic!("{label} fixture passes Checker: {error:?}"));
    let admission = mir::classify_canonical_mir_route_admission(&checked);
    assert!(
        admission.ordinary_map_owned_any_v2.is_candidate(),
        "{label} must remain an ordinary Map/Any v2 route candidate: {admission:?}"
    );
    const {
        assert!(
            !mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "this narrow L3 case must not claim global Map/Any v2 readiness"
        );
    };
    let selected = selected_profile(admission).unwrap_or_else(|| {
        panic!("{label} has no exact canonical profile for route receipt: {admission:?}")
    });
    if let Some(expected_profile) = match label {
        "dynamic-string-keys" => Some(COMPUTED_STRING_KEY_OWNER_CHAIN_PROFILE),
        "record-unit" => Some(mir::ORDINARY_MAP_OWNED_ANY_V2_RECORD_UNIT_PROFILE),
        "is-empty-control" => Some(mir::ORDINARY_MAP_OWNED_ANY_V2_IS_EMPTY_PROFILE),
        _ => None,
    } {
        assert_eq!(selected.as_str(), expected_profile, "{label} route profile");
    }
    materialize_canonical_mir_route(&checked, None).map_err(|error| {
        format!(
            "Checker admitted {} but canonical MIR materialization rejected it: {error}",
            selected.as_str()
        )
    })
}

fn assert_build_receipt(
    stderr: &str,
    explicit_mir: bool,
    label: &str,
    expected_profile: Option<&str>,
) -> (String, String, String) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback: {stderr}"
    );
    let route_lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    let [route_line] = route_lines.as_slice() else {
        panic!("{label} must emit one route-admission receipt: {stderr}");
    };
    let route_fields = parse_fields(
        route_line
            .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
            .expect("route admission prefix"),
        label,
    );
    assert_eq!(
        route_fields.get("consumer").map(String::as_str),
        Some("build")
    );
    assert_eq!(
        route_fields.get("mode").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        })
    );
    let profile = route_fields
        .get("checker_profile")
        .expect("route receipt has checker_profile")
        .clone();
    if let Some(expected_profile) = expected_profile {
        assert_eq!(profile, expected_profile, "{label} Checker route profile");
    }
    let mir_digest = route_fields
        .get("mir_digest")
        .expect("route receipt has MIR digest")
        .clone();
    let route_identity = route_fields
        .get("receipt_identity_digest")
        .expect("route receipt has identity digest")
        .clone();
    assert_digest(&mir_digest, &format!("{label} MIR digest"));
    assert_digest(&route_identity, &format!("{label} route identity digest"));

    let native_lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    let [native_line] = native_lines.as_slice() else {
        panic!("{label} must emit one native build receipt: {stderr}");
    };
    let native_fields = parse_fields(
        native_line
            .strip_prefix("MIMI_ROUTE_CENSUS canonical-native-build ")
            .expect("native build receipt prefix"),
        label,
    );
    assert_eq!(
        native_fields.get("consumer").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        })
    );
    assert_eq!(native_fields.get("route_id"), Some(&profile));
    assert_eq!(native_fields.get("profile"), Some(&profile));
    assert_eq!(native_fields.get("mir_digest"), Some(&mir_digest));
    assert_eq!(
        native_fields.get("route_identity_digest"),
        Some(&route_identity)
    );

    let census = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    let [census] = census.as_slice() else {
        panic!("{label} must emit one complete native MIR census: {stderr}");
    };
    let (counts, census_digest) = census
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("native census prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("native census records no legacy fallback");
    let (emitted, total) = counts.split_once('/').expect("emitted/total counts");
    let emitted = emitted.parse::<usize>().expect("emitted count is numeric");
    let total = total.parse::<usize>().expect("total count is numeric");
    assert!(total > 0, "{label} compiled an empty canonical MIR graph");
    assert_eq!(emitted, total, "{label} omitted a canonical MIR function");
    assert_eq!(census_digest, mir_digest);
    if !explicit_mir {
        let expected =
            format!("canonical route disposition: canonical ({profile}) mir_digest={mir_digest}");
        assert_eq!(
            stderr.lines().filter(|line| *line == expected).count(),
            1,
            "{label} default build lacks its exact route disposition"
        );
    }
    (profile, mir_digest, route_identity)
}

fn assert_verify_receipt(
    stdout: &str,
    stderr: &str,
    explicit_mir: bool,
    label: &str,
) -> ((String, String, String), String) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback: {stderr}"
    );
    assert!(
        !stderr.contains("MIR-COVERAGE-001"),
        "{label} lost dedicated verifier route coverage: {stderr}"
    );
    let combined_output = format!("{stdout}\n{stderr}");
    assert!(
        !combined_output.contains("NITS") && !combined_output.contains("unknown"),
        "{label} must report a successful proof instead of NITS/unknown:\n{combined_output}"
    );

    let route_lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    let [route_line] = route_lines.as_slice() else {
        panic!("{label} must emit one route-admission receipt: {stderr}");
    };
    let route_fields = parse_fields(
        route_line
            .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
            .expect("verify route-admission prefix"),
        label,
    );
    let field_order = route_line
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("verify route-admission prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("route field has key=value").0)
        .collect::<Vec<_>>();
    assert_eq!(
        field_order,
        [
            "consumer",
            "mode",
            "checker_profile",
            "mir_digest",
            "receipt_identity_digest",
        ]
        .to_vec(),
        "{label} verify route-admission identity shape changed"
    );
    assert_eq!(
        route_fields.get("consumer").map(String::as_str),
        Some("verify"),
        "{label} route consumer"
    );
    assert_eq!(
        route_fields.get("mode").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{label} route mode"
    );
    let profile = route_fields
        .get("checker_profile")
        .expect("verify route receipt has checker_profile")
        .clone();
    let mir_digest = route_fields
        .get("mir_digest")
        .expect("verify route receipt has MIR digest")
        .clone();
    let route_identity = route_fields
        .get("receipt_identity_digest")
        .expect("verify route receipt has identity digest")
        .clone();
    assert_digest(&mir_digest, &format!("{label} MIR digest"));
    assert_digest(&route_identity, &format!("{label} route identity digest"));

    let summaries = stdout
        .lines()
        .filter(|line| line.contains(" verified in "))
        .collect::<Vec<_>>();
    let [summary] = summaries.as_slice() else {
        panic!("{label} must print one completed proof summary: {stdout}");
    };
    let (counts, _) = summary
        .split_once(" verified in ")
        .expect("proof summary contains verified count");
    let (verified, total) = counts
        .split_once('/')
        .expect("proof summary has verified/total counts");
    let verified = verified
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{label} verified proof count is numeric: {summary}"));
    let total = total
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{label} total proof count is numeric: {summary}"));
    assert!(
        total > 0,
        "{label} proof summary has no obligations: {summary}"
    );
    assert_eq!(
        verified, total,
        "{label} must prove every obligation: {summary}"
    );

    (
        (profile, mir_digest, route_identity),
        format!("{verified}/{total}"),
    )
}

fn strict_memcheck(
    binary: &Path,
    directory: &Path,
    label: &str,
    expected_stdout: &[u8],
    require_zero_reachable: bool,
) {
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "{label} strict L3 invocation must require Valgrind"
    );
    let valgrind = executable_on_path("valgrind");
    let home = directory.join("home");
    create_directory(&home);
    write_new(
        &directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code=0\nexpected_stdout_bytes={}\n",
            expected_stdout.len()
        )
        .as_bytes(),
    );
    write_new(
        &directory.join("expected-native-stdout.bin"),
        expected_stdout,
    );
    let log_path = directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--command-line-only=yes"),
        OsString::from("--default-suppressions=no"),
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=97"),
        OsString::from(format!("--log-file={}", log_path.display())),
        binary.as_os_str().to_owned(),
    ];
    let mut command = Command::new(&valgrind);
    apply_controlled_runtime_environment(&mut command);
    let output = command
        .args(&arguments)
        .env("HOME", &home)
        .current_dir(&home)
        .output()
        .unwrap_or_else(|error| panic!("{label} strict Memcheck starts: {error}"));
    retain_command_output(directory, valgrind.as_os_str(), &arguments, &output);
    let report = fs::read(&log_path)
        .unwrap_or_else(|error| panic!("{label} strict Memcheck report exists: {error}"));
    assert!(
        !report.is_empty(),
        "{label} strict Memcheck report is empty"
    );
    let report_text = text(&report);
    let summary = strict_memcheck_summary::parse(&report_text);
    if require_zero_reachable {
        assert_eq!(
            (
                summary.still_reachable_bytes,
                summary.still_reachable_blocks
            ),
            (0, 0),
            "{label} strict Memcheck retained reachable allocations: {report_text}"
        );
    }
    write_new(
        &directory.join("memcheck-summary.txt"),
        summary.rendered.as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} strict Memcheck changed the native exit status: {report_text}"
    );
    assert_eq!(
        output.stdout.as_slice(),
        expected_stdout,
        "{label} native stdout differs from the fixture's expected behavior"
    );
    assert!(
        output.stderr.is_empty(),
        "{label} native program emitted diagnostics: {}",
        text(&output.stderr)
    );
}

fn executable_on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("strict Memcheck requires PATH");
    for directory in std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()) {
        let candidate = directory.join(name);
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() {
            return candidate;
        }
    }
    panic!("strict L3 requires executable {name} on PATH");
}

fn run_case(case: FixtureCase) {
    let directory = CaseDirectory::new(case.label);
    let native_root = directory.native();
    let source_path = fixture(case.filename);
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("read exact {} fixture: {error}", case.filename));
    write_new(&native_root.join("source.fixture.mimi"), source.as_bytes());

    let mimi = mimi_bin();
    let bytecode_directory = native_root.join("bytecode-run");
    create_directory(&bytecode_directory);
    let bytecode_arguments = vec![OsString::from("run"), source_path.as_os_str().to_owned()];
    let mut bytecode_command = Command::new(&mimi);
    apply_controlled_runtime_environment(&mut bytecode_command);
    bytecode_command
        .args(&bytecode_arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let bytecode = bytecode_command
        .output()
        .unwrap_or_else(|error| panic!("{} Bytecode VM run starts: {error}", case.label));
    retain_command_output(
        &bytecode_directory,
        mimi.as_os_str(),
        &bytecode_arguments,
        &bytecode,
    );
    assert_success(&bytecode, &format!("{} Bytecode VM run", case.label));
    assert_eq!(
        bytecode.status.code(),
        Some(0),
        "{} Bytecode VM exit code",
        case.label
    );
    assert_eq!(
        bytecode.stdout.as_slice(),
        case.expected_stdout,
        "{} Bytecode VM semantic output",
        case.label
    );

    let (reference_output, admission_text) =
        reference_observation(&source_path, &source, case.label);
    assert_eq!(
        reference_output.as_bytes(),
        case.expected_stdout,
        "{} MIR reference semantic output",
        case.label
    );
    assert_eq!(
        reference_output.as_bytes(),
        bytecode.stdout.as_slice(),
        "{} bytecode and reference runs differ",
        case.label
    );

    let route_result = canonical_route(&source_path, &source, case.label);
    let route_error = route_result.as_ref().err().cloned();
    let route = route_result.ok();
    let route_profile = route.as_ref().and_then(|materialized| {
        selected_profile(materialized.admission).map(|profile| profile.as_str().to_owned())
    });
    if let (Some(expected), Some(observed)) = (case.expected_profile, route_profile.as_deref()) {
        assert_eq!(
            observed, expected,
            "{} expected narrow route profile",
            case.label
        );
    }

    let mut builds = Vec::new();
    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-build"
        } else {
            "default-build"
        };
        let build_directory = native_root.join(mode);
        create_directory(&build_directory);
        let binary = build_directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source_path.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if explicit_mir {
            arguments.push(OsString::from("--mir"));
        }
        let mut command = Command::new(&mimi);
        apply_controlled_runtime_environment(&mut command);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .args(&arguments)
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("{} {mode} starts: {error}", case.label));
        retain_command_output(&build_directory, mimi.as_os_str(), &arguments, &output);
        write_new(
            &build_directory.join("command.environment.txt"),
            b"MIMI_ROUTE_CENSUS=1\nMIMI_environment=controlled-runtime-allowlist\nLD_PRELOAD=unset\n",
        );
        builds.push((explicit_mir, binary, output));
    }

    for (explicit_mir, binary, output) in &builds {
        if binary.is_file() {
            let mode = if *explicit_mir {
                "explicit-memcheck"
            } else {
                "default-memcheck"
            };
            let memcheck_directory = native_root.join(mode);
            create_directory(&memcheck_directory);
            strict_memcheck(
                binary,
                &memcheck_directory,
                &format!("{}/{}", case.label, mode),
                case.expected_stdout,
                case.require_zero_reachable,
            );
        } else {
            write_new(
                &native_root.join(format!("{}-memcheck.unavailable.txt", if *explicit_mir { "explicit" } else { "default" })),
                b"No native binary was produced; strict Memcheck cannot run until canonical native build succeeds.\n",
            );
        }
        assert_success(
            output,
            &format!(
                "{} {} canonical MIR native build",
                case.label,
                if *explicit_mir { "explicit" } else { "default" }
            ),
        );
        assert!(
            binary.is_file(),
            "{} {} canonical build produced no native binary",
            case.label,
            if *explicit_mir { "explicit" } else { "default" }
        );
    }

    let materialized = route.unwrap_or_else(|| {
        panic!(
            "{} product bug: canonical MIR route receipt could not be materialized: {}",
            case.label,
            route_error.unwrap_or_else(|| "no route materialization result".to_owned())
        )
    });
    let admission = materialized.admission;
    let profile = selected_profile(admission)
        .unwrap_or_else(|| panic!("{} has no admitted MIR profile", case.label));
    let profile_name = profile.as_str();
    assert!(
        profile.is_materialized(&materialized),
        "{} route profile {} has no materialization receipt",
        case.label,
        profile_name
    );
    if let Some(expected) = case.expected_profile {
        assert_eq!(profile_name, expected, "{} route profile", case.label);
    }
    let route_receipt = materialized.program.route_receipt(profile_name);
    assert_eq!(route_receipt.profile, profile_name);
    route_receipt
        .validate_against_program(&materialized.program)
        .unwrap_or_else(|error| {
            panic!(
                "{} route receipt binds the materialized MIR graph: {error}",
                case.label
            )
        });
    let mir_digest = route_receipt.mir_digest.clone();
    let route_identity = route_receipt.route_identity_digest();
    assert_digest(&mir_digest, &format!("{} MIR digest", case.label));
    assert_digest(&route_identity, &format!("{} route identity", case.label));
    for (field, digest) in DIGEST_FIELDS.iter().map(|field| {
        let digest = match *field {
            "mir_digest" => &route_receipt.mir_digest,
            "type_desc_digest" => &route_receipt.type_desc_digest,
            "abi_digest" => &route_receipt.abi_digest,
            "ffi_digest" => &route_receipt.ffi_digest,
            "ownership_digest" => &route_receipt.ownership_digest,
            "flow_transition_digest" => &route_receipt.flow_transition_digest,
            _ => unreachable!(),
        };
        (*field, digest)
    }) {
        assert_digest(digest, &format!("{} {field}", case.label));
    }
    let reference = MirReferenceInterpreter::new(&materialized.program)
        .with_route_receipt(&route_receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{} route-receipted MIR executes: {error}", case.label));
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output.as_bytes(), case.expected_stdout);

    let mut build_receipts = Vec::new();
    for (explicit_mir, _, output) in &builds {
        let mode = if *explicit_mir { "explicit" } else { "default" };
        let receipt = assert_build_receipt(
            &text(&output.stderr),
            *explicit_mir,
            &format!("{}/{} build", case.label, mode),
            Some(profile_name),
        );
        assert_eq!(receipt.0, profile_name);
        assert_eq!(receipt.1, mir_digest);
        assert_eq!(receipt.2, route_identity);
        build_receipts.push(receipt);
    }
    assert_eq!(build_receipts.len(), 2);
    assert_eq!(
        build_receipts[0], build_receipts[1],
        "{} default and explicit --mir builds must share one route receipt",
        case.label
    );

    let mut verify_results = Vec::new();
    if case.label == "clone" {
        let expected_route = (
            profile_name.to_owned(),
            mir_digest.clone(),
            route_identity.clone(),
        );
        for explicit_mir in [false, true] {
            let mode = if explicit_mir {
                "explicit-verify"
            } else {
                "default-verify"
            };
            let verify_directory = native_root.join(mode);
            create_directory(&verify_directory);
            let mut arguments = vec![OsString::from("verify"), source_path.as_os_str().to_owned()];
            if explicit_mir {
                arguments.push(OsString::from("--mir"));
            }
            let mut command = Command::new(&mimi);
            apply_controlled_runtime_environment(&mut command);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .args(&arguments)
                .current_dir(env!("CARGO_MANIFEST_DIR"));
            let output = command
                .output()
                .unwrap_or_else(|error| panic!("{} {mode} starts: {error}", case.label));
            retain_command_output(&verify_directory, mimi.as_os_str(), &arguments, &output);
            write_new(
                &verify_directory.join("command.environment.txt"),
                b"MIMI_ROUTE_CENSUS=1\nMIMI_environment=controlled-runtime-allowlist\nLD_PRELOAD=unset\n",
            );
            assert_success(&output, &format!("{} {mode}", case.label));
            let (receipt, proof_summary) = assert_verify_receipt(
                &text(&output.stdout),
                &text(&output.stderr),
                explicit_mir,
                &format!("{}/{}", case.label, mode),
            );
            assert_eq!(
                receipt, expected_route,
                "{} {mode} route receipt must match the materialized canonical MIR route",
                case.label
            );
            verify_results.push((explicit_mir, receipt, proof_summary));
        }
        let [(false, default_route, _default_proof), (true, explicit_route, _explicit_proof)] =
            verify_results.as_slice()
        else {
            panic!("clone strict case must retain default and explicit verify evidence");
        };
        assert_eq!(default_route, explicit_route);
    }

    let mut run_summary = format!(
        "fixture={}\nroute_profile={}\nglobal_map_any_v2_production_ready=false\nmir_digest={}\nroute_identity_digest={}\nchecker_admission={}\nbytecode_run=true\nreference_run=true\nbytecode_reference_equal=true\ndefault_native_build=true\nexplicit_native_build=true\nmemcheck_run_count=2\ndefault_route_receipt=true\nexplicit_route_receipt=true\nlegacy_fallback_count=0\nzero_reachable_required={}\nexpected_stdout={}\n",
        case.filename,
        profile_name,
        mir_digest,
        route_identity,
        admission_text,
        case.require_zero_reachable,
        String::from_utf8_lossy(case.expected_stdout).escape_debug()
    );
    if let [(false, default_route, default_proof), (true, explicit_route, explicit_proof)] =
        verify_results.as_slice()
    {
        run_summary.push_str(&format!(
            "default_verify=true\nexplicit_verify=true\ndefault_verify_route_receipt=true\nexplicit_verify_route_receipt=true\nverify_route_receipts_match_materialized=true\ndefault_verify_profile={}\ndefault_verify_mir_digest={}\ndefault_verify_route_identity_digest={}\ndefault_verify_proof_summary={}/verified\nexplicit_verify_profile={}\nexplicit_verify_mir_digest={}\nexplicit_verify_route_identity_digest={}\nexplicit_verify_proof_summary={}/verified\n",
            default_route.0,
            default_route.1,
            default_route.2,
            default_proof,
            explicit_route.0,
            explicit_route.1,
            explicit_route.2,
            explicit_proof,
        ));
    }
    write_new(&native_root.join("run-summary.txt"), run_summary.as_bytes());
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_any_v2_unmatched_dynamic_string_keys_has_active_semantic_coverage() {
    run_case(DYNAMIC_STRING_KEYS);
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_any_v2_unmatched_record_unit_has_active_semantic_coverage() {
    run_case(RECORD_UNIT);
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_any_v2_unmatched_is_empty_control_has_active_semantic_coverage() {
    run_case(IS_EMPTY_CONTROL);
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_any_v2_unmatched_clone_has_active_semantic_coverage() {
    run_case(CLONE);
}
