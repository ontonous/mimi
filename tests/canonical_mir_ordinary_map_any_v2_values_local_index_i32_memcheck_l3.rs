use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/memcheck_summary.rs"]
mod strict_memcheck_summary;

const ROUTE_PROFILE: &str = "map-any-owner-v2-values-local-index-i32-result-branch-v2";
const MIR_RECEIPT_PROFILE: &str = "cli-mir-v1";
const EXPECTED_STDOUT: &[u8] = b"";
const EXPECTED_PROGRAM_EXIT_CODE: i32 = 41;
const RECEIPT_FIELDS: &[&str] = &[
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
        .expect("Cargo must provide mimi for ValuesLocalIndexI32 production L3 evidence")
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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

fn retain_command_output(
    directory: &Path,
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
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
    let environment = environment
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect::<String>();
    write_new(
        &directory.join("command.environment.txt"),
        environment.as_bytes(),
    );
}

fn run_recorded(
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
) -> Output {
    let mut command = Command::new(program);
    command.args(arguments);
    for (key, value) in environment {
        command.env(key, value);
    }
    match command.output() {
        Ok(output) => {
            retain_command_output(directory, program, arguments, environment, &output);
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
            let environment = environment
                .iter()
                .map(|(key, value)| format!("{key}={value}\n"))
                .collect::<String>();
            write_new(
                &directory.join("command.environment.txt"),
                environment.as_bytes(),
            );
            panic!(
                "invoke {} for ValuesLocalIndexI32 L3 evidence: {error}",
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
    let fields = text(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.len(),
        RECEIPT_FIELDS.len(),
        "{mode} changed the canonical MIR receipt field set: {fields:?}"
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(MIR_RECEIPT_PROFILE),
        "{mode} changed the CLI receipt schema"
    );
    for field in RECEIPT_FIELDS {
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
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{mode} has malformed {field}: {digest}"
        );
    }
    assert!(
        fields
            .get("root_owners")
            .is_some_and(|owners| owners.split(',').any(|owner| owner == "function:main")),
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
    for field in RECEIPT_FIELDS {
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
        "{mode} did not select exactly the dedicated ValuesLocalIndexI32 route: {stderr}"
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
) -> String {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must report exactly one Checker route-admission record: {stderr}"
    );
    let fields = parse_key_values(
        lines[0]
            .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
            .expect("route-admission census has its stable prefix")
            .split_whitespace(),
        mode,
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
    fields
        .get("checker_profile")
        .expect("route-admission record has checker_profile")
        .clone()
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
    let fields = parse_key_values(tokens, mode);
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
    assert_receipt_identity(&fields, expected, mode);
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_verifier_receipt(stderr: &str, expected: &BTreeMap<String, String>, mode: &str) {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("canonical MIR verifier provenance: "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must expose the actual verifier receipt (Z3 must be available): {stderr}"
    );
    let fields = parse_key_values(
        lines[0]
            .strip_prefix("canonical MIR verifier provenance: ")
            .expect("verifier receipt has its prefix")
            .split_whitespace(),
        mode,
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} changed the verifier receipt profile"
    );
    for field in RECEIPT_DIGEST_FIELDS {
        assert_eq!(
            fields.get(*field),
            expected.get(*field),
            "{mode} changed MIR receipt identity field {field}"
        );
    }
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
        !diagnostic.contains("canonical route disposition: canonical (")
            && !diagnostic.contains("canonical route disposition: explicit canonical ("),
        "{mode} reported a selected canonical route despite rejecting the shape:\n{diagnostic}"
    );
}

fn assert_fail_closed(output: &Output, mode: &str) {
    assert!(
        !output.status.success(),
        "{mode} unexpectedly accepted a shape outside ValuesLocalIndexI32:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains("MIR-COVERAGE-001")
            || diagnostic.contains("not production-ready")
            || (diagnostic.contains("MIR-MATERIALIZATION-001")
                && diagnostic.contains("Canonical MIR route rejected:")
                && diagnostic.contains("index projection has no canonical ownership receipt"))
            || (diagnostic.contains("canonical MIR build error: MIR lowering failed")
                && diagnostic.contains("index projection has no canonical ownership receipt")),
        "{mode} did not fail at the canonical coverage/readiness boundary:\n{diagnostic}"
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
    assert!(
        report.ends_with('\n'),
        "{mode} Valgrind report is truncated or lacks its final newline"
    );
    let mut report_pid = None;
    for line in report.lines() {
        let (pid, body) = line
            .strip_prefix("==")
            .and_then(|remainder| remainder.split_once("=="))
            .unwrap_or_else(|| panic!("{mode} report has a line without a Valgrind PID: {line:?}"));
        assert!(
            !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
            "{mode} report has a malformed Valgrind PID: {line:?}"
        );
        if let Some(expected_pid) = report_pid {
            assert_eq!(pid, expected_pid, "{mode} report combines process IDs");
        } else {
            report_pid = Some(pid);
        }
        if body.trim_start().starts_with("suppressed:") {
            assert_eq!(
                body.trim(),
                "suppressed: 0 bytes in 0 blocks",
                "{mode} reports suppressed heap allocations: \n{report}"
            );
        }
    }
    assert!(report_pid.is_some(), "{mode} Valgrind report is empty");

    let summary = strict_memcheck_summary::parse(report);
    assert_eq!(
        summary.still_reachable_bytes, 0,
        "{mode} retained reachable heap bytes:\n{report}"
    );
    assert_eq!(
        summary.still_reachable_blocks, 0,
        "{mode} retained reachable heap blocks:\n{report}"
    );
    summary.rendered
}

fn assert_strict_memcheck(binary: &Path, evidence_directory: &Path) {
    write_new(
        &evidence_directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code={}\nexpected_stdout_bytes={}\n",
            EXPECTED_PROGRAM_EXIT_CODE,
            EXPECTED_STDOUT.len()
        )
        .as_bytes(),
    );
    write_new(
        &evidence_directory.join("expected-native-stdout.bin"),
        EXPECTED_STDOUT,
    );
    let valgrind_log = evidence_directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=97"),
        OsString::from(format!("--log-file={}", valgrind_log.display())),
        binary.as_os_str().to_owned(),
    ];
    let output = run_recorded(OsStr::new("valgrind"), &arguments, &[], evidence_directory);
    let report = fs::read(&valgrind_log).unwrap_or_else(|error| {
        panic!("read raw Valgrind log {}: {error}", valgrind_log.display())
    });
    write_new(
        &evidence_directory.join("memcheck-summary.txt"),
        memcheck_summary(&text(&report), "strict native Memcheck").as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(EXPECTED_PROGRAM_EXIT_CODE),
        "Valgrind must preserve the native payload exit code or found a Memcheck error:\n{}\n{}",
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
            "mimi-map-v2-values-local-index-i32-l3-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated ValuesLocalIndexI32 L3 directory");
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
            std::env::var_os("MIMI_MAP_V2_VALUES_LOCAL_INDEX_I32_L3_ARTIFACT_DIR")
                .filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let root = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for ValuesLocalIndexI32 artifacts")
                    .join(configured)
            };
            if !root.exists() {
                if let Some(parent) = root.parent() {
                    fs::create_dir_all(parent)
                        .expect("create parent for ValuesLocalIndexI32 artifact directory");
                }
                fs::create_dir(&root).expect("create fresh ValuesLocalIndexI32 artifact directory");
            }
            (
                fs::canonicalize(&root)
                    .expect("canonicalize ValuesLocalIndexI32 artifact directory"),
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

fn cli_environment() -> [(&'static str, &'static str); 2] {
    [("MIMI_ROUTE_CENSUS", "1"), ("MIMI_VERBOSE", "1")]
}

fn assert_successful_run(output: &Output, direct: bool, digest: &str, mode: &str) -> String {
    assert_eq!(
        output.status.code(),
        Some(EXPECTED_PROGRAM_EXIT_CODE),
        "{mode} must return the exact selected i32 payload as its exit code; stdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        output.stdout.as_slice(),
        EXPECTED_STDOUT,
        "{mode} stdout differs from the ValuesLocalIndexI32 fixture contract"
    );
    let stderr = text(&output.stderr);
    let profile = assert_route_admission(
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
    profile
}

fn negative_case(mimi: &Path, native: &Path, source: &Path, label: &str) {
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
    let receipt = run_mimi(mimi, &receipt_arguments, &[], &receipt_directory);
    if label == "altered-local-index-source" {
        assert!(
            !receipt.status.success(),
            "{label} unexpectedly materialized MIR without an exact local-index ownership receipt"
        );
        let diagnostic = format!("{}\n{}", text(&receipt.stdout), text(&receipt.stderr));
        assert!(
            diagnostic.contains("MIR lowering failed")
                && diagnostic.contains("index projection has no canonical ownership receipt"),
            "{label} MIR inspection did not fail at the expected ownership boundary:\n{diagnostic}"
        );
        assert_no_legacy_or_route_success(&receipt, &format!("{label} MIR inspection"));
    } else {
        let receipt_fields = parse_mir_receipt(&receipt, &format!("{label} read-only MIR receipt"));
        assert!(
            receipt_fields["root_owners"].contains("function:main"),
            "{label} receipt omitted the main owner"
        );
    }

    let environment = cli_environment();
    let mut command_count = 0;
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
        assert_fail_closed(&output, &format!("{label} {mode}"));
        command_count += 1;
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
        assert_fail_closed(&output, &format!("{label} {mode}"));
        assert!(
            !binary.exists(),
            "{label} {mode} left a native binary after rejecting the route"
        );
        command_count += 1;
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
        assert_fail_closed(&output, &format!("{label} {mode}"));
        command_count += 1;
    }
    assert_eq!(command_count, NEGATIVE_COMMANDS_PER_CASE);
}

#[test]
#[ignore = "strict ValuesLocalIndexI32 native Memcheck L3; run scripts/verify-ordinary-map-v2-values-local-index-i32-l3.sh"]
fn ordinary_map_owned_any_v2_values_local_index_i32_result_branch_cli_and_native_memcheck_are_strict(
) {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the ValuesLocalIndexI32 slice must not open global Map/Any V2 readiness"
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
    let source = fixture("mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi");
    let source_bytes = fs::read(&source).unwrap_or_else(|error| {
        panic!(
            "read ValuesLocalIndexI32 fixture {}: {error}",
            source.display()
        )
    });
    write_new(&positive.join("source.fixture.mimi"), &source_bytes);

    let receipt_directory = positive.join("receipt");
    create_capture_directory(&receipt_directory);
    let receipt_arguments = vec![
        OsString::from("mir"),
        OsString::from("--all"),
        OsString::from("--receipt"),
        source.as_os_str().to_owned(),
    ];
    let receipt = run_mimi(&mimi, &receipt_arguments, &[], &receipt_directory);
    let receipt_fields = parse_mir_receipt(&receipt, "mimi mir --all --receipt");
    let digest = receipt_fields["mir_digest"].clone();
    let fixture_sha256 = retain_sha256(
        &source,
        "fixture-source",
        "MimiLocalIndexI32 fixture",
        &positive,
    );
    let mut observed_route_profiles = Vec::with_capacity(6);

    for direct in [false, true] {
        let mode = if direct {
            "explicit-run"
        } else {
            "default-run"
        };
        let directory = positive.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("run")];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        arguments.push(source.as_os_str().to_owned());
        let output = run_mimi(&mimi, &arguments, &cli_environment(), &directory);
        observed_route_profiles.push(assert_successful_run(&output, direct, &digest, mode));
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-build"
        } else {
            "default-build"
        };
        let directory = positive.join(mode);
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
        let output = run_mimi(&mimi, &arguments, &cli_environment(), &directory);
        assert_success(&output, mode);
        assert!(binary.is_file(), "{mode} did not produce a native binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(&binary)
                    .expect("inspect ValuesLocalIndexI32 native binary")
                    .permissions()
                    .mode()
                    & 0o111,
                0,
                "{mode} binary is not executable"
            );
        }
        let stderr = text(&output.stderr);
        observed_route_profiles.push(assert_route_admission(
            &stderr,
            "build",
            if direct { "explicit-mir" } else { "default" },
            &digest,
            mode,
        ));
        assert_build_receipt(
            &stderr,
            if direct { "explicit-mir" } else { "default" },
            &receipt_fields,
            mode,
        );
        if !direct {
            assert_default_profile(&stderr, &digest, mode);
        }
        retain_sha256(&binary, "program", "mimi build --output", &directory);

        let memcheck_directory = positive.join(if direct {
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
        let directory = positive.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("verify"), source.as_os_str().to_owned()];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(&mimi, &arguments, &cli_environment(), &directory);
        assert_success(&output, mode);
        assert!(
            text(&output.stdout).contains("No contracts to verify"),
            "{mode} did not preserve the no-contract verification result: {}",
            text(&output.stdout)
        );
        let stderr = text(&output.stderr);
        observed_route_profiles.push(assert_route_admission(
            &stderr,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            &digest,
            mode,
        ));
        if !direct {
            assert_default_profile(&stderr, &digest, mode);
        }
        assert_verifier_receipt(&stderr, &receipt_fields, mode);
        assert_no_legacy_fallback(&stderr, mode);
    }

    let negative_values = fixture("mir_ordinary_map_any_owner_v2_values_unpack.mimi");
    negative_case(
        &mimi,
        &evidence.native,
        &negative_values,
        "broader-values-unpack",
    );

    let altered_directory = evidence.native.join("negative/altered-local-index");
    fs::create_dir_all(&altered_directory)
        .expect("create altered-index negative fixture directory");
    let original = text(&source_bytes);
    let needle = "let index: i32 = 0";
    assert_eq!(
        original.matches(needle).count(),
        1,
        "ValuesLocalIndexI32 fixture no longer has one immutable local initialized to index zero"
    );
    let altered_source_text = original.replacen(needle, "let index: i32 = 1", 1);
    let altered_source = altered_directory.join("altered-index.mimi");
    write_new(&altered_source, altered_source_text.as_bytes());
    negative_case(
        &mimi,
        &evidence.native,
        &altered_source,
        "altered-local-index-source",
    );

    assert_eq!(observed_route_profiles.len(), 6);
    assert!(
        observed_route_profiles
            .iter()
            .all(|profile| profile == ROUTE_PROFILE),
        "the six CLI consumers disagreed on the Checker route profile: {observed_route_profiles:?}"
    );
    let route_profile = observed_route_profiles[0].clone();
    let route_manifest = format!(
        "schema=mimi-ordinary-map-owned-any-v2-l3-route-manifest-v1\nroute_profile={route_profile}\nglobal_map_any_v2_production_ready=false\nfixture=tests/fixtures/mir_ordinary_map_any_owner_v2_values_local_index_i32_result_branch.mimi\nfixture_sha256={fixture_sha256}\ncli_receipt_profile={}\nreceipt_schema={}\nmir_digest={}\ntype_desc_digest={}\nabi_digest={}\nffi_digest={}\nownership_digest={}\nflow_transition_digest={}\nroot_owners={}\nconsumer_profiles={}\nexpected_program_exit_code={}\nexpected_stdout_hex={}\n",
        receipt_fields["profile"],
        receipt_fields["schema"],
        receipt_fields["mir_digest"],
        receipt_fields["type_desc_digest"],
        receipt_fields["abi_digest"],
        receipt_fields["ffi_digest"],
        receipt_fields["ownership_digest"],
        receipt_fields["flow_transition_digest"],
        receipt_fields["root_owners"],
        observed_route_profiles.join(","),
        EXPECTED_PROGRAM_EXIT_CODE,
        hex(EXPECTED_STDOUT),
    );
    write_new(
        &positive.join("route-manifest.txt"),
        route_manifest.as_bytes(),
    );
    write_new(
        &positive.join("run-summary.txt"),
        format!(
            "route_profile={route_profile}\nglobal_map_any_v2_production_ready=false\nfixture_sha256={fixture_sha256}\ncli_receipt_profile={}\nreceipt_schema={}\nmir_digest={}\ntype_desc_digest={}\nabi_digest={}\nffi_digest={}\nownership_digest={}\nflow_transition_digest={}\nroot_owners={}\ncase_count=1\ncli_receipt_count=1\ndefault_run_count=1\nexplicit_mir_run_count=1\nnative_build_count=2\nmemcheck_run_count=2\ndefault_native_build_receipt_count=1\nexplicit_native_build_receipt_count=1\ndefault_verify_count=1\nexplicit_mir_verify_count=1\nnegative_case_count=2\nnegative_route_attempt_count={}\nlegacy_fallback_count=0\nexpected_program_exit_code={}\nexpected_stdout_hex={}\n",
            receipt_fields["profile"],
            receipt_fields["schema"],
            receipt_fields["mir_digest"],
            receipt_fields["type_desc_digest"],
            receipt_fields["abi_digest"],
            receipt_fields["ffi_digest"],
            receipt_fields["ownership_digest"],
            receipt_fields["flow_transition_digest"],
            receipt_fields["root_owners"],
            2 * NEGATIVE_COMMANDS_PER_CASE,
            EXPECTED_PROGRAM_EXIT_CODE,
            hex(EXPECTED_STDOUT),
        )
        .as_bytes(),
    );
    assert!(evidence.root.is_dir());
}
