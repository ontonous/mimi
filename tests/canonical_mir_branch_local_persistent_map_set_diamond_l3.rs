use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const ROUTE_PROFILE: &str = "map-any-owner-v2-branch-local-set-diamond-v1";
const ARTIFACT_DIR_ENV: &str = "MIMI_BRANCH_LOCAL_SET_DIAMOND_L3_ARTIFACT_DIR";
const TRUE_FIXTURE: &str = "mir_branch_local_persistent_map_set_diamond_true.mimi";
const FALSE_FIXTURE: &str = "mir_branch_local_persistent_map_set_diamond_false.mimi";
const EXPECTED_STDOUT: &[u8] = b"";

struct EvidenceRoot {
    path: PathBuf,
    temporary: bool,
}

impl EvidenceRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let run_name = format!("run-{label}-{}-{stamp}", std::process::id());
        let temporary = std::env::var_os(ARTIFACT_DIR_ENV)
            .filter(|value| !value.is_empty())
            .is_none();
        let path = if let Some(configured) =
            std::env::var_os(ARTIFACT_DIR_ENV).filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let parent = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for L3 artifacts")
                    .join(configured)
            };
            fs::create_dir_all(&parent).unwrap_or_else(|error| {
                panic!("create L3 artifact parent {}: {error}", parent.display())
            });
            parent.join(run_name)
        } else {
            std::env::temp_dir().join(format!("mimi-branch-local-set-diamond-{run_name}"))
        };
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create fresh L3 evidence directory {} without replacing prior evidence: {error}",
                path.display()
            )
        });
        Self { path, temporary }
    }

    fn child_dir(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.path.join(relative);
        fs::create_dir_all(&path).unwrap_or_else(|error| {
            panic!("create evidence directory {}: {error}", path.display())
        });
        path
    }
}

impl Drop for EvidenceRoot {
    fn drop(&mut self) {
        if self.temporary {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug)]
struct RouteEvidence {
    fixture: &'static str,
    mode: &'static str,
    binary: PathBuf,
    mir_digest: String,
    receipt_identity_digest: String,
}

#[derive(Debug)]
struct BytecodeEvidence {
    mir_digest: String,
    receipt_identity_digest: String,
}

#[derive(Debug)]
struct VerifyEvidence {
    mir_digest: String,
    receipt_identity_digest: String,
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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

fn record_command(
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
    let mut environment_rows = environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    if program == OsStr::new("valgrind") {
        environment_rows.extend([
            "VALGRIND_OPTS=<removed>".to_owned(),
            "VALGRIND_LIB=<removed>".to_owned(),
        ]);
    }
    environment_rows.push(String::new());
    write_new(
        &directory.join("command.environment.txt"),
        environment_rows.join("\n").as_bytes(),
    );
    write_new(&directory.join("stdout.bin"), &output.stdout);
    write_new(&directory.join("stderr.bin"), &output.stderr);
    write_new(
        &directory.join("process.status"),
        process_status(output).as_bytes(),
    );
}

fn run_recorded(
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
) -> Output {
    fs::create_dir_all(directory)
        .unwrap_or_else(|error| panic!("create command evidence {}: {error}", directory.display()));
    let mut command = Command::new(program);
    command.args(arguments);
    for (key, value) in environment {
        command.env(key, value);
    }
    if program == OsStr::new("valgrind") {
        command.env_remove("VALGRIND_OPTS");
        command.env_remove("VALGRIND_LIB");
    }
    match command.output() {
        Ok(output) => {
            record_command(directory, program, arguments, environment, &output);
            output
        }
        Err(error) => {
            write_new(
                &directory.join("command.argv.json"),
                command_json(program, arguments).as_bytes(),
            );
            let mut environment_rows = environment
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>();
            if program == OsStr::new("valgrind") {
                environment_rows.extend([
                    "VALGRIND_OPTS=<removed>".to_owned(),
                    "VALGRIND_LIB=<removed>".to_owned(),
                ]);
            }
            environment_rows.push(String::new());
            write_new(
                &directory.join("command.environment.txt"),
                environment_rows.join("\n").as_bytes(),
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
            panic!("invoke {}: {error}", program.to_string_lossy());
        }
    }
}

fn assert_success(output: &Output, mode: &str) {
    assert!(
        output.status.success(),
        "{mode} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn assert_native_binary_is_ignored(binary: &Path, evidence: &EvidenceRoot, mode: &str) {
    let directory = evidence.child_dir(
        Path::new("gitignore-checks").join(mode).join(
            binary
                .parent()
                .and_then(Path::file_name)
                .unwrap_or_else(|| OsStr::new("unknown")),
        ),
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Ok(relative) = binary.strip_prefix(root) else {
        write_new(
            &directory.join("result.txt"),
            format!("outside_repository_not_staged={}\n", binary.display()).as_bytes(),
        );
        return;
    };
    let arguments = vec![
        OsString::from("check-ignore"),
        OsString::from("-v"),
        OsString::from("--"),
        relative.as_os_str().to_owned(),
    ];
    let output = run_recorded(
        OsStr::new("git"),
        &arguments,
        &[],
        &directory.join("git-check-ignore"),
    );
    assert_success(&output, &format!("{mode} native binary gitignore check"));
}

fn parse_fields(line: &str, mode: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for token in line.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        assert!(
            fields.insert(key.to_owned(), value.to_owned()).is_none(),
            "{mode} repeats receipt field {key}"
        );
    }
    fields
}

fn semantic_identity_digest_from_manifest(fields: &BTreeMap<String, String>) -> String {
    let mut framed = String::from("mimi-proof-mir-route-cache-v1");
    let mut frame = |value: &str| {
        use std::fmt::Write as _;
        write!(framed, "\n{}:", value.len()).expect("writing a String cannot fail");
        framed.push_str(value);
    };
    for key in [
        "schema",
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
    ] {
        frame(
            fields
                .get(key)
                .unwrap_or_else(|| panic!("native receipt manifest omitted {key}")),
        );
    }
    let root_owners = fields
        .get("root_owners")
        .unwrap_or_else(|| panic!("native receipt manifest omitted root_owners"));
    for owner in root_owners.split(',').filter(|owner| !owner.is_empty()) {
        frame(owner);
    }
    blake3::hash(framed.as_bytes()).to_hex().to_string()
}

fn route_identity_digest_from_manifest(fields: &BTreeMap<String, String>) -> String {
    let semantic = semantic_identity_digest_from_manifest(fields);
    let profile = fields
        .get("profile")
        .unwrap_or_else(|| panic!("native receipt manifest omitted profile"));
    let mut framed = String::from("mimi-proof-mir-route-identity-v1");
    let mut frame = |value: &str| {
        use std::fmt::Write as _;
        write!(framed, "{}:", value.len()).expect("writing a String cannot fail");
        framed.push_str(value);
        framed.push('\n');
    };
    frame(profile);
    frame(&semantic);
    blake3::hash(framed.as_bytes()).to_hex().to_string()
}

fn one_line<'a>(stderr: &'a str, prefix: &str, mode: &str) -> &'a str {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} needs one {prefix:?} record: {stderr}"
    );
    lines[0]
}

fn assert_digest(value: &str, label: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} is not a lowercase SHA-256 digest: {value:?}"
    );
}

fn assert_route_receipts(stderr: &str, explicit_mir: bool, mode: &str) -> (String, String) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} selected a legacy route: {stderr}"
    );
    let admission = one_line(stderr, "MIMI_ROUTE_CENSUS route-admission ", mode);
    let admission_fields = parse_fields(admission, mode);
    assert_eq!(
        admission_fields.get("consumer").map(String::as_str),
        Some("build"),
        "{mode} route-admission consumer"
    );
    assert_eq!(
        admission_fields.get("mode").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{mode} route-admission mode"
    );
    assert_eq!(
        admission_fields.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} Checker profile"
    );
    let mir_digest = admission_fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{mode} route-admission omitted mir_digest"));
    let receipt_identity_digest = admission_fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{mode} route-admission omitted receipt identity"));
    assert_digest(mir_digest, mode);
    assert_digest(receipt_identity_digest, mode);

    let native_build = one_line(stderr, "MIMI_ROUTE_CENSUS canonical-native-build ", mode);
    let native_fields = parse_fields(native_build, mode);
    assert_eq!(
        native_fields.get("consumer").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{mode} native build receipt consumer"
    );
    assert_eq!(
        native_fields.get("route_id").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} native build route id"
    );
    assert_eq!(
        native_fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} native build receipt profile"
    );
    assert_eq!(
        native_fields.get("mir_digest"),
        Some(mir_digest),
        "{mode} native receipt is not bound to route-admission MIR"
    );
    assert_eq!(
        route_identity_digest_from_manifest(&native_fields).as_str(),
        receipt_identity_digest.as_str(),
        "{mode} native receipt manifest is not bound to route-admission identity"
    );
    if !explicit_mir {
        let disposition = format!(
            "canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest={mir_digest}"
        );
        assert_eq!(
            one_line(stderr, "canonical route disposition: canonical (", mode),
            disposition,
            "{mode} default route disposition"
        );
    }
    (mir_digest.clone(), receipt_identity_digest.clone())
}

fn run_bytecode_route(
    mimi: &Path,
    evidence: &EvidenceRoot,
    fixture_name: &'static str,
    branch: &'static str,
    explicit_mir: bool,
) -> BytecodeEvidence {
    let mode = if explicit_mir { "explicit" } else { "default" };
    let mut arguments = vec![
        OsString::from("run"),
        fixture_path(fixture_name).into_os_string(),
    ];
    if explicit_mir {
        arguments.push(OsString::from("--mir"));
    }
    let output = run_recorded(
        mimi.as_os_str(),
        &arguments,
        &[("MIMI_ROUTE_CENSUS", "1")],
        &evidence.child_dir(Path::new("bytecode").join(branch).join(mode)),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{branch} {mode} bytecode exit status; stderr={}",
        text(&output.stderr)
    );
    assert_eq!(
        output.stdout.as_slice(),
        EXPECTED_STDOUT,
        "{branch} {mode} bytecode stdout"
    );

    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{branch} {mode} bytecode selected a legacy route: {stderr}"
    );
    let admission = one_line(
        &stderr,
        "MIMI_ROUTE_CENSUS route-admission ",
        &format!("{branch} {mode} bytecode"),
    );
    let fields = parse_fields(admission, &format!("{branch} {mode} bytecode"));
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some("run"),
        "{branch} {mode} bytecode route consumer"
    );
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{branch} {mode} bytecode route mode"
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{branch} {mode} bytecode Checker profile"
    );
    let mir_digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{branch} {mode} bytecode omitted MIR digest"));
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{branch} {mode} bytecode omitted receipt identity"));
    assert_digest(mir_digest, &format!("{branch} {mode} bytecode"));
    assert_digest(
        receipt_identity_digest,
        &format!("{branch} {mode} bytecode"),
    );
    let disposition_prefix = if explicit_mir {
        "canonical route disposition: explicit canonical ("
    } else {
        "canonical route disposition: canonical ("
    };
    let disposition = one_line(
        &stderr,
        disposition_prefix,
        &format!("{branch} {mode} bytecode"),
    );
    assert_eq!(
        disposition,
        format!("{disposition_prefix}{ROUTE_PROFILE}) mir_digest={mir_digest}"),
        "{branch} {mode} bytecode disposition"
    );
    BytecodeEvidence {
        mir_digest: mir_digest.clone(),
        receipt_identity_digest: receipt_identity_digest.clone(),
    }
}

fn run_verify_route(
    mimi: &Path,
    evidence: &EvidenceRoot,
    fixture_name: &'static str,
    branch: &'static str,
    explicit_mir: bool,
) -> VerifyEvidence {
    let mode = if explicit_mir { "explicit" } else { "default" };
    let fixture = fixture_path(fixture_name);
    let mut arguments = vec![OsString::from("verify"), fixture.as_os_str().to_owned()];
    if explicit_mir {
        arguments.push(OsString::from("--mir"));
    }
    let label = format!("{branch} {mode} verify");
    let output = run_recorded(
        mimi.as_os_str(),
        &arguments,
        &[("MIMI_ROUTE_CENSUS", "1"), ("MIMI_VERBOSE", "1")],
        &evidence.child_dir(Path::new("verifier").join(branch).join(mode)),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} exit status; stdout={} stderr={}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        text(&output.stdout),
        format!("No contracts to verify in {}\n", fixture.display()),
        "{label} must preserve the empty proof request without emitting a verdict"
    );

    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy verifier route: {stderr}"
    );
    let admission = one_line(&stderr, "MIMI_ROUTE_CENSUS route-admission ", &label);
    let fields = parse_fields(admission, &label);
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some("verify"),
        "{label} route-admission consumer"
    );
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{label} route-admission mode"
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{label} Checker profile"
    );
    let mir_digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{label} route-admission omitted MIR digest"));
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{label} route-admission omitted receipt identity"));
    assert_digest(mir_digest, &label);
    assert_digest(receipt_identity_digest, &label);

    let provenance = one_line(&stderr, "canonical MIR verifier provenance: ", &label);
    let provenance_fields = parse_fields(provenance, &label);
    assert_eq!(
        provenance_fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{label} verifier provenance profile"
    );
    assert_eq!(
        provenance_fields.get("mir_digest"),
        Some(mir_digest),
        "{label} verifier proof provenance must come from the admitted MIR graph"
    );
    assert_eq!(
        provenance_fields.get("route_identity_digest"),
        Some(receipt_identity_digest),
        "{label} verifier provenance must retain the admitted full route identity"
    );
    for field in [
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
        "source_hash",
    ] {
        let value = provenance_fields
            .get(field)
            .unwrap_or_else(|| panic!("{label} verifier provenance omitted {field}"));
        assert_digest(value, &format!("{label} verifier {field}"));
    }
    VerifyEvidence {
        mir_digest: mir_digest.clone(),
        receipt_identity_digest: receipt_identity_digest.clone(),
    }
}

fn record_test_environment(evidence: &EvidenceRoot) {
    let mut rows = vec![
        "MIMI_ROUTE_CENSUS=1 (set on each native build, bytecode run, and verifier command)"
            .to_string(),
        "MIMI_VERBOSE=1 (set on each verifier command to expose consumed receipt provenance)"
            .to_string(),
    ];
    if let Some(value) = std::env::var_os("LLVM_SYS_181_PREFIX") {
        rows.push(format!("LLVM_SYS_181_PREFIX={}", value.to_string_lossy()));
    }
    if let Some(value) = std::env::var_os(ARTIFACT_DIR_ENV) {
        rows.push(format!("{ARTIFACT_DIR_ENV}={}", value.to_string_lossy()));
    }
    rows.push(format!("evidence_directory={}", evidence.path.display()));
    rows.push("strict_memcheck_default_suppressions=disabled".to_owned());
    rows.push("strict_memcheck_VALGRIND_OPTS=removed_from_invocations".to_owned());
    rows.push("strict_memcheck_VALGRIND_LIB=removed_from_invocations".to_owned());
    rows.push(String::new());
    write_new(
        &evidence.path.join("test-environment.txt"),
        rows.join("\n").as_bytes(),
    );
}

fn record_source_hashes(evidence: &EvidenceRoot) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let paths = [
        root.join("tests").join("fixtures").join(TRUE_FIXTURE),
        root.join("tests").join("fixtures").join(FALSE_FIXTURE),
        root.join("tests")
            .join("canonical_mir_branch_local_persistent_map_set_diamond_l3.rs"),
        root.join("scripts")
            .join("verify-branch-local-persistent-map-set-diamond-l3.sh"),
        root.join("tests")
            .join("support")
            .join("memcheck_summary.rs"),
    ];
    let arguments = paths
        .iter()
        .map(|path| path.as_os_str().to_owned())
        .collect::<Vec<_>>();
    let output = run_recorded(
        OsStr::new("sha256sum"),
        &arguments,
        &[],
        &evidence.child_dir("source-hashes-command"),
    );
    assert_success(&output, "sha256sum branch-local Set diamond sources");
    write_new(&evidence.path.join("source-hashes.sha256"), &output.stdout);
}

fn assert_fixture_pair_is_exact() {
    let true_source =
        fs::read_to_string(fixture_path(TRUE_FIXTURE)).expect("read true-branch fixture");
    let false_source =
        fs::read_to_string(fixture_path(FALSE_FIXTURE)).expect("read false-branch fixture");
    assert_eq!(
        true_source.replace("let flag = true", "let flag = false"),
        false_source,
        "the true and false fixtures must differ only at the flag binding"
    );
    for required in [
        "type Label { text: string }",
        "let payload = Label { text: \"shared\" }",
        "match map_new()",
        "Ok(base) =>",
        "let flag = true",
        "match map_set(base, \"left\", payload)",
        "match map_set(base, \"right\", payload)",
        "Ok(child) =>",
        "map_size(child)",
        "drop(child)",
        "drop(base)",
        "drop(payload)",
    ] {
        assert!(
            true_source.contains(required),
            "true fixture does not preserve required source shape {required:?}"
        );
    }
}

fn build_and_run_route(
    mimi: &Path,
    evidence: &EvidenceRoot,
    fixture_name: &'static str,
    branch: &'static str,
    explicit_mir: bool,
) -> RouteEvidence {
    let mode = if explicit_mir { "explicit" } else { "default" };
    let build_name = if explicit_mir {
        "explicit-build"
    } else {
        "default-build"
    };
    let run_name = if explicit_mir {
        "explicit-run"
    } else {
        "default-run"
    };
    let build_directory = evidence.child_dir(Path::new("native").join(branch).join(build_name));
    let binary = build_directory.join("program");
    let mut arguments = vec![
        OsString::from("build"),
        fixture_path(fixture_name).into_os_string(),
        OsString::from("--output"),
        binary.as_os_str().to_owned(),
    ];
    if explicit_mir {
        arguments.push(OsString::from("--mir"));
    }
    let build = run_recorded(
        mimi.as_os_str(),
        &arguments,
        &[("MIMI_ROUTE_CENSUS", "1")],
        &build_directory.join("build-command"),
    );
    assert_success(&build, &format!("{branch} {mode} native build"));
    assert!(
        binary.is_file(),
        "{branch} {mode} build did not create {}",
        binary.display()
    );
    assert_native_binary_is_ignored(&binary, evidence, &format!("{branch}-{mode}"));
    let (mir_digest, receipt_identity_digest) = assert_route_receipts(
        &text(&build.stderr),
        explicit_mir,
        &format!("{branch} {mode}"),
    );

    let run = run_recorded(
        binary.as_os_str(),
        &[],
        &[],
        &evidence.child_dir(Path::new("native").join(branch).join(run_name)),
    );
    assert_eq!(
        run.status.code(),
        Some(0),
        "{branch} {mode} native run exit status; stderr={}",
        text(&run.stderr)
    );
    assert_eq!(
        run.stdout.as_slice(),
        EXPECTED_STDOUT,
        "{branch} {mode} native stdout"
    );
    RouteEvidence {
        fixture: fixture_name,
        mode,
        binary,
        mir_digest,
        receipt_identity_digest,
    }
}

fn exercise_native_routes(label: &str) -> (EvidenceRoot, Vec<RouteEvidence>) {
    assert_fixture_pair_is_exact();
    let evidence = EvidenceRoot::new(label);
    record_test_environment(&evidence);
    record_source_hashes(&evidence);
    let mimi = mimi_bin();
    let mut routes = Vec::new();
    for (fixture_name, branch) in [(TRUE_FIXTURE, "case-true"), (FALSE_FIXTURE, "case-false")] {
        let default = build_and_run_route(&mimi, &evidence, fixture_name, branch, false);
        let explicit = build_and_run_route(&mimi, &evidence, fixture_name, branch, true);
        let default_bytecode = run_bytecode_route(&mimi, &evidence, fixture_name, branch, false);
        let explicit_bytecode = run_bytecode_route(&mimi, &evidence, fixture_name, branch, true);
        let default_verify = run_verify_route(&mimi, &evidence, fixture_name, branch, false);
        let explicit_verify = run_verify_route(&mimi, &evidence, fixture_name, branch, true);
        assert_eq!(
            default.mir_digest, explicit.mir_digest,
            "{branch} default and explicit MIR digest"
        );
        assert_eq!(
            default.receipt_identity_digest, explicit.receipt_identity_digest,
            "{branch} default and explicit route receipt identity"
        );
        assert_eq!(
            default.mir_digest, default_bytecode.mir_digest,
            "{branch} default native and bytecode MIR digest"
        );
        assert_eq!(
            default.receipt_identity_digest, default_bytecode.receipt_identity_digest,
            "{branch} default native and bytecode receipt identity"
        );
        assert_eq!(
            explicit.mir_digest, explicit_bytecode.mir_digest,
            "{branch} explicit native and bytecode MIR digest"
        );
        assert_eq!(
            explicit.receipt_identity_digest, explicit_bytecode.receipt_identity_digest,
            "{branch} explicit native and bytecode receipt identity"
        );
        assert_eq!(
            default_bytecode.mir_digest, explicit_bytecode.mir_digest,
            "{branch} default and explicit bytecode MIR digest"
        );
        assert_eq!(
            default_bytecode.receipt_identity_digest, explicit_bytecode.receipt_identity_digest,
            "{branch} default and explicit bytecode receipt identity"
        );
        assert_eq!(
            default.mir_digest, default_verify.mir_digest,
            "{branch} default native and verifier MIR digest"
        );
        assert_eq!(
            default.receipt_identity_digest, default_verify.receipt_identity_digest,
            "{branch} default native and verifier receipt identity"
        );
        assert_eq!(
            explicit.mir_digest, explicit_verify.mir_digest,
            "{branch} explicit native and verifier MIR digest"
        );
        assert_eq!(
            explicit.receipt_identity_digest, explicit_verify.receipt_identity_digest,
            "{branch} explicit native and verifier receipt identity"
        );
        assert_eq!(
            default_verify.mir_digest, explicit_verify.mir_digest,
            "{branch} default and explicit verifier MIR digest"
        );
        assert_eq!(
            default_verify.receipt_identity_digest, explicit_verify.receipt_identity_digest,
            "{branch} default and explicit verifier receipt identity"
        );
        routes.push(default);
        routes.push(explicit);
    }
    (evidence, routes)
}

fn run_strict_memcheck(route: &RouteEvidence, evidence: &EvidenceRoot) {
    let directory = evidence.child_dir(
        Path::new("memcheck")
            .join(route.fixture.trim_end_matches(".mimi"))
            .join(route.mode),
    );
    write_new(
        &directory.join("expected-result.txt"),
        b"expected_exit_code=0\nexpected_stdout_bytes=0\n",
    );
    let log_file = directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--leak-check=full"),
        OsString::from("--default-suppressions=no"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=97"),
        OsString::from(format!("--log-file={}", log_file.display())),
        route.binary.as_os_str().to_owned(),
    ];
    let output = run_recorded(
        OsStr::new("valgrind"),
        &arguments,
        &[],
        &directory.join("valgrind-command"),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{} {} strict Memcheck changed the program result or found an error: {}",
        route.fixture,
        route.mode,
        text(&output.stderr)
    );
    assert_eq!(
        output.stdout.as_slice(),
        EXPECTED_STDOUT,
        "strict Memcheck stdout"
    );
    let report = fs::read_to_string(&log_file)
        .unwrap_or_else(|error| panic!("read Memcheck report {}: {error}", log_file.display()));
    let summary = memcheck_summary::parse(&report);
    write_new(
        &directory.join("memcheck-summary.txt"),
        summary.rendered.as_bytes(),
    );
    assert!(
        summary.rendered.contains("ERROR SUMMARY: 0 errors"),
        "strict Memcheck reported errors:\n{report}"
    );
    for kind in ["definitely lost", "indirectly lost", "possibly lost"] {
        assert!(
            summary
                .rendered
                .lines()
                .any(|line| line.contains(&format!("{kind}: 0 bytes in 0 blocks"))),
            "strict Memcheck reported {kind} allocations:\n{report}"
        );
    }
}

#[test]
fn branch_local_persistent_set_diamond_default_and_explicit_native_routes_pass() {
    let (_evidence, routes) = exercise_native_routes("native-routes");
    assert_eq!(routes.len(), 4);
}

#[test]
#[ignore = "strict L3 requires Valgrind; use scripts/verify-branch-local-persistent-map-set-diamond-l3.sh"]
fn branch_local_persistent_set_diamond_strict_memcheck_l3() {
    let (evidence, routes) = exercise_native_routes("strict-memcheck-l3");
    let version = run_recorded(
        OsStr::new("valgrind"),
        &[OsString::from("--version")],
        &[],
        &evidence.child_dir("valgrind-version"),
    );
    assert_success(&version, "Valgrind version");
    write_new(&evidence.path.join("valgrind-version.txt"), &version.stdout);
    for route in &routes {
        run_strict_memcheck(route, &evidence);
    }
    write_new(
        &evidence.path.join("strict-memcheck-result.txt"),
        b"status=passed\nroute_count=4\nmemcheck_error_count=0\nlost_bytes=0\nlost_blocks=0\n",
    );
}
