use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "map-any-owner-v2-computed-key-get-v1";
const MIR_RECEIPT_PROFILE: &str = "cli-mir-v1";
const MIR_RECEIPT_HEADER: &str = "mimi-mir-route-manifest-v1";
const MIR_RECEIPT_SCHEMA: &str = "mimi-mir-route-receipt-v1";
const EXPECTED_STDOUT: &[u8] = b"";
const MIR_RECEIPT_FIELDS: &[&str] = &[
    "schema",
    "profile",
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
    "root_owners",
];
const RECEIPT_DIGEST_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];
const BUILD_RECEIPT_FIELDS: &[&str] = &[
    "consumer",
    "route_id",
    "schema",
    "profile",
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
    "root_owners",
    "route_identity_digest",
];
const VERIFIER_RECEIPT_FIELDS: &[&str] = &[
    "profile",
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
    "source_hash",
    "route_identity_digest",
];
const NEGATIVE_COMMANDS_PER_CASE: usize = 6;

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide mimi for ComputedKeyGet production L3 evidence")
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

fn command_environment(
    environment: &[(&str, &str)],
    working_directory: Option<&Path>,
    clear_valgrind_overrides: bool,
) -> String {
    const RECORDED_ENVIRONMENT: &[&str] = &[
        "CARGO_BUILD_JOBS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        "CC",
        "CFLAGS",
        "CXX",
        "HOME",
        "LDFLAGS",
        "LIBCLANG_PATH",
        "LLVM_SYS_181_PREFIX",
        "MIMI_MAP_V2_COMPUTED_KEY_GET_L3_ARTIFACT_DIR",
        "MIMI_MAP_V2_COMPUTED_KEY_GET_L3_LOG",
        "MIMI_STDLIB",
        "MIMI_ROUTE_CENSUS",
        "MIMI_VERBOSE",
        "PATH",
        "RUSTFLAGS",
        "RUSTUP_TOOLCHAIN",
        "TMPDIR",
        "VALGRIND_OPTS",
    ];
    let explicit = environment
        .iter()
        .map(|(key, value)| (*key, *value))
        .collect::<BTreeMap<_, _>>();
    let mut fields = BTreeMap::new();
    fields.insert(
        "cwd".to_string(),
        working_directory
            .map(Path::to_path_buf)
            .unwrap_or_else(|| {
                std::env::current_dir().expect("resolve cwd for L3 command evidence")
            })
            .display()
            .to_string(),
    );
    for key in RECORDED_ENVIRONMENT {
        let value = explicit
            .get(key)
            .map(|value| (*value).to_string())
            .or_else(|| std::env::var_os(key).map(|value| value.to_string_lossy().into_owned()));
        fields.insert(
            (*key).to_string(),
            value.unwrap_or_else(|| "<unset>".to_string()),
        );
    }
    if clear_valgrind_overrides {
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("VALGRIND_") {
                fields.insert(name.into_owned(), "<unset>".to_string());
            }
        }
    }
    for (key, value) in environment {
        fields.insert((*key).to_string(), (*value).to_string());
    }
    fields
        .into_iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}

fn retain_command_output(
    directory: &Path,
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    working_directory: Option<&Path>,
    clear_valgrind_overrides: bool,
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
    write_new(
        &directory.join("command.environment.txt"),
        command_environment(environment, working_directory, clear_valgrind_overrides).as_bytes(),
    );
}

fn run_recorded(
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
) -> Output {
    let clear_valgrind_overrides = program == OsStr::new("valgrind");
    run_recorded_with_cwd(
        program,
        arguments,
        environment,
        directory,
        None,
        clear_valgrind_overrides,
    )
}

fn run_recorded_with_cwd(
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
    working_directory: Option<&Path>,
    clear_valgrind_overrides: bool,
) -> Output {
    let mut command = Command::new(program);
    command.args(arguments);
    if clear_valgrind_overrides {
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("VALGRIND_") {
                command.env_remove(key);
            }
        }
    }
    for (key, value) in environment {
        if *value == "<unset>" {
            command.env_remove(key);
        } else {
            command.env(key, value);
        }
    }
    if let Some(working_directory) = working_directory {
        command.current_dir(working_directory);
    }
    match command.output() {
        Ok(output) => {
            retain_command_output(
                directory,
                program,
                arguments,
                environment,
                working_directory,
                clear_valgrind_overrides,
                &output,
            );
            output
        }
        Err(error) => {
            write_new(
                &directory.join("command.argv.json"),
                command_json(program, arguments).as_bytes(),
            );
            write_new(&directory.join("stdout.bin"), b"");
            write_new(
                &directory.join("stderr.bin"),
                format!("failed to spawn command: {error}\n").as_bytes(),
            );
            write_new(
                &directory.join("process.status"),
                format!("spawn_error={error}\nsuccess=false\n").as_bytes(),
            );
            let environment =
                command_environment(environment, working_directory, clear_valgrind_overrides);
            write_new(
                &directory.join("command.environment.txt"),
                environment.as_bytes(),
            );
            panic!(
                "invoke {} for ComputedKeyGet L3 evidence: {error}",
                program.to_string_lossy()
            );
        }
    }
}

fn run_mimi(
    mimi: &Path,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
) -> Output {
    run_recorded(mimi.as_os_str(), arguments, environment, directory)
}

fn assert_success(output: &Output, mode: &str) {
    assert!(
        output.status.success(),
        "{mode} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn assert_no_legacy_fallback(stderr: &str, mode: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} selected a legacy route: {stderr}"
    );
}

fn parse_key_values<'a>(
    tokens: impl Iterator<Item = &'a str>,
    mode: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for token in tokens {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{mode} contains a malformed receipt token: {token}"));
        assert!(
            fields.insert(key.to_string(), value.to_string()).is_none(),
            "{mode} repeats receipt field {key}"
        );
    }
    fields
}

fn parse_mir_receipt(output: &Output, mode: &str) -> BTreeMap<String, String> {
    assert_success(output, mode);
    let receipt_text = text(&output.stdout);
    let mut lines = receipt_text.lines();
    assert_eq!(
        lines.next(),
        Some(MIR_RECEIPT_HEADER),
        "{mode} changed the canonical MIR route manifest header"
    );
    let mut fields = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for (row, expected_field) in MIR_RECEIPT_FIELDS.iter().enumerate() {
        let line = lines.next().unwrap_or_else(|| {
            panic!("{mode} MIR route manifest omitted field {expected_field} at row {row}")
        });
        let (field, value) = line.split_once('=').unwrap_or_else(|| {
            panic!("{mode} MIR route manifest row {row} is missing '=': {line:?}")
        });
        assert!(
            MIR_RECEIPT_FIELDS.contains(&field),
            "{mode} MIR route manifest has unknown field {field:?} at row {row}"
        );
        assert!(
            seen.insert(field),
            "{mode} MIR route manifest repeats field {field:?} at row {row}"
        );
        assert_eq!(
            field, *expected_field,
            "{mode} MIR route manifest field order changed at row {row}"
        );
        fields.insert(field.to_string(), value.to_string());
    }
    assert!(
        lines.next().is_none(),
        "{mode} MIR route manifest has rows after its exact field set: {receipt_text}"
    );
    assert_eq!(
        fields.len(),
        MIR_RECEIPT_FIELDS.len(),
        "{mode} changed the canonical MIR receipt field set: {fields:?}"
    );
    assert_eq!(
        fields.get("schema").map(String::as_str),
        Some(MIR_RECEIPT_SCHEMA),
        "{mode} changed the canonical MIR receipt schema"
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(MIR_RECEIPT_PROFILE),
        "{mode} changed the CLI receipt schema"
    );
    for field in MIR_RECEIPT_FIELDS {
        assert!(fields.contains_key(*field), "{mode} omitted {field}");
    }
    for field in [
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
    ] {
        let digest = &fields[field];
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{mode} has malformed {field}: {digest}"
        );
    }
    let owners = fields
        .get("root_owners")
        .expect("route manifest has the required root_owners field")
        .split(',')
        .collect::<Vec<_>>();
    assert!(
        !owners.is_empty()
            && owners.iter().all(|owner| {
                !owner.is_empty()
                    && !owner.chars().any(|character| {
                        character.is_control()
                            || character.is_whitespace()
                            || character == '='
                            || character == ','
                    })
            }),
        "{mode} has empty or unsafe root owners: {fields:?}"
    );
    assert!(
        owners.windows(2).all(|pair| pair[0] < pair[1]),
        "{mode} root owners are not strictly sorted: {fields:?}"
    );
    assert!(
        owners.contains(&"function:main"),
        "{mode} omitted the main owner: {fields:?}"
    );
    assert!(
        !fields.contains_key("execution_route")
            && !fields.contains_key("production_ready")
            && !fields.contains_key("global_map_any_v2_production_ready"),
        "{mode} receipt claimed global production readiness: {fields:?}"
    );
    fields
}

fn assert_receipt_identity(
    observed: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
    mode: &str,
) {
    for field in MIR_RECEIPT_FIELDS {
        if *field == "profile" {
            continue;
        }
        assert_eq!(
            observed.get(*field),
            expected.get(*field),
            "{mode} changed MIR receipt identity field {field}"
        );
    }
}

fn assert_default_profile(stderr: &str, digest: &str, mode: &str) {
    let expected =
        format!("canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest={digest}");
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("canonical route disposition: canonical ("))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} did not select exactly the dedicated ComputedKeyGet route: {stderr}"
    );
    assert_eq!(matching[0], expected.as_str());
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_route_admission(
    stderr: &str,
    consumer: &str,
    mode_name: &str,
    digest: &str,
    mode: &str,
) -> (String, String, String) {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must report exactly one Checker route-admission record: {stderr}"
    );
    let tokens = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route-admission census has its stable prefix")
        .split_whitespace()
        .collect::<Vec<_>>();
    let field_order = tokens
        .iter()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} contains a malformed route token: {token}"))
                .0
        })
        .collect::<Vec<_>>();
    assert_eq!(
        field_order,
        [
            "consumer",
            "mode",
            "checker_profile",
            "mir_digest",
            "receipt_identity_digest"
        ]
        .to_vec(),
        "{mode} route-admission field order changed"
    );
    let fields = parse_key_values(tokens.into_iter(), mode);
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "checker_profile",
            "consumer",
            "mir_digest",
            "mode",
            "receipt_identity_digest"
        ],
        "{mode} route-admission schema changed"
    );
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some(consumer),
        "{mode} reported the wrong route-admission consumer"
    );
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(mode_name),
        "{mode} reported the wrong route-admission mode"
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} did not report the dedicated Checker admission profile"
    );
    assert_eq!(
        fields.get("mir_digest").map(String::as_str),
        Some(digest),
        "{mode} route-admission record is not bound to the exact MIR digest"
    );
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .expect("route-admission record has receipt_identity_digest");
    assert!(
        receipt_identity_digest.len() == 64
            && receipt_identity_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{mode} route-admission identity digest is malformed: {receipt_identity_digest}"
    );
    let profile = fields
        .get("checker_profile")
        .expect("route-admission record has checker_profile")
        .clone();
    let observed_digest = fields
        .get("mir_digest")
        .expect("route-admission record has mir_digest")
        .clone();
    let observed_identity_digest = receipt_identity_digest.clone();
    assert_no_legacy_fallback(stderr, mode);
    (profile, observed_digest, observed_identity_digest)
}

fn assert_native_census(stderr: &str, digest: &str, mode: &str) {
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one complete canonical MIR native census: {stderr}"
    );
    let (counts, observed_digest) = matching[0]
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("native census has its stable prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("native census reports no legacy fallback and a MIR digest");
    let (emitted, expected) = counts
        .split_once('/')
        .expect("native census reports emitted and expected function counts");
    let emitted = emitted
        .parse::<usize>()
        .expect("native census emitted function count is numeric");
    let expected = expected
        .parse::<usize>()
        .expect("native census expected function count is numeric");
    assert!(expected > 0, "{mode} native census is empty");
    assert_eq!(emitted, expected, "{mode} omitted a MIR function");
    assert_eq!(
        observed_digest, digest,
        "{mode} native census MIR digest differs"
    );
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_explicit_run_profile(stderr: &str, digest: &str, mode: &str) {
    let expected = format!(
        "canonical route disposition: explicit canonical ({ROUTE_PROFILE}) mir_digest={digest}"
    );
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("canonical route disposition: explicit canonical ("))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} did not report one explicit MIR run receipt: {stderr}"
    );
    assert_eq!(matching[0], expected.as_str());
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_build_receipt(
    stderr: &str,
    consumer: &str,
    expected: &BTreeMap<String, String>,
    expected_route_identity_digest: &str,
    mode: &str,
) {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must emit one build receipt: {stderr}"
    );
    let mut tokens = lines[0].split_whitespace();
    assert_eq!(tokens.next(), Some("MIMI_ROUTE_CENSUS"));
    assert_eq!(tokens.next(), Some("canonical-native-build"));
    let receipt_tokens = tokens.collect::<Vec<_>>();
    let field_order = receipt_tokens
        .iter()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} contains a malformed build token: {token}"))
                .0
        })
        .collect::<Vec<_>>();
    assert_eq!(
        field_order,
        BUILD_RECEIPT_FIELDS.to_vec(),
        "{mode} native build receipt schema or field order changed"
    );
    let fields = parse_key_values(receipt_tokens.into_iter(), mode);
    assert_eq!(
        fields.get("schema").map(String::as_str),
        Some(MIR_RECEIPT_SCHEMA),
        "{mode} native build receipt schema changed"
    );
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some(consumer),
        "{mode} reported the wrong build consumer"
    );
    assert_eq!(
        fields.get("route_id").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} changed the build receipt identity"
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} changed the build receipt profile"
    );
    assert_eq!(
        fields.get("route_identity_digest").map(String::as_str),
        Some(expected_route_identity_digest),
        "{mode} native build route identity differs from route admission"
    );
    assert_receipt_identity(&fields, expected, mode);
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_verifier_receipt(
    stderr: &str,
    expected: &BTreeMap<String, String>,
    expected_source_hash: &str,
    expected_route_identity_digest: &str,
    mode: &str,
) {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("canonical MIR verifier provenance: "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must expose the actual verifier receipt (Z3 must be available): {stderr}"
    );
    let tokens = lines[0]
        .strip_prefix("canonical MIR verifier provenance: ")
        .expect("verifier receipt has its prefix")
        .split_whitespace()
        .collect::<Vec<_>>();
    let field_order = tokens
        .iter()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} contains a malformed verifier token: {token}"))
                .0
        })
        .collect::<Vec<_>>();
    assert_eq!(
        field_order,
        VERIFIER_RECEIPT_FIELDS.to_vec(),
        "{mode} verifier receipt schema or field order changed"
    );
    let fields = parse_key_values(tokens.into_iter(), mode);
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} changed the verifier receipt profile"
    );
    assert_eq!(
        fields.get("route_identity_digest").map(String::as_str),
        Some(expected_route_identity_digest),
        "{mode} verifier route identity differs from route admission"
    );
    for field in RECEIPT_DIGEST_FIELDS {
        assert_eq!(
            fields.get(*field),
            expected.get(*field),
            "{mode} changed MIR receipt identity field {field}"
        );
    }
    let observed_source_hash = fields
        .get("source_hash")
        .expect("verifier receipt has source_hash");
    assert!(
        observed_source_hash.len() == 64
            && observed_source_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{mode} has malformed source_hash: {observed_source_hash}"
    );
    assert_eq!(
        observed_source_hash, expected_source_hash,
        "{mode} verifier source_hash is not bound to the captured fixture bytes"
    );
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_no_legacy_or_route_success(output: &Output, mode: &str) {
    let stderr = text(&output.stderr);
    let diagnostic = format!("{}\n{stderr}", text(&output.stdout));
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} attempted legacy fallback:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: canonical ("),
        "{mode} reported a selected canonical route despite rejecting the shape:\n{diagnostic}"
    );
}

fn assert_fail_closed(output: &Output, mode: &str, expected_diagnostic: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{mode} must reject a shape outside ComputedKeyGet with exit code 1:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr),
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains("[E0200]")
            || diagnostic.contains("MIR-COVERAGE-001")
            || diagnostic.contains("not production-ready"),
        "{mode} did not fail closed in Checker or at the canonical coverage/readiness boundary:\n{diagnostic}"
    );
    assert!(
        diagnostic.contains(expected_diagnostic),
        "{mode} lost the stable ComputedKeyGet near-miss diagnostic {expected_diagnostic:?}:\n{diagnostic}"
    );
    assert_no_legacy_or_route_success(output, mode);
}

fn retain_sha256(path: &Path, label: &str, source: &str, evidence_root: &Path) -> String {
    let canonical = fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()));
    let directory = evidence_root.join(format!("{label}-sha256"));
    create_capture_directory(&directory);
    let arguments = vec![OsString::from("--"), canonical.as_os_str().to_owned()];
    let output = run_recorded(OsStr::new("sha256sum"), &arguments, &[], &directory);
    assert_success(&output, &format!("sha256sum {}", canonical.display()));
    let line = text(&output.stdout);
    let (digest, hashed_path) = line
        .trim_end()
        .split_once("  ")
        .expect("sha256sum output contains digest and path");
    assert_eq!(hashed_path, canonical.to_string_lossy());
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "sha256sum returned malformed digest {digest}"
    );
    write_new(
        &evidence_root.join(format!("{label}-identity.txt")),
        format!(
            "binary_source={source}\ncanonical_path={}\nsha256={digest}\n",
            canonical.display()
        )
        .as_bytes(),
    );
    digest.to_string()
}

fn memcheck_summary(report: &str, mode: &str) -> String {
    let messages = report
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let prefixed = line.strip_prefix("==").unwrap_or_else(|| {
                panic!("{mode} has a Memcheck line without a Valgrind PID prefix: {line:?}")
            });
            let (pid, message) = prefixed
                .split_once("== ")
                .unwrap_or_else(|| panic!("{mode} has a malformed Valgrind PID prefix: {line:?}"));
            assert!(
                !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
                "{mode} has a malformed Valgrind PID: {line:?}"
            );
            (pid, message.trim())
        })
        .collect::<Vec<_>>();
    let [(report_pid, _), ..] = messages.as_slice() else {
        panic!("{mode} has no PID-bound Valgrind report rows:\n{report}");
    };
    assert!(
        messages.iter().all(|(pid, _)| pid == report_pid),
        "{mode} Memcheck report combines multiple process IDs:\n{report}"
    );

    let in_use_rows = messages
        .iter()
        .filter(|(_, message)| message.contains("in use at exit:"))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    assert_eq!(
        in_use_rows.as_slice(),
        ["in use at exit: 0 bytes in 0 blocks"],
        "{mode} must have exactly one zero in-use heap summary row:\n{report}"
    );
    let all_freed_rows = messages
        .iter()
        .filter(|(_, message)| message.contains("All heap blocks were freed"))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    assert_eq!(
        all_freed_rows.as_slice(),
        ["All heap blocks were freed -- no leaks are possible"],
        "{mode} must have exactly one canonical all-free summary row:\n{report}"
    );

    let suppressed_rows = messages
        .iter()
        .filter(|(_, message)| message.to_ascii_lowercase().starts_with("suppressed"))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    assert!(
        suppressed_rows.len() <= 1,
        "{mode} has duplicate suppressed heap summaries:\n{report}"
    );
    if let Some(summary) = suppressed_rows.first() {
        assert_eq!(
            *summary, "suppressed: 0 bytes in 0 blocks",
            "{mode} has suppressed live heap allocations or a malformed summary:\n{report}"
        );
    }

    let leak_markers = [
        "definitely lost:",
        "indirectly lost:",
        "possibly lost:",
        "still reachable:",
    ];
    let leak_rows = leak_markers.map(|marker| {
        messages
            .iter()
            .filter(|(_, message)| message.contains(marker))
            .map(|(_, message)| *message)
            .collect::<Vec<_>>()
    });
    let observed_counts = leak_rows.each_ref().map(Vec::len);
    assert!(
        observed_counts == [0, 0, 0, 0] || observed_counts == [1, 1, 1, 1],
        "{mode} has a partial or duplicate leak-summary set {observed_counts:?}:\n{report}"
    );
    let mut selected = vec![in_use_rows[0], all_freed_rows[0]];
    if observed_counts == [0, 0, 0, 0] {
        for marker in leak_markers {
            selected.push(match marker {
                "definitely lost:" => "definitely lost: 0 bytes in 0 blocks",
                "indirectly lost:" => "indirectly lost: 0 bytes in 0 blocks",
                "possibly lost:" => "possibly lost: 0 bytes in 0 blocks",
                "still reachable:" => "still reachable: 0 bytes in 0 blocks",
                _ => unreachable!("closed leak summary marker set"),
            });
        }
    } else {
        let expected = [
            "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost: 0 bytes in 0 blocks",
            "still reachable: 0 bytes in 0 blocks",
        ];
        for (rows, expected_line) in leak_rows.iter().zip(expected) {
            assert_eq!(
                rows.as_slice(),
                [expected_line],
                "{mode} has nonzero or malformed {expected_line}:\n{report}"
            );
            selected.push(rows[0]);
        }
    }
    if let Some(summary) = suppressed_rows.first() {
        selected.push(summary);
    }

    let error_rows = messages
        .iter()
        .filter(|(_, message)| message.contains("ERROR SUMMARY:"))
        .map(|(_, message)| *message)
        .collect::<Vec<_>>();
    let [error_summary] = error_rows.as_slice() else {
        panic!("{mode} must contain one ERROR SUMMARY row:\n{report}");
    };
    assert_eq!(
        error_summary_counts(error_summary),
        Some((0, 0, 0, 0)),
        "{mode} reported Memcheck or suppressed errors:\n{report}"
    );
    selected.push("ERROR SUMMARY: 0 errors");
    format!("{}\n", selected.join("\n"))
}

fn error_summary_counts(summary: &str) -> Option<(u64, u64, u64, u64)> {
    let summary = summary.strip_prefix("ERROR SUMMARY: ")?;
    let (errors, summary) = summary.split_once(" errors from ")?;
    let (contexts, summary) = summary.split_once(" contexts (suppressed: ")?;
    let suppressed_errors = summary.strip_suffix(')')?;
    let (suppressed_errors, suppressed_contexts) = suppressed_errors.split_once(" from ")?;
    Some((
        errors.parse().ok()?,
        contexts.parse().ok()?,
        suppressed_errors.parse().ok()?,
        suppressed_contexts.parse().ok()?,
    ))
}

#[cfg(test)]
mod memcheck_summary_tests {
    use super::memcheck_summary;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn report(rows: &[&str]) -> String {
        rows.iter().map(|row| format!("==4242== {row}\n")).collect()
    }

    fn all_free_report(include_leak_rows: bool) -> String {
        let mut rows = vec!["in use at exit: 0 bytes in 0 blocks"];
        if include_leak_rows {
            rows.extend([
                "definitely lost: 0 bytes in 0 blocks",
                "indirectly lost: 0 bytes in 0 blocks",
                "possibly lost: 0 bytes in 0 blocks",
                "still reachable: 0 bytes in 0 blocks",
            ]);
        }
        rows.extend([
            "All heap blocks were freed -- no leaks are possible",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ]);
        report(&rows)
    }

    fn assert_rejected(report: &str) {
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                memcheck_summary(report, "synthetic strict Memcheck report")
            }))
            .is_err(),
            "malformed Memcheck report was accepted:\n{report}"
        );
    }

    #[test]
    fn canonical_all_free_report_without_zero_class_rows_is_accepted() {
        let summary = memcheck_summary(&all_free_report(false), "synthetic strict Memcheck report");
        for expected in [
            "in use at exit: 0 bytes in 0 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost: 0 bytes in 0 blocks",
            "still reachable: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors",
        ] {
            assert!(summary.lines().any(|line| line == expected), "{summary}");
        }
    }

    #[test]
    fn full_zero_heap_summary_is_accepted() {
        memcheck_summary(&all_free_report(true), "synthetic strict Memcheck report");
    }

    #[test]
    fn malformed_duplicate_in_use_rows_are_rejected() {
        let report = report(&[
            "in use at exit: 0 bytes in 0 blocks",
            "warning: in use at exit: 9 bytes in 1 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ]);
        assert_rejected(&report);
    }

    #[test]
    fn partial_leak_summary_set_is_rejected() {
        let report = report(&[
            "in use at exit: 0 bytes in 0 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "definitely lost: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ]);
        assert_rejected(&report);
    }

    #[test]
    fn malformed_or_nonzero_suppressed_summary_is_rejected() {
        let malformed = report(&[
            "in use at exit: 0 bytes in 0 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "suppressed : 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ]);
        assert_rejected(&malformed);

        let nonzero = report(&[
            "in use at exit: 0 bytes in 0 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "suppressed: 8 bytes in 1 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ]);
        assert_rejected(&nonzero);
    }
}

fn assert_strict_memcheck(binary: &Path, evidence_directory: &Path) {
    write_new(
        &evidence_directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code=0\nexpected_stdout_bytes={}\n",
            EXPECTED_STDOUT.len()
        )
        .as_bytes(),
    );
    write_new(
        &evidence_directory.join("expected-native-stdout.bin"),
        EXPECTED_STDOUT,
    );
    let isolated_home = evidence_directory.join("valgrind-home");
    fs::create_dir(&isolated_home).expect("create isolated Valgrind HOME");
    assert!(
        !evidence_directory.join(".valgrindrc").exists(),
        "fresh Valgrind working directory unexpectedly contains .valgrindrc"
    );
    assert!(
        !isolated_home.join(".valgrindrc").exists(),
        "fresh Valgrind HOME unexpectedly contains .valgrindrc"
    );
    let isolated_home_text = isolated_home
        .to_str()
        .expect("Valgrind HOME path must be UTF-8 for command evidence");
    let valgrind_environment = [("HOME", isolated_home_text), ("VALGRIND_OPTS", "<unset>")];
    write_new(
        &evidence_directory.join("valgrind-config-isolation.txt"),
        format!(
            "VALGRIND_OPTS=<unset>\nVALGRIND_*_overrides=<unset>\nHOME={isolated_home_text}\nHOME/.valgrindrc=absent-at-start\nworking_directory={}\nworking_directory/.valgrindrc=absent-at-start\nexternal_valgrindrc=isolated\n",
            evidence_directory.display()
        )
        .as_bytes(),
    );
    let valgrind_log = evidence_directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--default-suppressions=yes"),
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible,reachable"),
        OsString::from("--error-exitcode=97"),
        OsString::from(format!("--log-file={}", valgrind_log.display())),
        binary.as_os_str().to_owned(),
    ];
    let output = run_recorded_with_cwd(
        OsStr::new("valgrind"),
        &arguments,
        &valgrind_environment,
        evidence_directory,
        Some(evidence_directory),
        true,
    );
    let report = fs::read(&valgrind_log).unwrap_or_else(|error| {
        panic!("read raw Valgrind log {}: {error}", valgrind_log.display())
    });
    write_new(
        &evidence_directory.join("memcheck-summary.txt"),
        memcheck_summary(&text(&report), "strict native Memcheck").as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "Valgrind changed the expected native result or found an error:\n{}\n{}",
        text(&report),
        text(&output.stderr)
    );
    assert_eq!(output.stdout.as_slice(), EXPECTED_STDOUT);
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-map-v2-computed-key-get-l3-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated ComputedKeyGet L3 directory");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct EvidenceDirectory {
    root: PathBuf,
    native: PathBuf,
    _temporary: Option<TemporaryDirectory>,
}

impl EvidenceDirectory {
    fn new() -> Self {
        let (root, temporary) = if let Some(configured) =
            std::env::var_os("MIMI_MAP_V2_COMPUTED_KEY_GET_L3_ARTIFACT_DIR")
                .filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let root = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for ComputedKeyGet artifacts")
                    .join(configured)
            };
            if !root.exists() {
                if let Some(parent) = root.parent() {
                    fs::create_dir_all(parent)
                        .expect("create parent for ComputedKeyGet artifact directory");
                }
                fs::create_dir(&root).expect("create fresh ComputedKeyGet artifact directory");
            }
            (
                fs::canonicalize(&root).expect("canonicalize ComputedKeyGet artifact directory"),
                None,
            )
        } else {
            let temporary = TemporaryDirectory::new();
            (temporary.0.clone(), Some(temporary))
        };
        let native = root.join("native");
        create_capture_directory(&native);
        Self {
            root,
            native,
            _temporary: temporary,
        }
    }
}

fn stdlib_environment() -> [(&'static str, &'static str); 1] {
    [("MIMI_STDLIB", concat!(env!("CARGO_MANIFEST_DIR"), "/std"))]
}

fn cli_environment() -> [(&'static str, &'static str); 3] {
    [
        ("MIMI_ROUTE_CENSUS", "1"),
        ("MIMI_VERBOSE", "1"),
        ("MIMI_STDLIB", concat!(env!("CARGO_MANIFEST_DIR"), "/std")),
    ]
}

fn assert_successful_run(
    output: &Output,
    direct: bool,
    digest: &str,
    mode: &str,
) -> (String, String, String) {
    assert_success(output, mode);
    assert_eq!(
        output.stdout.as_slice(),
        EXPECTED_STDOUT,
        "{mode} output differs from the ComputedKeyGet fixture contract"
    );
    let stderr = text(&output.stderr);
    let route = assert_route_admission(
        &stderr,
        "run",
        if direct { "explicit-mir" } else { "default" },
        digest,
        mode,
    );
    if direct {
        assert_explicit_run_profile(&stderr, digest, mode);
    } else {
        assert_default_profile(&stderr, digest, mode);
    }
    route
}

fn write_near_miss_source(
    original: &str,
    directory: &Path,
    label: &str,
    substitutions: &[(&str, &str)],
) -> PathBuf {
    let mut altered = original.to_string();
    for (needle, replacement) in substitutions {
        assert_eq!(
            altered.matches(needle).count(),
            1,
            "ComputedKeyGet fixture no longer has the unique {needle:?} projection"
        );
        altered = altered.replacen(needle, replacement, 1);
    }
    let path = directory.join(format!("{label}.mimi"));
    write_new(&path, altered.as_bytes());
    path
}

#[derive(Default)]
struct NegativeCaseAccounting {
    receipt_checker_rejection_count: usize,
    route_attempt_count: usize,
}

fn assert_checker_receipt_rejection(output: &Output, mode: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{mode} invalid Map/Any unpack must fail with exit code 1: {}",
        text(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "{mode} emitted a partial MIR receipt despite Checker rejection: {}",
        text(&output.stdout)
    );
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("[E0200]")
            && stderr.contains("Map/Any unpack requires one exact same-owner GetAny receipt"),
        "{mode} did not report the exact Checker Map/Any unpack rejection: {stderr}"
    );
    assert!(
        !stderr.contains(MIR_RECEIPT_HEADER),
        "{mode} emitted a MIR receipt header after Checker rejection: {stderr}"
    );
}

fn negative_case(mimi: &Path, native: &Path, source: &Path, label: &str) -> NegativeCaseAccounting {
    let case_directory = native.join(format!("negative/{label}"));
    fs::create_dir_all(&case_directory)
        .unwrap_or_else(|error| panic!("create negative case directory: {error}"));
    let source_bytes = fs::read(source)
        .unwrap_or_else(|error| panic!("read negative fixture {}: {error}", source.display()));
    write_new(&case_directory.join("source.fixture.mimi"), &source_bytes);

    let receipt_directory = case_directory.join("receipt");
    create_capture_directory(&receipt_directory);
    let receipt_arguments = vec![
        OsString::from("mir"),
        OsString::from("--all"),
        OsString::from("--receipt"),
        source.as_os_str().to_owned(),
    ];
    let receipt = run_mimi(
        mimi,
        &receipt_arguments,
        &stdlib_environment(),
        &receipt_directory,
    );
    assert_checker_receipt_rejection(&receipt, &format!("{label} read-only MIR receipt"));
    write_new(
        &case_directory.join("receipt-outcome.txt"),
        b"receipt_outcome=fail-closed-checker-e0200\nreceipt_exit_code=1\n",
    );
    let mut accounting = NegativeCaseAccounting {
        receipt_checker_rejection_count: 1,
        route_attempt_count: 0,
    };

    let environment = cli_environment();
    for direct in [false, true] {
        let mode = if direct {
            "explicit-mir-run"
        } else {
            "default-run"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("run")];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        arguments.push(source.as_os_str().to_owned());
        let output = run_mimi(mimi, &arguments, &environment, &directory);
        assert_fail_closed(&output, &format!("{label} {mode}"), "E0200");
        accounting.route_attempt_count += 1;
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-mir-build"
        } else {
            "default-build"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let binary = directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(mimi, &arguments, &environment, &directory);
        assert_fail_closed(&output, &format!("{label} {mode}"), "E0200");
        assert!(
            !binary.exists(),
            "{label} {mode} left a native binary after rejecting the route"
        );
        accounting.route_attempt_count += 1;
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-mir-verify"
        } else {
            "default-verify"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("verify"), source.as_os_str().to_owned()];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(mimi, &arguments, &environment, &directory);
        assert_fail_closed(&output, &format!("{label} {mode}"), "E0200");
        accounting.route_attempt_count += 1;
    }
    assert_eq!(accounting.route_attempt_count, NEGATIVE_COMMANDS_PER_CASE);
    accounting
}

fn positive_case(mimi: &Path, source: &Path, case_directory: &Path, label: &str) {
    create_capture_directory(case_directory);
    let source_bytes = fs::read(source).unwrap_or_else(|error| {
        panic!("read ComputedKeyGet fixture {}: {error}", source.display())
    });
    let captured_source = case_directory.join("source.fixture.mimi");
    write_new(&captured_source, &source_bytes);
    assert_eq!(
        fs::read(&captured_source).expect("read captured ComputedKeyGet fixture bytes"),
        source_bytes,
        "captured fixture bytes changed while preparing source-hash evidence"
    );
    let verifier_source_hash = blake3::hash(&source_bytes).to_hex().to_string();
    let fixture_source_sha256 = retain_sha256(
        &captured_source,
        "fixture-source",
        "captured fixture bytes",
        case_directory,
    );
    write_new(
        &case_directory.join("source-hash.expected.txt"),
        format!(
            "verifier_source_hash_algorithm=blake3\nverifier_source_hash={verifier_source_hash}\nfixture_source_sha256={fixture_source_sha256}\n"
        )
        .as_bytes(),
    );

    let receipt_directory = case_directory.join("receipt");
    create_capture_directory(&receipt_directory);
    let receipt_arguments = vec![
        OsString::from("mir"),
        OsString::from("--all"),
        OsString::from("--receipt"),
        source.as_os_str().to_owned(),
    ];
    let receipt = run_mimi(
        mimi,
        &receipt_arguments,
        &stdlib_environment(),
        &receipt_directory,
    );
    let receipt_fields = parse_mir_receipt(&receipt, &format!("{label} mimi mir --all --receipt"));
    let digest = receipt_fields["mir_digest"].clone();
    let mut observed_routes = Vec::with_capacity(6);

    for direct in [false, true] {
        let mode = if direct {
            "explicit-run"
        } else {
            "default-run"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("run")];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        arguments.push(source.as_os_str().to_owned());
        let output = run_mimi(mimi, &arguments, &cli_environment(), &directory);
        let (route_profile, route_digest, route_identity_digest) =
            assert_successful_run(&output, direct, &digest, mode);
        observed_routes.push((
            if direct {
                "explicit-mir-run"
            } else {
                "default-run"
            }
            .to_string(),
            route_profile,
            route_digest,
            route_identity_digest.clone(),
        ));
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-build"
        } else {
            "default-build"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let binary = directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(mimi, &arguments, &cli_environment(), &directory);
        assert_success(&output, mode);
        assert!(binary.is_file(), "{mode} did not produce a native binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(&binary)
                    .expect("inspect ComputedKeyGet native binary")
                    .permissions()
                    .mode()
                    & 0o111,
                0,
                "{mode} binary is not executable"
            );
        }
        let stderr = text(&output.stderr);
        let (route_profile, route_digest, route_identity_digest) = assert_route_admission(
            &stderr,
            "build",
            if direct { "explicit-mir" } else { "default" },
            &digest,
            mode,
        );
        observed_routes.push((
            if direct {
                "explicit-mir-build"
            } else {
                "default-build"
            }
            .to_string(),
            route_profile,
            route_digest,
            route_identity_digest.clone(),
        ));
        assert_build_receipt(
            &stderr,
            if direct { "explicit-mir" } else { "default" },
            &receipt_fields,
            &route_identity_digest,
            mode,
        );
        assert_native_census(&stderr, &digest, mode);
        if !direct {
            assert_default_profile(&stderr, &digest, mode);
        }
        retain_sha256(&binary, "program", "mimi build --output", &directory);

        let memcheck_directory = case_directory.join(if direct {
            "explicit-memcheck"
        } else {
            "default-memcheck"
        });
        create_capture_directory(&memcheck_directory);
        assert_strict_memcheck(&binary, &memcheck_directory);
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-verify"
        } else {
            "default-verify"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("verify"), source.as_os_str().to_owned()];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(mimi, &arguments, &cli_environment(), &directory);
        assert_success(&output, mode);
        assert!(
            text(&output.stdout).contains("No contracts to verify"),
            "{mode} did not preserve the no-contract verification result: {}",
            text(&output.stdout)
        );
        let stderr = text(&output.stderr);
        let (route_profile, route_digest, route_identity_digest) = assert_route_admission(
            &stderr,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            &digest,
            mode,
        );
        observed_routes.push((
            if direct {
                "explicit-mir-verify"
            } else {
                "default-verify"
            }
            .to_string(),
            route_profile,
            route_digest,
            route_identity_digest.clone(),
        ));
        if !direct {
            assert_default_profile(&stderr, &digest, mode);
        }
        assert_verifier_receipt(
            &stderr,
            &receipt_fields,
            &verifier_source_hash,
            &route_identity_digest,
            mode,
        );
        assert_no_legacy_fallback(&stderr, mode);
    }

    const EXPECTED_ROUTE_PATHS: [&str; 6] = [
        "default-run",
        "explicit-mir-run",
        "default-build",
        "explicit-mir-build",
        "default-verify",
        "explicit-mir-verify",
    ];
    assert_eq!(observed_routes.len(), EXPECTED_ROUTE_PATHS.len());
    let expected_route_identity_digest = &observed_routes[0].3;
    for ((path, profile, route_digest, route_identity_digest), expected_path) in
        observed_routes.iter().zip(EXPECTED_ROUTE_PATHS)
    {
        assert_eq!(
            path, expected_path,
            "ComputedKeyGet route order or set changed"
        );
        assert_eq!(profile, ROUTE_PROFILE, "{path} route profile changed");
        assert_eq!(
            route_digest, &digest,
            "{path} route digest differs from the read-only source receipt"
        );
        assert_eq!(
            route_identity_digest, expected_route_identity_digest,
            "{path} route receipt identity changed across consumers or invocation modes"
        );
    }
    let route_profile = observed_routes[0].1.clone();
    write_new(
        &case_directory.join("run-summary.txt"),
        format!(
            "case_label={label}\nfixture_path={}\nroute_profile={route_profile}\nroute_receipt_identity_digest={expected_route_identity_digest}\nglobal_map_any_v2_production_ready=false\ncli_receipt_profile={}\nreceipt_schema={}\nmir_digest={}\ntype_desc_digest={}\nabi_digest={}\nffi_digest={}\nownership_digest={}\nflow_transition_digest={}\nroot_owners={}\nsource_sha256={fixture_source_sha256}\nverifier_source_hash_blake3={verifier_source_hash}\ncase_count=1\ncli_receipt_count=1\nroute_record_count=6\ndefault_run_count=1\nexplicit_mir_run_count=1\nnative_build_count=2\nmemcheck_run_count=2\ndefault_native_build_receipt_count=1\nexplicit_native_build_receipt_count=1\ndefault_verify_count=1\nexplicit_mir_verify_count=1\nlegacy_fallback_count=0\nexpected_stdout_hex=\n",
            source.display(),
            receipt_fields["profile"],
            receipt_fields["schema"],
            receipt_fields["mir_digest"],
            receipt_fields["type_desc_digest"],
            receipt_fields["abi_digest"],
            receipt_fields["ffi_digest"],
            receipt_fields["ownership_digest"],
            receipt_fields["flow_transition_digest"],
            receipt_fields["root_owners"],
        )
        .as_bytes(),
    );
}

#[test]
#[ignore = "strict ComputedKeyGet native Memcheck L3; run scripts/verify-ordinary-map-v2-computed-key-get-l3.sh"]
fn ordinary_map_owned_any_v2_computed_key_get_cli_and_native_memcheck_are_strict() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the ComputedKeyGet slice must not open global Map/Any V2 readiness"
        );
    };

    let mimi = mimi_bin();
    let evidence = EvidenceDirectory::new();
    retain_sha256(&mimi, "mimi-binary", "CARGO_BIN_EXE_mimi", &evidence.native);

    let version_directory = evidence.native.join("valgrind-version");
    create_capture_directory(&version_directory);
    let version = run_recorded(
        OsStr::new("valgrind"),
        &[OsString::from("--version")],
        &[],
        &version_directory,
    );
    assert_success(&version, "Valgrind version query");

    let positive = evidence.native.join("positive");
    create_capture_directory(&positive);
    let positive_fixtures = [
        (
            "computed-key-get",
            "mir_ordinary_map_any_owner_v2_computed_key_get.mimi",
        ),
        (
            "computed-key-get-string",
            "mir_ordinary_map_any_owner_v2_computed_key_get_string.mimi",
        ),
    ];
    for (label, fixture_name) in positive_fixtures {
        let source = fixture(fixture_name);
        positive_case(&mimi, &source, &positive.join(label), label);
    }

    let source = fixture("mir_ordinary_map_any_owner_v2_computed_key_get.mimi");
    let source_bytes = fs::read(&source).unwrap_or_else(|error| {
        panic!(
            "read ComputedKeyGet negative-case fixture {}: {error}",
            source.display()
        )
    });
    let original = text(&source_bytes);
    let negative_inputs = evidence.native.join("negative-inputs");
    fs::create_dir_all(&negative_inputs).expect("create ComputedKeyGet near-miss inputs");
    let mut negative_accounting = NegativeCaseAccounting::default();
    for (label, substitutions) in [
        (
            "wrong-computed-key",
            &[(
                "let pair = map_get(root, key)",
                "let pair = map_get(root, key + \"?\")",
            )][..],
        ),
        (
            "wrong-dynamic-any-unpack-type",
            &[
                (
                    "let payload = unpack_i32(pair.1)",
                    "let payload = unpack_string(pair.1)",
                ),
                ("payload - 41", "drop(payload)\n    0"),
            ][..],
        ),
    ] {
        let altered_source =
            write_near_miss_source(&original, &negative_inputs, label, substitutions);
        let case_accounting = negative_case(&mimi, &evidence.native, &altered_source, label);
        negative_accounting.receipt_checker_rejection_count +=
            case_accounting.receipt_checker_rejection_count;
        negative_accounting.route_attempt_count += case_accounting.route_attempt_count;
    }
    assert_eq!(negative_accounting.receipt_checker_rejection_count, 2);
    assert_eq!(
        negative_accounting.route_attempt_count,
        2 * NEGATIVE_COMMANDS_PER_CASE
    );

    let positive_case_count = positive_fixtures.len();
    let route_count = positive_case_count * 6;
    let native_count = positive_case_count * 2;
    write_new(
        &positive.join("run-summary.txt"),
        format!(
            "route_profile={ROUTE_PROFILE}\nglobal_map_any_v2_production_ready=false\ncase_count={positive_case_count}\ncase_labels=computed-key-get,computed-key-get-string\ncli_receipt_count={positive_case_count}\nroute_record_count={route_count}\ndefault_run_count={positive_case_count}\nexplicit_mir_run_count={positive_case_count}\nnative_build_count={native_count}\nmemcheck_run_count={native_count}\ndefault_native_build_receipt_count={positive_case_count}\nexplicit_native_build_receipt_count={positive_case_count}\ndefault_verify_count={positive_case_count}\nexplicit_mir_verify_count={positive_case_count}\nnegative_case_count=2\nnegative_receipt_attempt_count=2\nnegative_receipt_checker_rejection_count={}\nnegative_route_attempt_count={}\nlegacy_fallback_count=0\nexpected_stdout_hex=\n",
            negative_accounting.receipt_checker_rejection_count,
            negative_accounting.route_attempt_count,
        )
        .as_bytes(),
    );
    assert!(evidence.root.is_dir());
}
