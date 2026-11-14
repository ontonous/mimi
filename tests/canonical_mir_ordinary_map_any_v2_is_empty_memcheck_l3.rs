use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{self, materialize_canonical_mir_route};
use mimi::core::resolved::NodeId;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "map-any-owner-v2-is-empty-v1";
const CONTROL_PROFILE: &str = "map-any-owner-v2-map-set-drop-control-v1";
const EXPECTED_STDOUT: &[u8] = b"true\nfalse\n";
const EXPECTED_CONTROL_STDOUT: &[u8] = b"";
const DIGEST_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn remove_mimi_environment(command: &mut Command) {
    let keys = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("MIMI_"))
        .collect::<Vec<_>>();
    for key in keys {
        command.env_remove(key);
    }
    command.env_remove("LD_PRELOAD");
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

fn create_capture_directory(path: &Path) {
    fs::create_dir(path).unwrap_or_else(|error| {
        panic!(
            "create fresh evidence directory {} without replacing prior evidence: {error}",
            path.display()
        )
    });
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

fn assert_success(output: &Output, mode: &str) {
    assert!(
        output.status.success(),
        "{mode} failed with {:?}:\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn parse_key_values<'a>(
    tokens: impl Iterator<Item = &'a str>,
    label: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for token in tokens {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{label} contains a malformed field: {token}"));
        assert!(
            fields.insert(key.to_owned(), value.to_owned()).is_none(),
            "{label} repeats field {key}"
        );
    }
    fields
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

fn executable_on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("Valgrind requires PATH");
    for directory in std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()) {
        let candidate = directory.join(name);
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return fs::canonicalize(&candidate).unwrap_or_else(|error| {
            panic!(
                "canonicalize executable {name} at {}: {error}",
                candidate.display()
            )
        });
    }
    panic!("strict IsEmpty L3 requires executable {name} on PATH")
}

fn retain_sha256(path: &Path, label: &str, evidence_root: &Path) -> String {
    let directory = evidence_root.join(format!("{label}-sha256"));
    create_capture_directory(&directory);
    let canonical = fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()));
    let arguments = vec![OsString::from("--"), canonical.as_os_str().to_owned()];
    let output = Command::new("sha256sum")
        .args(&arguments)
        .output()
        .unwrap_or_else(|error| panic!("hash {}: {error}", canonical.display()));
    retain_command_output(&directory, OsStr::new("sha256sum"), &arguments, &output);
    assert_success(&output, &format!("sha256sum {}", canonical.display()));
    let line = text(&output.stdout);
    let (digest, hashed_path) = line
        .trim_end()
        .split_once("  ")
        .expect("sha256sum output contains digest and path");
    let canonical_path = canonical.to_string_lossy();
    assert_eq!(hashed_path, canonical_path.as_ref());
    assert_digest(digest, &format!("{label} SHA-256"));
    digest.to_owned()
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mimi-map-any-v2-is-empty-l3-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated Map/Any IsEmpty L3 directory {}: {error}",
                path.display()
            )
        });
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct L3Directory {
    native: PathBuf,
    _temporary: Option<TemporaryDirectory>,
}

impl L3Directory {
    fn new() -> Self {
        if let Some(configured) = std::env::var_os("MIMI_MAP_V2_IS_EMPTY_L3_ARTIFACT_DIR")
            .filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let requested = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for IsEmpty L3 artifacts")
                    .join(configured)
            };
            if fs::symlink_metadata(&requested).is_err() {
                fs::create_dir_all(&requested).unwrap_or_else(|error| {
                    panic!(
                        "create IsEmpty L3 artifact root {}: {error}",
                        requested.display()
                    )
                });
            }
            let metadata = fs::symlink_metadata(&requested).unwrap_or_else(|error| {
                panic!("inspect artifact root {}: {error}", requested.display())
            });
            assert!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "IsEmpty L3 artifact root must be a real directory: {}",
                requested.display()
            );
            let root = fs::canonicalize(&requested).unwrap_or_else(|error| {
                panic!("resolve artifact root {}: {error}", requested.display())
            });
            let native = root.join("native");
            create_capture_directory(&native);
            Self {
                native,
                _temporary: None,
            }
        } else {
            let temporary = TemporaryDirectory::new();
            let root = temporary.0.clone();
            let native = root.join("native");
            create_capture_directory(&native);
            Self {
                native,
                _temporary: Some(temporary),
            }
        }
    }
}

struct RouteControl {
    profile: String,
    mir_digest: String,
    receipt_identity_digest: String,
    payload_selection_digest: String,
    payload_type_id: mimi::core::ir::ResolvedTypeId,
    payload_type_desc_digest: String,
    payload_desc: mimi::core::mir::types::MirTypeDesc,
    digest_lines: String,
}

fn payload_desc_without_source_field_ids(
    desc: &mimi::core::mir::types::MirTypeDesc,
) -> mimi::core::mir::types::MirTypeDesc {
    let mut desc = desc.clone();
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
    desc
}

fn route_control(
    source_path: &Path,
    label: &str,
    profile: mir::CanonicalMirRouteProfile,
    expected_stdout: &[u8],
) -> RouteControl {
    let profile_name = profile.as_str();
    let source = fs::read_to_string(source_path)
        .unwrap_or_else(|error| panic!("read {label} fixture {}: {error}", source_path.display()));
    let tokens = mimi::lexer::Lexer::new(&source)
        .tokenize()
        .unwrap_or_else(|error| panic!("{label} fixture tokenizes: {error}"));
    let mut file = mimi::loader::parser_for_path(tokens, source_path)
        .unwrap_or_else(|error| panic!("construct {label} path parser: {error}"))
        .parse_file()
        .unwrap_or_else(|error| panic!("{label} fixture parses: {error}"));
    mimi::loader::merge_prelude_into(&mut file);
    let checked = mimi::core::check_program(&file)
        .unwrap_or_else(|error| panic!("{label} fixture passes Checker: {error:?}"));
    let route = materialize_canonical_mir_route(&checked, None)
        .unwrap_or_else(|error| panic!("dedicated {profile_name} profile materializes: {error}"));
    let receipt = match profile {
        mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty => {
            assert!(
                route.materialized_ordinary_map_owned_any_v2_is_empty_candidate,
                "IsEmpty must materialize its dedicated candidate"
            );
            route
                .ordinary_map_owned_any_v2_is_empty_route_receipt
                .as_ref()
                .expect("dedicated Checker-bound IsEmpty route receipt")
        }
        mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl => {
            assert!(
                route.materialized_ordinary_map_owned_any_v2_map_set_drop_control_candidate,
                "MapSetDropControl must materialize its dedicated candidate"
            );
            route
                .ordinary_map_owned_any_v2_map_set_drop_control_route_receipt
                .as_ref()
                .expect("dedicated Checker-bound MapSetDropControl route receipt")
        }
        _ => panic!("unsupported IsEmpty L3 profile {profile_name}"),
    };
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "narrow L3 route must not claim broad owner-v2 materialization"
    );
    const {
        assert!(
            !mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow L3 route must leave global owner-v2 readiness closed"
        );
    };
    assert_eq!(receipt.profile, profile_name);
    assert_eq!(receipt, &route.program.route_receipt(profile_name));
    match profile {
        mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty => {
            mir::validate_ordinary_map_owned_any_v2_is_empty_route(&checked, &route.program)
                .expect("Checker and MIR IsEmpty receipts agree");
        }
        mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl => {
            mir::validate_ordinary_map_owned_any_v2_map_set_drop_control_route(
                &checked,
                &route.program,
            )
            .expect("Checker and MIR MapSetDropControl receipts agree");
        }
        _ => unreachable!(),
    }
    let selection = route
        .program
        .ordinary_map_owned_any_v2_payload_selection_receipt()
        .expect("payload selection receipt derives")
        .expect("narrow profile has one selected Set payload");
    selection
        .validate_against_program(&route.program)
        .expect("payload selection receipt binds this MIR and TypeCatalog");
    assert_eq!(selection.entries.len(), 1);
    assert_eq!(selection.selected_types.len(), 1);
    let payload_type_id = selection.selected_types[0].type_id.clone();
    let payload_type_desc_digest = selection.selected_types[0].type_desc_digest.clone();
    let payload_desc = route
        .program
        .type_catalog()
        .get(&payload_type_id)
        .expect("selected payload TypeDesc exists")
        .clone();
    let mir_digest = receipt.mir_digest.clone();
    let receipt_identity_digest = receipt.route_identity_digest();
    assert_digest(
        &mir_digest,
        &format!("Checker-materialized {profile_name} MIR digest"),
    );
    assert_digest(
        &receipt_identity_digest,
        &format!("Checker-materialized {profile_name} route receipt digest"),
    );

    let mut digest_lines = format!(
        "profile={profile_name}\nreceipt_identity_digest={receipt_identity_digest}\npayload_selection_digest={}\nselected_type_count={}\n",
        selection.selection_digest,
        selection.selected_types.len()
    );
    for field in DIGEST_FIELDS {
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
        digest_lines.push_str(&format!("{field}={digest}\n"));
    }
    digest_lines.push_str(&format!(
        "selected_type_id={}\nselected_type_desc_digest={payload_type_desc_digest}\n",
        payload_type_id.as_str()
    ));
    digest_lines.push_str("global_map_any_v2_production_ready=false\n");

    let reference = MirReferenceInterpreter::new(&route.program)
        .with_route_receipt(receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{profile_name} reference MIR executes: {error}"));
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output.as_bytes(), expected_stdout);
    digest_lines.push_str(&format!(
        "reference_exit_code=0\nreference_stdout_bytes={}\n",
        reference.output.len()
    ));

    RouteControl {
        profile: profile_name.to_owned(),
        payload_selection_digest: selection.selection_digest.clone(),
        payload_type_id,
        payload_type_desc_digest,
        payload_desc,
        mir_digest,
        receipt_identity_digest,
        digest_lines,
    }
}

fn assert_route_census(
    stderr: &str,
    explicit_mir: bool,
    label: &str,
    expected_profile: &str,
) -> (String, String) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback: {stderr}"
    );
    let route_lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    let [route_line] = route_lines.as_slice() else {
        panic!("{label} must emit one route-admission census: {stderr}");
    };
    let fields = parse_key_values(
        route_line
            .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
            .expect("route census prefix")
            .split_whitespace(),
        label,
    );
    assert_eq!(fields.get("consumer").map(String::as_str), Some("build"));
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        })
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(expected_profile),
        "{label} did not select the requested narrow Map/Any route"
    );
    let mir_digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{label} omitted MIR digest: {route_line}"));
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{label} omitted receipt identity digest: {route_line}"));
    assert_digest(mir_digest, &format!("{label} MIR digest"));
    assert_digest(
        receipt_identity_digest,
        &format!("{label} route receipt identity digest"),
    );

    let native_receipts = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    let [native_receipt] = native_receipts.as_slice() else {
        panic!("{label} must emit one canonical native build receipt: {stderr}");
    };
    let native_fields = parse_key_values(
        native_receipt
            .strip_prefix("MIMI_ROUTE_CENSUS canonical-native-build ")
            .expect("canonical native build prefix")
            .split_whitespace(),
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
    assert_eq!(
        native_fields.get("route_id").map(String::as_str),
        Some(expected_profile)
    );
    assert_eq!(
        native_fields.get("profile").map(String::as_str),
        Some(expected_profile)
    );
    assert_eq!(native_fields.get("mir_digest"), Some(mir_digest));
    assert_eq!(
        native_fields.get("route_identity_digest"),
        Some(receipt_identity_digest),
        "{label} native build receipt differs from route admission identity"
    );

    let native_census = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    let [native_census] = native_census.as_slice() else {
        panic!("{label} must emit exactly one native function census: {stderr}");
    };
    let (counts, digest) = native_census
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("native function census prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("native function census reports no fallbacks and a digest");
    let (emitted, total) = counts
        .split_once('/')
        .expect("native function census reports emitted/total");
    let emitted = emitted.parse::<usize>().expect("emitted count is numeric");
    let total = total.parse::<usize>().expect("total count is numeric");
    assert!(total > 0, "{label} emitted an empty MIR graph");
    assert_eq!(emitted, total, "{label} omitted a MIR function");
    assert_eq!(digest, mir_digest);

    if !explicit_mir {
        let expected_disposition = format!(
            "canonical route disposition: canonical ({expected_profile}) mir_digest={mir_digest}"
        );
        assert_eq!(
            stderr
                .lines()
                .filter(|line| *line == expected_disposition.as_str())
                .count(),
            1,
            "default build lacks its dedicated canonical disposition: {stderr}"
        );
    }
    (mir_digest.clone(), receipt_identity_digest.clone())
}

fn summary_line(report: &str, marker: &str, label: &str) -> String {
    let matching = report
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("==")?;
            let (pid, message) = rest.split_once("==")?;
            message
                .trim_start()
                .starts_with(marker)
                .then(|| format!("=={pid}== {}", message.trim()))
        })
        .collect::<Vec<_>>();
    let [line] = matching.as_slice() else {
        panic!(
            "{label} must contain one {marker:?} summary row; found {}:\n{report}",
            matching.len()
        );
    };
    (*line).to_owned()
}

fn summary_pid(line: &str, label: &str) -> String {
    let pid = line
        .strip_prefix("==")
        .and_then(|line| line.split_once("=="))
        .map(|(pid, _)| pid)
        .unwrap_or_else(|| panic!("{label} summary row lacks a process identity: {line}"));
    assert!(
        !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
        "{label} summary row has a nonnumeric process identity: {line}"
    );
    pid.to_owned()
}

fn parse_grouped_memcheck_count(value: &str, label: &str) -> u64 {
    let groups = value.split(',').collect::<Vec<_>>();
    let valid = match groups.as_slice() {
        [digits] => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        [first, rest @ ..] => {
            (1..=3).contains(&first.len())
                && first.bytes().all(|byte| byte.is_ascii_digit())
                && rest.iter().all(|group| {
                    group.len() == 3 && group.bytes().all(|byte| byte.is_ascii_digit())
                })
        }
        [] => false,
    };
    assert!(
        valid,
        "{label} has a malformed grouped Memcheck count: {value:?}"
    );
    value
        .bytes()
        .filter(|byte| *byte != b',')
        .map(char::from)
        .collect::<String>()
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("{label} Memcheck count overflows u64: {error}"))
}

fn all_free_report_summary(report: &str, label: &str) -> Option<(String, String, String)> {
    let all_free_rows = report
        .lines()
        .filter(|line| {
            line.strip_prefix("==")
                .and_then(|line| line.split_once("=="))
                .is_some_and(|(_, message)| {
                    message
                        .trim_start()
                        .starts_with("All heap blocks were freed -- no leaks are possible")
                })
        })
        .collect::<Vec<_>>();
    let [all_free_row] = all_free_rows.as_slice() else {
        if all_free_rows.is_empty() {
            return None;
        }
        panic!("{label} has duplicate all-free Memcheck summaries:\n{report}");
    };
    let all_free = summary_line(report, "All heap blocks were freed", label);
    let in_use = summary_line(report, "in use at exit:", label);
    let total = summary_line(report, "total heap usage:", label);
    let errors = summary_line(report, "ERROR SUMMARY:", label);
    let pid = summary_pid(&all_free, label);
    let row_prefix = format!("=={pid}== ");
    for line in report.lines().filter(|line| line.starts_with("==")) {
        assert_eq!(
            summary_pid(line, label),
            pid,
            "{label} all-free report contains a malformed or foreign process identity: {line}"
        );
    }
    assert_eq!(
        all_free,
        format!(
            "=={}== All heap blocks were freed -- no leaks are possible",
            summary_pid(all_free_row, label)
        ),
        "{label} has an unexpected all-free summary"
    );
    assert_eq!(
        in_use.strip_prefix(&row_prefix),
        Some("in use at exit: 0 bytes in 0 blocks"),
        "{label} all-free report must show zero bytes and blocks in use at exit"
    );
    assert_eq!(
        errors.strip_prefix(&row_prefix),
        Some("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)"),
        "{label} all-free report must show zero Memcheck errors and suppressions: {report}"
    );
    assert_eq!(
        summary_pid(&in_use, label),
        pid,
        "{label} process identity drift"
    );
    assert_eq!(
        summary_pid(&total, label),
        pid,
        "{label} process identity drift"
    );
    assert_eq!(
        summary_pid(&errors, label),
        pid,
        "{label} process identity drift"
    );
    let usage = total
        .strip_prefix(&row_prefix)
        .and_then(|message| message.strip_prefix("total heap usage:"))
        .expect("total heap usage prefix")
        .trim();
    let (allocations, after_allocations) = usage
        .split_once(" allocs, ")
        .unwrap_or_else(|| panic!("{label} has malformed allocation total: {total}"));
    let (frees, allocated_bytes) = after_allocations
        .split_once(" frees, ")
        .unwrap_or_else(|| panic!("{label} has malformed free total: {total}"));
    let allocated_bytes = allocated_bytes
        .strip_suffix(" bytes allocated")
        .unwrap_or_else(|| panic!("{label} has malformed allocated-byte total: {total}"));
    let allocations = parse_grouped_memcheck_count(allocations, label);
    let frees = parse_grouped_memcheck_count(frees, label);
    let _allocated_bytes = parse_grouped_memcheck_count(allocated_bytes, label);
    assert_eq!(
        allocations, frees,
        "{label} all-free heap totals disagree: {total}"
    );
    let has_leak_details = report.lines().any(|line| {
        line.strip_prefix("==")
            .and_then(|line| line.split_once("=="))
            .is_some_and(|(_, message)| {
                let message = message.trim_start();
                [
                    "LEAK SUMMARY:",
                    "definitely lost:",
                    "indirectly lost:",
                    "possibly lost:",
                    "still reachable:",
                    "suppressed:",
                ]
                .iter()
                .any(|marker| message.starts_with(marker))
                    || message.contains(" loss record ")
            })
    });
    assert!(
        !has_leak_details,
        "{label} all-free report must not mix leak summary/detail rows:\n{report}"
    );
    Some((in_use, all_free, errors))
}

fn strict_memcheck(
    valgrind: &Path,
    binary: &Path,
    directory: &Path,
    label: &str,
    expected_stdout: &[u8],
) -> (Vec<u8>, u64, u64) {
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
    let home_directory = directory.join("isolated-home");
    create_capture_directory(&home_directory);
    write_new(
        &directory.join("memcheck-environment.txt"),
        format!(
            "HOME={}\nMIMI_environment=unset\nLD_PRELOAD=unset\nVALGRIND_OPTS=unset\nVALGRIND_LIB=unset\ncommand_line_only=true\ndefault_suppressions=false\n",
            home_directory.display()
        )
        .as_bytes(),
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
    let mut command = Command::new(valgrind);
    remove_mimi_environment(&mut command);
    let output = command
        .args(&arguments)
        .env_remove("VALGRIND_OPTS")
        .env_remove("VALGRIND_LIB")
        .env("HOME", &home_directory)
        .current_dir(&home_directory)
        .output()
        .unwrap_or_else(|error| panic!("run {label} under strict Valgrind Memcheck: {error}"));
    retain_command_output(directory, valgrind.as_os_str(), &arguments, &output);

    let report = fs::read(&log_path).unwrap_or_else(|error| {
        panic!(
            "read strict Memcheck report {}: {error}",
            log_path.display()
        )
    });
    assert!(!report.is_empty(), "{label} left an empty Valgrind report");
    let report_text = text(&report);
    if let Some((in_use, all_free, error_summary)) = all_free_report_summary(&report_text, label) {
        let summary = format!("{in_use}\n{all_free}\n{error_summary}\n");
        write_new(&directory.join("memcheck-summary.txt"), summary.as_bytes());
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} changed native status"
        );
        assert_eq!(
            output.stdout.as_slice(),
            expected_stdout,
            "{label} native output"
        );
        assert!(
            output.stderr.is_empty(),
            "{label} native program emitted diagnostics: {}",
            text(&output.stderr)
        );
        eprintln!("[mimi-test] strict IsEmpty Memcheck {label} report:\n{report_text}");
        return (output.stdout, 0, 0);
    }
    let definitely_lost = summary_line(&report_text, "definitely lost:", label);
    let indirectly_lost = summary_line(&report_text, "indirectly lost:", label);
    let possibly_lost = summary_line(&report_text, "possibly lost:", label);
    let suppressed = summary_line(&report_text, "suppressed:", label);
    let error_summary = summary_line(&report_text, "ERROR SUMMARY:", label);
    assert!(
        definitely_lost.contains("definitely lost: 0 bytes in 0 blocks"),
        "{label} has definitely lost memory:\n{report_text}"
    );
    assert!(
        indirectly_lost.contains("indirectly lost: 0 bytes in 0 blocks"),
        "{label} has indirectly lost memory:\n{report_text}"
    );
    assert!(
        possibly_lost.contains("possibly lost: 0 bytes in 0 blocks"),
        "{label} has possibly lost memory:\n{report_text}"
    );
    assert!(
        suppressed.contains("suppressed: 0 bytes in 0 blocks"),
        "{label} used Memcheck suppressions:\n{report_text}"
    );
    assert!(
        error_summary.contains("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)"),
        "{label} has Memcheck errors:\n{report_text}"
    );
    let reachable = report_text
        .lines()
        .filter(|line| line.contains("still reachable:"))
        .map(str::trim)
        .collect::<Vec<_>>();
    let reachable_line = match reachable.as_slice() {
        [line] => (*line).to_owned(),
        _ => panic!("{label} has an ambiguous still-reachable summary:\n{report_text}"),
    };
    let reachable_counts = reachable_line
        .split_once("still reachable:")
        .expect("still-reachable summary marker")
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let [bytes, "bytes", "in", blocks, "blocks", ..] = reachable_counts.as_slice() else {
        panic!("{label} has an invalid still-reachable summary: {reachable_line}");
    };
    let reachable_bytes = bytes
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("parse still-reachable bytes {bytes:?}: {error}"));
    let reachable_blocks = blocks
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("parse still-reachable blocks {blocks:?}: {error}"));
    assert_eq!(
        (reachable_bytes, reachable_blocks),
        (0, 0),
        "{label} must release every heap allocation, including still-reachable owners, at process exit:\n{report_text}"
    );
    let summary = format!(
        "{definitely_lost}\n{indirectly_lost}\n{possibly_lost}\n{reachable_line}\n{suppressed}\n{error_summary}\n"
    );
    write_new(&directory.join("memcheck-summary.txt"), summary.as_bytes());

    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} changed native status or strict Memcheck found an error:\n{report_text}\napplication stderr:\n{}",
        text(&output.stderr)
    );
    assert_eq!(
        output.stdout.as_slice(),
        expected_stdout,
        "{label} native output"
    );
    assert!(
        output.stderr.is_empty(),
        "{label} native program emitted diagnostics: {}",
        text(&output.stderr)
    );
    eprintln!("[mimi-test] strict IsEmpty Memcheck {label} report:\n{report_text}");
    (output.stdout, reachable_bytes, reachable_blocks)
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-is-empty-l3.sh"]
fn ordinary_map_owned_any_v2_is_empty_default_and_explicit_native_builds_are_strict_memcheck_clean()
{
    let directory = L3Directory::new();
    let mimi = mimi_bin();
    let source = fixture();
    let control_source = map_set_drop_control_fixture();
    let target_route = route_control(
        &source,
        "IsEmpty",
        mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2IsEmpty,
        EXPECTED_STDOUT,
    );
    let control = route_control(
        &control_source,
        "MapSetDropControl",
        mir::CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControl,
        EXPECTED_CONTROL_STDOUT,
    );
    assert_eq!(target_route.profile, ROUTE_PROFILE);
    assert_eq!(control.profile, CONTROL_PROFILE);
    assert_eq!(
        target_route.payload_type_id, control.payload_type_id,
        "IsEmpty and no-query control must select the same nominal payload TypeId"
    );
    assert_eq!(
        payload_desc_without_source_field_ids(&target_route.payload_desc),
        payload_desc_without_source_field_ids(&control.payload_desc),
        "both profiles must select the same complete payload TypeDesc apart from source-specific field NodeIds"
    );
    assert_ne!(
        target_route.mir_digest, control.mir_digest,
        "the no-query control must have an independently distinct MIR graph"
    );
    assert_ne!(
        target_route.receipt_identity_digest, control.receipt_identity_digest,
        "the no-query control must have an independently distinct route receipt"
    );
    write_new(
        &directory.native.join("source.fixture.mimi"),
        &fs::read(&source).expect("read exact IsEmpty fixture source"),
    );
    write_new(
        &directory.native.join("route-control.txt"),
        target_route.digest_lines.as_bytes(),
    );
    write_new(
        &directory.native.join("map-set-drop-control.fixture.mimi"),
        &fs::read(&control_source).expect("read exact MapSetDropControl fixture source"),
    );
    let mimi_sha256 = retain_sha256(&mimi, "mimi-binary", &directory.native);
    write_new(
        &directory.native.join("mimi-binary-identity.txt"),
        format!(
            "binary={}\nsha256={mimi_sha256}\n",
            fs::canonicalize(&mimi).unwrap().display()
        )
        .as_bytes(),
    );

    let valgrind = executable_on_path("valgrind");
    let version_directory = directory.native.join("valgrind-version");
    create_capture_directory(&version_directory);
    let version_arguments = vec![OsString::from("--version")];
    let version = Command::new(&valgrind)
        .args(&version_arguments)
        .output()
        .expect("query Valgrind version");
    retain_command_output(
        &version_directory,
        valgrind.as_os_str(),
        &version_arguments,
        &version,
    );
    assert_success(&version, "Valgrind version query");
    let version_text = text(&version.stdout).trim().to_owned();
    assert!(
        version_text.starts_with("valgrind-"),
        "unexpected Valgrind version output: {version_text:?}"
    );
    let valgrind_sha256 = retain_sha256(&valgrind, "valgrind-launcher", &directory.native);
    write_new(
        &directory.native.join("valgrind-identity.txt"),
        format!(
            "launcher_path={}\nlauncher_sha256={valgrind_sha256}\nversion={}\n",
            valgrind.display(),
            version_text
        )
        .as_bytes(),
    );

    let mut outputs = Vec::new();
    let mut cli_identity: Option<(String, String)> = None;
    let control_directory = directory.native.join("control-map-set-drop");
    create_capture_directory(&control_directory);
    let control_build_directory = control_directory.join("build");
    create_capture_directory(&control_build_directory);
    let control_binary = control_build_directory.join("program");
    let control_arguments = vec![
        OsString::from("build"),
        control_source.as_os_str().to_owned(),
        OsString::from("--output"),
        control_binary.as_os_str().to_owned(),
    ];
    let mut control_command = Command::new(&mimi);
    remove_mimi_environment(&mut control_command);
    control_command
        .env("MIMI_ROUTE_CENSUS", "1")
        .args(&control_arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let control_build = control_command
        .output()
        .unwrap_or_else(|error| panic!("build independent MapSetDropControl: {error}"));
    retain_command_output(
        &control_build_directory,
        mimi.as_os_str(),
        &control_arguments,
        &control_build,
    );
    write_new(
        &control_build_directory.join("command.environment.txt"),
        b"MIMI_environment=unset_except_MIMI_ROUTE_CENSUS\nLD_PRELOAD=unset\nMIMI_ROUTE_CENSUS=1\n",
    );
    assert_success(&control_build, "independent MapSetDropControl build");
    assert!(
        control_binary.is_file(),
        "MapSetDropControl did not produce a native binary"
    );
    let control_identity = assert_route_census(
        &text(&control_build.stderr),
        false,
        "MapSetDropControl",
        CONTROL_PROFILE,
    );
    assert_eq!(control_identity.0, control.mir_digest);
    assert_eq!(control_identity.1, control.receipt_identity_digest);
    let control_memcheck_directory = control_directory.join("memcheck");
    create_capture_directory(&control_memcheck_directory);
    let (control_stdout, baseline_bytes, baseline_blocks) = strict_memcheck(
        &valgrind,
        &control_binary,
        &control_memcheck_directory,
        "MapSetDropControl native artifact",
        EXPECTED_CONTROL_STDOUT,
    );
    assert_eq!(control_stdout.as_slice(), EXPECTED_CONTROL_STDOUT);
    write_new(
        &control_directory.join("route-identity.txt"),
        format!(
            "route_profile={}\nmir_digest={}\nreceipt_identity_digest={}\npayload_selection_digest={}\nselected_type_count=1\nselected_type_id={}\nselected_type_desc_digest={}\n",
            control.profile,
            control_identity.0,
            control_identity.1,
            control.payload_selection_digest,
            control.payload_type_id.as_str(),
            control.payload_type_desc_digest
        )
        .as_bytes(),
    );

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let case_directory = directory.native.join(mode);
        create_capture_directory(&case_directory);
        let build_directory = case_directory.join("build");
        create_capture_directory(&build_directory);
        let binary = build_directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if explicit_mir {
            arguments.push(OsString::from("--mir"));
        }
        let mut build_command = Command::new(&mimi);
        remove_mimi_environment(&mut build_command);
        build_command
            .env("MIMI_ROUTE_CENSUS", "1")
            .args(&arguments)
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        let build = build_command
            .output()
            .unwrap_or_else(|error| panic!("build {mode} IsEmpty native artifact: {error}"));
        retain_command_output(&build_directory, mimi.as_os_str(), &arguments, &build);
        write_new(
            &build_directory.join("command.environment.txt"),
            b"MIMI_environment=unset_except_MIMI_ROUTE_CENSUS\nLD_PRELOAD=unset\nMIMI_ROUTE_CENSUS=1\n",
        );
        assert_success(&build, &format!("{mode} IsEmpty native build"));
        assert!(
            binary.is_file(),
            "{mode} build did not produce a native binary"
        );
        let identity = assert_route_census(&text(&build.stderr), explicit_mir, mode, ROUTE_PROFILE);
        assert_eq!(identity.0, target_route.mir_digest);
        assert_eq!(identity.1, target_route.receipt_identity_digest);
        if let Some(expected) = &cli_identity {
            assert_eq!(
                &identity, expected,
                "default and explicit --mir builds must use the same CLI-loaded MIR and receipt"
            );
        } else {
            cli_identity = Some(identity);
        }

        let memcheck_directory = case_directory.join("memcheck");
        create_capture_directory(&memcheck_directory);
        let (stdout, reachable_bytes, reachable_blocks) = strict_memcheck(
            &valgrind,
            &binary,
            &memcheck_directory,
            &format!("{mode} native artifact"),
            EXPECTED_STDOUT,
        );
        assert!(
            reachable_bytes <= baseline_bytes,
            "{mode} IsEmpty retained more reachable bytes than the independent MapSetDropControl route: target={reachable_bytes}, control={baseline_bytes}"
        );
        assert!(
            reachable_blocks <= baseline_blocks,
            "{mode} IsEmpty retained more reachable blocks than the independent MapSetDropControl route: target={reachable_blocks}, control={baseline_blocks}"
        );
        outputs.push((stdout, reachable_bytes, reachable_blocks));
    }
    assert_eq!(
        outputs[0].0, outputs[1].0,
        "default and explicit --mir stdout differ"
    );
    assert_eq!(
        baseline_bytes, 0,
        "MapSetDropControl must release every heap allocation at process exit"
    );
    assert_eq!(
        baseline_blocks, 0,
        "MapSetDropControl must leave no reachable heap blocks at process exit"
    );
    assert_eq!(
        (outputs[0].1, outputs[0].2),
        (baseline_bytes, baseline_blocks),
        "default IsEmpty must leave no Map/Any allocations beyond the dedicated no-query control"
    );
    assert_eq!(
        (outputs[1].1, outputs[1].2),
        (baseline_bytes, baseline_blocks),
        "explicit --mir IsEmpty must leave no Map/Any allocations beyond the dedicated no-query control"
    );
    write_new(
        &directory.native.join("still-reachable-comparison.txt"),
        format!(
            "target_route_profile={}\ncontrol_route_profile={}\ntarget_mir_digest={}\ncontrol_mir_digest={}\ntarget_receipt_identity_digest={}\ncontrol_receipt_identity_digest={}\ntarget_payload_selection_digest={}\ncontrol_payload_selection_digest={}\ntarget_payload_type_id={}\ncontrol_payload_type_id={}\ntarget_payload_type_desc_digest={}\ncontrol_payload_type_desc_digest={}\npayload_type_selection_equal=true\npayload_type_desc_shape_equal_ignoring_source_field_node_ids=true\nroute_graphs_distinct=true\ncontrol_reachable_bytes={baseline_bytes}\ncontrol_reachable_blocks={baseline_blocks}\ndefault_reachable_bytes={}\ndefault_reachable_blocks={}\ndefault_added_reachable_bytes={}\ndefault_added_reachable_blocks={}\nexplicit_reachable_bytes={}\nexplicit_reachable_blocks={}\nexplicit_added_reachable_bytes={}\nexplicit_added_reachable_blocks={}\n",
            target_route.profile,
            control.profile,
            target_route.mir_digest,
            control_identity.0,
            target_route.receipt_identity_digest,
            control_identity.1,
            target_route.payload_selection_digest,
            control.payload_selection_digest,
            target_route.payload_type_id.as_str(),
            control.payload_type_id.as_str(),
            target_route.payload_type_desc_digest,
            control.payload_type_desc_digest,
            outputs[0].1,
            outputs[0].2,
            outputs[0].1.saturating_sub(baseline_bytes),
            outputs[0].2.saturating_sub(baseline_blocks),
            outputs[1].1,
            outputs[1].2,
            outputs[1].1.saturating_sub(baseline_bytes),
            outputs[1].2.saturating_sub(baseline_blocks),
        )
        .as_bytes(),
    );

    let (cli_mir_digest, cli_receipt_identity_digest) =
        cli_identity.expect("default and explicit native builds emitted CLI route identities");
    write_new(
        &directory.native.join("run-summary.txt"),
        format!(
            "route_profile={ROUTE_PROFILE}\ncontrol_route_profile={CONTROL_PROFILE}\nglobal_map_any_v2_production_ready=false\ncase_count=1\nnative_build_count=3\ncontrol_native_build_count=1\nmemcheck_run_count=3\ncontrol_memcheck_run_count=1\ndefault_route_receipt_count=1\ncontrol_route_receipt_count=1\nexplicit_mir_census_count=1\nlegacy_fallback_count=0\ncontrol_reachable_bytes={baseline_bytes}\ncontrol_reachable_blocks={baseline_blocks}\ncontrol_mir_digest={}\ncontrol_receipt_identity_digest={}\nmir_digest={}\nreceipt_identity_digest={}\nselected_payload_type_count=1\npayload_type_selection_equal=true\npayload_type_desc_shape_equal_ignoring_source_field_node_ids=true\nroute_graphs_distinct=true\nexpected_stdout=true\\nfalse\\n\nvalgrind_path={}\nvalgrind_version={}\n",
            control_identity.0,
            control_identity.1,
            cli_mir_digest,
            cli_receipt_identity_digest,
            valgrind.display(),
            version_text
        )
        .as_bytes(),
    );
}
