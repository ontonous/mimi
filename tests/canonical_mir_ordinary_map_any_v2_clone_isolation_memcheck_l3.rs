use mimi::core::mir::reference::{MirReferenceInterpreter, MirRuntimeValue};
use mimi::core::mir::{self, materialize_canonical_mir_route};
use mimi::core::resolved::NodeId;
use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = mir::ORDINARY_MAP_OWNED_ANY_V2_CLONE_ISOLATION_PROFILE;
const EXPECTED_STDOUT: &[u8] = b"true\ntrue\ntrue\n";
const FIXTURES: [(&str, &str); 2] = [
    (
        "source-first",
        "mir_ordinary_map_owned_any_v2_clone_source_first.mimi",
    ),
    (
        "clone-first",
        "mir_ordinary_map_owned_any_v2_clone_clone_first.mimi",
    ),
];
const DIGEST_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];
const ENVIRONMENT_ALLOWLIST: &[&str] = &[
    "PATH",
    "LD_LIBRARY_PATH",
    "LLVM_SYS_181_PREFIX",
    "MIMI_STDLIB",
    "MIMI_ROUTE_CENSUS",
];

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

fn write_environment_snapshot(path: &Path, explicit_values: &[(&str, &str)]) {
    let mut snapshot = String::new();
    for name in ENVIRONMENT_ALLOWLIST {
        let explicit = explicit_values
            .iter()
            .find(|(explicit_name, _)| explicit_name == name)
            .map(|(_, value)| format!("{value:?}"));
        let value = explicit.or_else(|| {
            matches!(
                *name,
                "PATH" | "LD_LIBRARY_PATH" | "LLVM_SYS_181_PREFIX" | "MIMI_STDLIB"
            )
            .then(|| std::env::var_os(name))
            .flatten()
            .map(|value| format!("{:?}", value.to_string_lossy()))
        });
        match value {
            Some(value) => snapshot.push_str(&format!("{name}={value}\n")),
            None => snapshot.push_str(&format!("{name}=<unset>\n")),
        }
    }
    write_new(path, snapshot.as_bytes());
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
    panic!("strict CloneIsolation L3 requires executable {name} on PATH")
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
            "mimi-map-any-v2-clone-isolation-l3-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated CloneIsolation L3 directory {}: {error}",
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
        if let Some(configured) = std::env::var_os("MIMI_MAP_V2_CLONE_ISOLATION_L3_ARTIFACT_DIR")
            .filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let requested = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for CloneIsolation L3 artifacts")
                    .join(configured)
            };
            if fs::symlink_metadata(&requested).is_err() {
                fs::create_dir_all(&requested).unwrap_or_else(|error| {
                    panic!(
                        "create CloneIsolation L3 artifact root {}: {error}",
                        requested.display()
                    )
                });
            }
            let metadata = fs::symlink_metadata(&requested).unwrap_or_else(|error| {
                panic!("inspect artifact root {}: {error}", requested.display())
            });
            assert!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "CloneIsolation L3 artifact root must be a real directory: {}",
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

struct RouteEvidence {
    mir_digest: String,
    receipt_identity_digest: String,
    digest_lines: String,
}

fn route_evidence(source_path: &Path, source: &str, label: &str) -> RouteEvidence {
    let tokens = mimi::lexer::Lexer::new(source)
        .tokenize()
        .unwrap_or_else(|error| panic!("{label} fixture tokenizes: {error:?}"));
    let mut file = mimi::loader::parser_for_path(tokens, source_path)
        .unwrap_or_else(|error| panic!("{label} source identity resolves: {error}"))
        .parse_file()
        .unwrap_or_else(|error| panic!("{label} fixture parses: {error:?}"));
    mimi::loader::merge_prelude_into(&mut file);
    if let Some(main_idx) = file
        .items
        .iter()
        .position(|item| matches!(item, mimi::ast::Item::Func(function) if function.name == "main"))
    {
        let main_item = file.items.remove(main_idx);
        file.items.push(main_item);
    }
    let checked = mimi::core::check_program(&file)
        .unwrap_or_else(|error| panic!("{label} fixture passes Checker: {error:?}"));
    let admission = mir::classify_canonical_mir_route_admission(&checked);
    assert_eq!(
        admission.ordinary_map_owned_any_v2,
        mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation,
        "{label} must receive the exact Checker CloneIsolation admission"
    );
    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    let route = materialize_canonical_mir_route(&checked, Some(&excluded_sources))
        .unwrap_or_else(|error| panic!("{label} CloneIsolation profile materializes: {error:?}"));
    assert!(
        route.materialized_ordinary_map_owned_any_v2_clone_isolation_candidate,
        "{label} must materialize the dedicated CloneIsolation candidate"
    );
    assert!(
        !route.materialized_ordinary_map_owned_any_v2_candidate,
        "{label} must not claim broad owner-v2 materialization"
    );
    const {
        assert!(
            !mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow CloneIsolation route must leave global owner-v2 readiness closed"
        );
    };
    let receipt = route
        .ordinary_map_owned_any_v2_clone_isolation_route_receipt
        .as_ref()
        .expect("dedicated Checker-bound CloneIsolation route receipt");
    assert_eq!(receipt.profile, ROUTE_PROFILE);
    assert_eq!(receipt, &route.program.route_receipt(ROUTE_PROFILE));
    mir::validate_ordinary_map_owned_any_v2_clone_isolation_route(&checked, &route.program)
        .unwrap_or_else(|error| {
            panic!("{label} Checker/MIR CloneIsolation receipt validates: {error:?}")
        });

    let mir_digest = receipt.mir_digest.clone();
    let receipt_identity_digest = receipt.route_identity_digest();
    assert_digest(
        &mir_digest,
        &format!("{label} Checker-materialized MIR digest"),
    );
    assert_digest(
        &receipt_identity_digest,
        &format!("{label} Checker-materialized route receipt digest"),
    );

    let mut digest_lines = format!(
        "fixture={label}\nsource_identity=production-path-loader-parser\nprofile={ROUTE_PROFILE}\nreference_checker_mir_digest={mir_digest}\nreference_receipt_identity_digest={receipt_identity_digest}\n"
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
        assert_digest(digest, &format!("{label} {field}"));
        digest_lines.push_str(&format!("{field}={digest}\n"));
    }
    digest_lines.push_str("global_map_any_v2_production_ready=false\n");

    let reference = MirReferenceInterpreter::new(&route.program)
        .with_route_receipt(receipt)
        .execute_with_output(&NodeId("function:main".into()), &[])
        .unwrap_or_else(|error| panic!("{label} reference MIR executes: {error}"));
    assert_eq!(reference.value, MirRuntimeValue::Int(0));
    assert_eq!(reference.output.as_bytes(), EXPECTED_STDOUT);
    digest_lines.push_str(&format!(
        "reference_exit_code=0\nreference_stdout={}\n",
        reference.output.as_bytes().escape_ascii()
    ));

    RouteEvidence {
        mir_digest,
        receipt_identity_digest,
        digest_lines,
    }
}

fn assert_route_census(stderr: &str, explicit_mir: bool, label: &str) -> (String, String) {
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
        Some(ROUTE_PROFILE),
        "{label} did not select the dedicated CloneIsolation route"
    );
    let mir_digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{label} omitted MIR digest: {route_line}"))
        .clone();
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{label} omitted receipt identity digest: {route_line}"))
        .clone();
    assert_digest(&mir_digest, &format!("{label} MIR digest"));
    assert_digest(
        &receipt_identity_digest,
        &format!("{label} receipt identity digest"),
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
        Some(ROUTE_PROFILE)
    );
    assert_eq!(
        native_fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE)
    );
    assert_eq!(native_fields.get("mir_digest"), Some(&mir_digest));
    assert_eq!(
        native_fields.get("route_identity_digest"),
        Some(&receipt_identity_digest),
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
            "canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest={mir_digest}"
        );
        assert_eq!(
            stderr
                .lines()
                .filter(|line| *line == expected_disposition.as_str())
                .count(),
            1,
            "default build lacks its dedicated CloneIsolation disposition: {stderr}"
        );
    }
    (mir_digest, receipt_identity_digest)
}

fn assert_consumer_route_census(
    stderr: &str,
    consumer: &str,
    explicit_mir: bool,
    label: &str,
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
    assert_eq!(fields.get("consumer").map(String::as_str), Some(consumer));
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
        Some(ROUTE_PROFILE),
        "{label} did not select the dedicated CloneIsolation route"
    );
    let mir_digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{label} omitted MIR digest: {route_line}"))
        .clone();
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{label} omitted receipt identity digest: {route_line}"))
        .clone();
    assert_digest(&mir_digest, &format!("{label} MIR digest"));
    assert_digest(
        &receipt_identity_digest,
        &format!("{label} receipt identity digest"),
    );
    if !explicit_mir {
        let expected_disposition = format!(
            "canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest={mir_digest}"
        );
        assert_eq!(
            stderr
                .lines()
                .filter(|line| *line == expected_disposition.as_str())
                .count(),
            1,
            "{label} lacks its dedicated CloneIsolation disposition: {stderr}"
        );
    }
    (mir_digest, receipt_identity_digest)
}

fn summary_line<'a>(report: &'a str, marker: &str, label: &str) -> &'a str {
    let valgrind_messages = report
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let prefixed = line.strip_prefix("==").unwrap_or_else(|| {
                panic!("{label} has a Memcheck line without a Valgrind PID prefix: {line:?}")
            });
            let (pid, message) = prefixed
                .split_once("== ")
                .unwrap_or_else(|| panic!("{label} has a malformed Valgrind PID prefix: {line:?}"));
            assert!(
                !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
                "{label} has a malformed Valgrind PID: {line:?}"
            );
            (pid, message.trim())
        })
        .collect::<Vec<_>>();
    let [(report_pid, _), ..] = valgrind_messages.as_slice() else {
        panic!("{label} has no PID-bound Valgrind report rows:\n{report}");
    };
    assert!(
        valgrind_messages.iter().all(|(pid, _)| pid == report_pid),
        "{label} Memcheck report contains rows from multiple processes:\n{report}"
    );
    let matching = valgrind_messages
        .iter()
        .filter(|(_, message)| message.contains(marker))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    let all_heap_freed_count = valgrind_messages
        .iter()
        .filter(|(_, message)| *message == "All heap blocks were freed -- no leaks are possible")
        .count();
    let in_use_rows = valgrind_messages
        .iter()
        .filter_map(|(_, message)| message.strip_prefix("in use at exit:"))
        .collect::<Vec<_>>();
    if matching.is_empty()
        && matches!(
            marker,
            "definitely lost:" | "indirectly lost:" | "possibly lost:" | "still reachable:"
        )
        && all_heap_freed_count == 1
        && in_use_rows.as_slice() == [" 0 bytes in 0 blocks"]
    {
        return match marker {
            "definitely lost:" => "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost:" => "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost:" => "possibly lost: 0 bytes in 0 blocks",
            "still reachable:" => "still reachable: 0 bytes in 0 blocks",
            _ => unreachable!("only the four zero-loss summaries are synthesized"),
        };
    }
    let [line] = matching.as_slice() else {
        panic!(
            "{label} must contain one {marker:?} summary row; found {}:\n{report}",
            matching.len()
        );
    };
    line
}

fn validate_zero_heap_summary(report: &str, label: &str) {
    let messages = report
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let prefixed = line.strip_prefix("==").unwrap_or_else(|| {
                panic!("{label} has a Memcheck line without a Valgrind PID prefix: {line:?}")
            });
            let (pid, message) = prefixed
                .split_once("== ")
                .unwrap_or_else(|| panic!("{label} has a malformed Valgrind PID prefix: {line:?}"));
            assert!(
                !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
                "{label} has a malformed Valgrind PID: {line:?}"
            );
            (pid, message.trim())
        })
        .collect::<Vec<_>>();
    let [(report_pid, _), ..] = messages.as_slice() else {
        panic!("{label} has no PID-bound Valgrind report rows:\n{report}");
    };
    assert!(
        messages.iter().all(|(pid, _)| pid == report_pid),
        "{label} Memcheck report contains rows from multiple processes:\n{report}"
    );

    let in_use_rows = messages
        .iter()
        .filter(|(_, message)| message.contains("in use at exit:"))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    assert_eq!(
        in_use_rows.as_slice(),
        ["in use at exit: 0 bytes in 0 blocks"],
        "{label} must have exactly one zero in-use heap summary row:\n{report}"
    );

    let all_heap_freed_rows = messages
        .iter()
        .filter(|(_, message)| message.contains("All heap blocks were freed"))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    assert_eq!(
        all_heap_freed_rows.as_slice(),
        ["All heap blocks were freed -- no leaks are possible"],
        "{label} must have exactly one all-heap-blocks-freed row:\n{report}"
    );

    let suppressed_rows = messages
        .iter()
        .filter(|(_, message)| message.to_ascii_lowercase().starts_with("suppressed"))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    assert!(
        suppressed_rows.len() <= 1,
        "{label} has duplicate suppressed heap summaries:\n{report}"
    );
    if let Some(summary) = suppressed_rows.first() {
        let summary = summary.strip_prefix("suppressed:").unwrap_or_else(|| {
            panic!("{label} has a malformed suppressed heap summary:\n{report}")
        });
        assert_eq!(
            summary.trim(),
            "0 bytes in 0 blocks",
            "{label} has suppressed live heap allocations:\n{report}"
        );
    }

    let leak_rows = [
        "definitely lost:",
        "indirectly lost:",
        "possibly lost:",
        "still reachable:",
    ]
    .map(|marker| {
        messages
            .iter()
            .filter(|(_, message)| message.contains(marker))
            .count()
    });
    assert!(
        leak_rows == [0, 0, 0, 0] || leak_rows == [1, 1, 1, 1],
        "{label} has a partial or duplicated leak-summary set {leak_rows:?}:\n{report}"
    );
}

#[cfg(test)]
mod memcheck_summary_tests {
    use super::validate_zero_heap_summary;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn report(rows: &[&str]) -> String {
        rows.iter().map(|row| format!("==4242== {row}\n")).collect()
    }

    fn clean_report() -> String {
        report(&[
            "in use at exit: 0 bytes in 0 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost: 0 bytes in 0 blocks",
            "still reachable: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ])
    }

    fn assert_rejected(report: &str) {
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                validate_zero_heap_summary(report, "synthetic strict Memcheck report")
            }))
            .is_err(),
            "malformed Memcheck summary was accepted:\n{report}"
        );
    }

    #[test]
    fn exact_zero_heap_summary_is_accepted() {
        validate_zero_heap_summary(&clean_report(), "synthetic strict Memcheck report");
    }

    #[test]
    fn malformed_duplicate_in_use_rows_are_rejected() {
        let report = report(&[
            "in use at exit: 0 bytes in 0 blocks",
            "warning: in use at exit: 9 bytes in 1 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost: 0 bytes in 0 blocks",
            "still reachable: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ]);
        assert_rejected(&report);
    }

    #[test]
    fn malformed_all_free_rows_are_rejected() {
        let report = report(&[
            "in use at exit: 0 bytes in 0 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "warning: All heap blocks were freed -- no leaks are possible (duplicate)",
            "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost: 0 bytes in 0 blocks",
            "still reachable: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ]);
        assert_rejected(&report);
    }
}

fn error_summary_counts(summary: &str) -> Option<(u64, u64, u64, u64)> {
    let summary = summary.strip_prefix("ERROR SUMMARY: ")?;
    let (errors, summary) = summary.split_once(" errors from ")?;
    let (contexts, summary) = summary.split_once(" contexts (suppressed: ")?;
    let (suppressed_errors, summary) = summary.split_once(" from ")?;
    let suppressed_contexts = summary.strip_suffix(')')?;
    Some((
        errors.parse().ok()?,
        contexts.parse().ok()?,
        suppressed_errors.parse().ok()?,
        suppressed_contexts.parse().ok()?,
    ))
}

fn strict_memcheck(valgrind: &Path, binary: &Path, directory: &Path, label: &str) {
    write_new(
        &directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code=0\nexpected_stdout_bytes={}\n",
            EXPECTED_STDOUT.len()
        )
        .as_bytes(),
    );
    write_new(
        &directory.join("expected-native-stdout.bin"),
        EXPECTED_STDOUT,
    );
    let log_path = directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible,reachable"),
        OsString::from("--error-exitcode=97"),
        OsString::from(format!("--log-file={}", log_path.display())),
        fs::canonicalize(binary)
            .unwrap_or_else(|error| panic!("canonicalize {}: {error}", binary.display()))
            .into_os_string(),
    ];
    write_environment_snapshot(&directory.join("command.environment.txt"), &[]);
    let mut command = Command::new(valgrind);
    apply_controlled_runtime_environment(&mut command);
    let output = command
        .args(&arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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
    validate_zero_heap_summary(&report_text, label);
    let definitely_lost = summary_line(&report_text, "definitely lost:", label);
    let indirectly_lost = summary_line(&report_text, "indirectly lost:", label);
    let possibly_lost = summary_line(&report_text, "possibly lost:", label);
    let still_reachable = summary_line(&report_text, "still reachable:", label);
    let error_summary = summary_line(&report_text, "ERROR SUMMARY:", label);
    assert_eq!(
        definitely_lost, "definitely lost: 0 bytes in 0 blocks",
        "{label} has definitely lost memory:\n{report_text}"
    );
    assert_eq!(
        indirectly_lost, "indirectly lost: 0 bytes in 0 blocks",
        "{label} has indirectly lost memory:\n{report_text}"
    );
    assert_eq!(
        possibly_lost, "possibly lost: 0 bytes in 0 blocks",
        "{label} has possibly lost memory:\n{report_text}"
    );
    assert_eq!(
        still_reachable, "still reachable: 0 bytes in 0 blocks",
        "{label} leaves heap allocations reachable at process exit:\n{report_text}"
    );
    assert_eq!(
        error_summary_counts(error_summary),
        Some((0, 0, 0, 0)),
        "{label} has Memcheck errors or suppressed errors:\n{report_text}"
    );
    let summary = format!(
        "{definitely_lost}\n{indirectly_lost}\n{possibly_lost}\n{still_reachable}\nERROR SUMMARY: 0 errors\n"
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
        EXPECTED_STDOUT,
        "{label} native output"
    );
    assert!(
        output.stderr.is_empty(),
        "{label} native program emitted diagnostics: {}",
        text(&output.stderr)
    );
    eprintln!("[mimi-test] strict CloneIsolation Memcheck {label} report:\n{report_text}");
}

#[test]
fn ordinary_map_owned_any_v2_clone_isolation_cli_routes_share_checker_receipt() {
    let directory = TemporaryDirectory::new();
    let mimi = mimi_bin();

    for (order, fixture_name) in FIXTURES {
        let source = fixture(fixture_name);
        let source_text = fs::read_to_string(&source)
            .unwrap_or_else(|error| panic!("read {order} CloneIsolation fixture: {error}"));
        let evidence = route_evidence(&source, &source_text, order);
        let case_directory = directory.0.join(order);
        create_capture_directory(&case_directory);

        for explicit_mir in [false, true] {
            let mode = if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            };

            let mut run_command = Command::new(&mimi);
            apply_controlled_runtime_environment(&mut run_command);
            run_command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
            if explicit_mir {
                run_command.arg("--mir");
            }
            let run = run_command
                .arg(&source)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap_or_else(|error| panic!("run {order}/{mode}: {error}"));
            assert_success(&run, &format!("{order}/{mode} run"));
            assert_eq!(run.stdout, EXPECTED_STDOUT, "{order}/{mode} run output");
            let run_route = assert_consumer_route_census(
                &text(&run.stderr),
                "run",
                explicit_mir,
                &format!("{order}/{mode} run"),
            );
            assert_eq!(run_route.0, evidence.mir_digest, "{order}/{mode} run MIR");
            assert_eq!(
                run_route.1, evidence.receipt_identity_digest,
                "{order}/{mode} run receipt"
            );

            let build_directory = case_directory.join(mode);
            create_capture_directory(&build_directory);
            let binary = build_directory.join("program");
            let mut build_command = Command::new(&mimi);
            apply_controlled_runtime_environment(&mut build_command);
            build_command
                .env("MIMI_ROUTE_CENSUS", "1")
                .arg("build")
                .arg(&source)
                .arg("--output")
                .arg(&binary);
            if explicit_mir {
                build_command.arg("--mir");
            }
            let build = build_command
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap_or_else(|error| panic!("build {order}/{mode}: {error}"));
            assert_success(&build, &format!("{order}/{mode} build"));
            assert!(binary.is_file(), "{order}/{mode} did not produce a binary");
            let build_route = assert_route_census(
                &text(&build.stderr),
                explicit_mir,
                &format!("{order}/{mode} build"),
            );
            assert_eq!(
                build_route.0, evidence.mir_digest,
                "{order}/{mode} build MIR"
            );
            assert_eq!(
                build_route.1, evidence.receipt_identity_digest,
                "{order}/{mode} build receipt"
            );

            let native = Command::new(&binary)
                .output()
                .unwrap_or_else(|error| panic!("run {order}/{mode} native binary: {error}"));
            assert_success(&native, &format!("{order}/{mode} native binary"));
            assert_eq!(
                native.stdout, EXPECTED_STDOUT,
                "{order}/{mode} native output"
            );

            let mut verify_command = Command::new(&mimi);
            apply_controlled_runtime_environment(&mut verify_command);
            verify_command.env("MIMI_ROUTE_CENSUS", "1").arg("verify");
            if explicit_mir {
                verify_command.arg("--mir");
            }
            let verify = verify_command
                .arg(&source)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap_or_else(|error| panic!("verify {order}/{mode}: {error}"));
            assert_success(&verify, &format!("{order}/{mode} verify"));
            assert!(
                text(&verify.stdout).contains("No contracts to verify"),
                "{order}/{mode} verifier result changed: {}",
                text(&verify.stdout)
            );
            let verify_route = assert_consumer_route_census(
                &text(&verify.stderr),
                "verify",
                explicit_mir,
                &format!("{order}/{mode} verify"),
            );
            assert_eq!(
                verify_route.0, evidence.mir_digest,
                "{order}/{mode} verify MIR"
            );
            assert_eq!(
                verify_route.1, evidence.receipt_identity_digest,
                "{order}/{mode} verify receipt"
            );
        }
    }

    let source_first = fixture(FIXTURES[0].1);
    let source_first_text = fs::read_to_string(&source_first)
        .expect("read source-first fixture for wrong-payload negative route");
    let expected_comparison = "let source_is_original = source_payload == \"original payload\"";
    let mutated_comparison =
        "let source_is_original = source_payload == \"not the stored payload\"";
    assert_eq!(
        source_first_text.matches(expected_comparison).count(),
        1,
        "the negative mutation anchor must identify exactly one selected payload comparison"
    );
    let negative_source_text =
        source_first_text.replacen(expected_comparison, mutated_comparison, 1);
    assert_ne!(
        negative_source_text, source_first_text,
        "the negative CLI source mutation must change exactly one selected payload comparison"
    );
    let negative_source = directory.0.join("clone-source-wrong-payload.mimi");
    fs::write(&negative_source, &negative_source_text)
        .expect("write isolated wrong-payload CloneIsolation source");
    let negative_tokens = mimi::lexer::Lexer::new(&negative_source_text)
        .tokenize()
        .expect("the wrong-payload negative fixture still tokenizes");
    let mut negative_file = mimi::loader::parser_for_path(negative_tokens, &negative_source)
        .expect("the wrong-payload negative fixture keeps production source identity")
        .parse_file()
        .expect("the wrong-payload negative fixture still parses");
    mimi::loader::merge_prelude_into(&mut negative_file);
    if let Some(main_idx) = negative_file
        .items
        .iter()
        .position(|item| matches!(item, mimi::ast::Item::Func(function) if function.name == "main"))
    {
        let main_item = negative_file.items.remove(main_idx);
        negative_file.items.push(main_item);
    }
    let negative_checked = mimi::core::check_program(&negative_file)
        .expect("the wrong-payload negative fixture still passes Checker");
    assert_ne!(
        mir::classify_canonical_mir_route_admission(&negative_checked).ordinary_map_owned_any_v2,
        mir::OrdinaryMapOwnedAnyV2Admission::CloneIsolation,
        "a different comparison literal must lose the CloneIsolation Checker admission"
    );

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        for consumer in ["run", "build", "verify"] {
            let mut command = Command::new(&mimi);
            apply_controlled_runtime_environment(&mut command);
            command.env("MIMI_ROUTE_CENSUS", "1").arg(consumer);
            if explicit_mir {
                command.arg("--mir");
            }
            command.arg(&negative_source);
            let binary = if consumer == "build" {
                let output = directory.0.join(format!("negative-{mode}-program"));
                command.arg("--output").arg(&output);
                Some(output)
            } else {
                None
            };
            let output = command
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap_or_else(|error| panic!("negative {mode} {consumer}: {error}"));
            let label = format!("negative {mode} {consumer}");
            assert!(
                !output.status.success(),
                "{label} unexpectedly accepted the wrong-payload clone profile:\nstdout:\n{}\nstderr:\n{}",
                text(&output.stdout),
                text(&output.stderr)
            );
            assert!(
                output.stdout.is_empty(),
                "{label} emitted program output before route rejection: {}",
                text(&output.stdout)
            );
            let stderr = text(&output.stderr);
            assert!(
                stderr.contains("MIR-COVERAGE-001"),
                "{label} did not fail closed at the unsupported ownership boundary: {stderr}"
            );
            for forbidden in [
                "canonical route disposition: canonical",
                "canonical route disposition: explicit canonical",
                "canonical route disposition: legacy",
                "MIMI_ROUTE_CENSUS route-admission ",
                "MIMI_ROUTE_CENSUS canonical-native-build ",
            ] {
                assert!(
                    !stderr.contains(forbidden),
                    "{label} accepted or fell back through forbidden route evidence {forbidden:?}: {stderr}"
                );
            }
            if let Some(binary) = binary {
                assert!(
                    !binary.exists(),
                    "{label} left a native artifact despite fail-closed rejection"
                );
            }
        }
    }
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-clone-isolation-l3.sh"]
fn ordinary_map_owned_any_v2_clone_isolation_both_drop_orders_are_strict_memcheck_clean() {
    let directory = L3Directory::new();
    let mimi = mimi_bin();
    let mimi_sha256 = retain_sha256(&mimi, "mimi-binary", &directory.native);
    write_new(
        &directory.native.join("mimi-binary-identity.txt"),
        format!(
            "binary={}\nsha256={mimi_sha256}\n",
            fs::canonicalize(&mimi)
                .unwrap_or_else(|error| panic!("canonicalize Mimi binary: {error}"))
                .display()
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

    let mut checked_builds = 0usize;
    for (order, fixture_name) in FIXTURES {
        let source = fixture(fixture_name);
        let source_bytes = fs::read(&source)
            .unwrap_or_else(|error| panic!("read exact {order} fixture source: {error}"));
        let source_text = text(&source_bytes);
        let evidence = route_evidence(&source, &source_text, order);
        let case_directory = directory.native.join(order);
        create_capture_directory(&case_directory);
        write_new(&case_directory.join("source.fixture.mimi"), &source_bytes);
        write_new(
            &case_directory.join("route-control.txt"),
            evidence.digest_lines.as_bytes(),
        );
        let source_sha256 = retain_sha256(&source, &format!("{order}-fixture"), &case_directory);
        write_new(
            &case_directory.join("source-identity.txt"),
            format!(
                "source={}\nsha256={source_sha256}\n",
                fs::canonicalize(&source)
                    .unwrap_or_else(|error| panic!("canonicalize {order} fixture: {error}"))
                    .display()
            )
            .as_bytes(),
        );

        let mut file_backed_receipt: Option<(String, String)> = None;
        for explicit_mir in [false, true] {
            let mode = if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            };
            let mode_directory = case_directory.join(mode);
            create_capture_directory(&mode_directory);
            let build_directory = mode_directory.join("build");
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
            apply_controlled_runtime_environment(&mut build_command);
            build_command
                .env("MIMI_ROUTE_CENSUS", "1")
                .args(&arguments)
                .current_dir(env!("CARGO_MANIFEST_DIR"));
            let build = build_command
                .output()
                .unwrap_or_else(|error| panic!("build {order}/{mode} native artifact: {error}"));
            retain_command_output(&build_directory, mimi.as_os_str(), &arguments, &build);
            write_environment_snapshot(
                &build_directory.join("command.environment.txt"),
                &[("MIMI_ROUTE_CENSUS", "1")],
            );
            assert_success(&build, &format!("{order}/{mode} native build"));
            assert!(
                binary.is_file(),
                "{order}/{mode} build produced no native binary"
            );
            let observed_receipt = assert_route_census(
                &text(&build.stderr),
                explicit_mir,
                &format!("{order}/{mode}"),
            );
            assert_eq!(
                observed_receipt.0.as_str(),
                evidence.mir_digest.as_str(),
                "{order}/{mode} native route uses the same MIR as the reference interpreter"
            );
            assert_eq!(
                observed_receipt.1.as_str(),
                evidence.receipt_identity_digest.as_str(),
                "{order}/{mode} native route uses the same Checker receipt as the reference interpreter"
            );
            if let Some(expected_receipt) = &file_backed_receipt {
                assert_eq!(
                    &observed_receipt, expected_receipt,
                    "{order} default and explicit --mir routes must share one file-backed MIR and receipt identity"
                );
            } else {
                file_backed_receipt = Some(observed_receipt);
            }
            let binary_sha256 = retain_sha256(&binary, "native-binary", &build_directory);
            write_new(
                &build_directory.join("binary-identity.txt"),
                format!(
                    "binary={}\nsha256={binary_sha256}\n",
                    fs::canonicalize(&binary)
                        .unwrap_or_else(|error| panic!("canonicalize native binary: {error}"))
                        .display()
                )
                .as_bytes(),
            );

            let memcheck_directory = mode_directory.join("memcheck");
            create_capture_directory(&memcheck_directory);
            strict_memcheck(
                &valgrind,
                &binary,
                &memcheck_directory,
                &format!("{order}/{mode} native artifact"),
            );
            checked_builds += 1;
        }
    }

    assert_eq!(
        checked_builds, 4,
        "both drop orders need default and explicit MIR builds"
    );
    write_new(
        &directory.native.join("run-summary.txt"),
        format!(
            "route_profile={ROUTE_PROFILE}\nglobal_map_any_v2_production_ready=false\nfixture_order_count=2\nnative_build_count={checked_builds}\nmemcheck_run_count={checked_builds}\nmemcheck_still_reachable=0 bytes in 0 blocks\nsource_first_default_checked=true\nsource_first_explicit_mir_checked=true\nclone_first_default_checked=true\nclone_first_explicit_mir_checked=true\nexpected_stdout=true\\ntrue\\ntrue\\n\nvalgrind_path={}\nvalgrind_version={}\n",
            valgrind.display(),
            version_text
        )
        .as_bytes(),
    );
}
