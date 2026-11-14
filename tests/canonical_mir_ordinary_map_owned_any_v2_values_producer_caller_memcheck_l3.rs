use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const ARTIFACT_DIR_ENV: &str = "MIMI_VALUES_PRODUCER_CALLER_L3_ARTIFACT_DIR";
const ROUTE_PROFILE: &str =
    "ordinary-map-owned-any-v2-values-result-producer-caller-v1-one-length-return";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const NATIVE_BUILD_PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";

struct RouteIdentity {
    mir_digest: String,
    receipt_identity_digest: String,
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn artifact_root() -> PathBuf {
    let configured = std::env::var_os(ARTIFACT_DIR_ENV)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{ARTIFACT_DIR_ENV} must name the fresh L3 evidence directory"));
    let path = PathBuf::from(configured);
    assert!(
        path.is_dir(),
        "the L3 runner must create the evidence root before the ignored test starts: {}",
        path.display()
    );
    path
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("hash source {}: {error}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        text(&output.stderr)
    );
    text(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("sha256sum returned no source digest for {}", path.display()))
        .to_owned()
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

fn retained_environment(environment: &[(String, String)], valgrind: bool) -> String {
    let mut rows = environment
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    rows.extend([
        "ambient_MIMI_*_variables=removed".to_owned(),
        "LD_PRELOAD=removed".to_owned(),
        "other_environment=inherited".to_owned(),
    ]);
    if valgrind {
        rows.extend([
            "ambient_VALGRIND_*_variables=removed".to_owned(),
            "VALGRIND_OPTS=removed".to_owned(),
            "VALGRIND_LIB=removed".to_owned(),
        ]);
    }
    rows.push(String::new());
    rows.join("\n")
}

fn retain_command(
    directory: &Path,
    program: &OsStr,
    arguments: &[OsString],
    cwd: &Path,
    environment: &[(String, String)],
    valgrind: bool,
    output: &Output,
) {
    write_new(
        &directory.join("command.argv.json"),
        command_json(program, arguments).as_bytes(),
    );
    write_new(
        &directory.join("command.cwd.txt"),
        format!("{}\n", cwd.display()).as_bytes(),
    );
    write_new(
        &directory.join("command.environment.txt"),
        retained_environment(environment, valgrind).as_bytes(),
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
    cwd: &Path,
    environment: &[(String, String)],
    directory: &Path,
    valgrind: bool,
) -> Output {
    fs::create_dir(directory).unwrap_or_else(|error| {
        panic!(
            "create fresh command capture directory {}: {error}",
            directory.display()
        )
    });
    let mut command = Command::new(program);
    command.args(arguments).current_dir(cwd);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("MIMI_") || (valgrind && name.starts_with("VALGRIND_")) {
            command.env_remove(key);
        }
    }
    command.env_remove("LD_PRELOAD");
    if valgrind {
        command
            .env_remove("VALGRIND_OPTS")
            .env_remove("VALGRIND_LIB");
    }
    for (key, value) in environment {
        command.env(key, value);
    }

    match command.output() {
        Ok(output) => {
            retain_command(
                directory,
                program,
                arguments,
                cwd,
                environment,
                valgrind,
                &output,
            );
            output
        }
        Err(error) => {
            write_new(
                &directory.join("command.argv.json"),
                command_json(program, arguments).as_bytes(),
            );
            write_new(
                &directory.join("command.cwd.txt"),
                format!("{}\n", cwd.display()).as_bytes(),
            );
            write_new(
                &directory.join("command.environment.txt"),
                retained_environment(environment, valgrind).as_bytes(),
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

fn assert_success(output: &Output, label: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn prefixed_line<'a>(stderr: &'a str, prefix: &str, label: &str) -> &'a str {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect::<Vec<_>>();
    let [line] = lines.as_slice() else {
        panic!(
            "{label} must emit exactly one record prefixed {prefix:?}; got {}:\n{stderr}",
            lines.len()
        );
    };
    line
}

fn fields<'a>(line: &'a str, prefix: &str, label: &str) -> BTreeMap<&'a str, &'a str> {
    let payload = line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{label} record is missing prefix {prefix:?}"));
    let mut result = BTreeMap::new();
    for field in payload.split_whitespace() {
        let (key, value) = field
            .split_once('=')
            .unwrap_or_else(|| panic!("{label} contains malformed route field {field:?}"));
        assert!(
            result.insert(key, value).is_none(),
            "{label} repeats route field {key}"
        );
    }
    result
}

fn assert_digest(value: &str, label: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} must be a lowercase SHA-256 digest, got {value:?}"
    );
}

fn route_identity(output: &Output, explicit_mir: bool, label: &str) -> RouteIdentity {
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback:\n{stderr}"
    );
    assert!(
        !stderr.contains("MIR-COVERAGE-001"),
        "{label} lost canonical route admission:\n{stderr}"
    );

    let admission = prefixed_line(&stderr, ROUTE_ADMISSION_PREFIX, label);
    let admission_fields = fields(admission, ROUTE_ADMISSION_PREFIX, label);
    let field_order = admission
        .strip_prefix(ROUTE_ADMISSION_PREFIX)
        .expect("route admission prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("route field key=value").0)
        .collect::<Vec<_>>();
    assert_eq!(
        field_order,
        [
            "consumer",
            "mode",
            "checker_profile",
            "mir_digest",
            "receipt_identity_digest",
        ],
        "{label} route census field contract changed"
    );
    assert_eq!(admission_fields.get("consumer"), Some(&"build"), "{label}");
    assert_eq!(
        admission_fields.get("mode"),
        Some(if explicit_mir {
            &"explicit-mir"
        } else {
            &"default"
        }),
        "{label}"
    );
    assert_eq!(
        admission_fields.get("checker_profile"),
        Some(&ROUTE_PROFILE),
        "{label} must use the dedicated producer/caller route"
    );

    let mir_digest = admission_fields
        .get("mir_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its MIR digest"));
    let receipt_identity_digest = admission_fields
        .get("receipt_identity_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its receipt identity digest"));
    assert_digest(mir_digest, &format!("{label} MIR digest"));
    assert_digest(
        receipt_identity_digest,
        &format!("{label} receipt identity digest"),
    );

    let native = prefixed_line(&stderr, NATIVE_BUILD_PREFIX, label);
    let native_fields = fields(native, NATIVE_BUILD_PREFIX, label);
    assert_eq!(
        native_fields.get("route_id"),
        Some(&ROUTE_PROFILE),
        "{label} native build must consume the producer/caller route"
    );
    assert_eq!(
        native_fields.get("profile"),
        Some(&ROUTE_PROFILE),
        "{label} native receipt profile"
    );
    assert_eq!(
        native_fields.get("consumer"),
        Some(if explicit_mir {
            &"explicit-mir"
        } else {
            &"default"
        }),
        "{label} native consumer mode"
    );
    assert_eq!(
        native_fields.get("mir_digest"),
        Some(&mir_digest),
        "{label} native build must consume the admitted MIR"
    );

    RouteIdentity {
        mir_digest: mir_digest.to_owned(),
        receipt_identity_digest: receipt_identity_digest.to_owned(),
    }
}

fn build_and_memcheck(
    root: &Path,
    source: &Path,
    case: &str,
    mode: &str,
    explicit_mir: bool,
    expected_stdout: &[u8],
) -> RouteIdentity {
    let mode_directory = root.join("native").join(case).join(mode);
    fs::create_dir_all(&mode_directory).unwrap_or_else(|error| {
        panic!(
            "create native evidence directory {}: {error}",
            mode_directory.display()
        )
    });
    let binary = mode_directory.join("program");
    let mut build_arguments = vec![OsString::from("build"), source.as_os_str().to_owned()];
    if explicit_mir {
        build_arguments.push(OsString::from("--mir"));
    }
    build_arguments.push(OsString::from("--output"));
    build_arguments.push(binary.as_os_str().to_owned());
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let build_environment = vec![
        ("MIMI_ROUTE_CENSUS".to_owned(), "1".to_owned()),
        ("MIMI_VERBOSE".to_owned(), "1".to_owned()),
        ("LC_ALL".to_owned(), "C".to_owned()),
    ];
    let build_output = run_recorded(
        mimi_bin().as_os_str(),
        &build_arguments,
        repo_root,
        &build_environment,
        &mode_directory.join("build-command"),
        false,
    );
    let label = format!("{case} {mode} native build");
    assert_success(&build_output, &label);
    assert!(
        binary.is_file(),
        "{label} did not create {}",
        binary.display()
    );
    let identity = route_identity(&build_output, explicit_mir, &label);

    write_new(&mode_directory.join("expected-stdout.bin"), expected_stdout);
    let hash_output = run_recorded(
        OsStr::new("sha256sum"),
        &[OsString::from("--"), binary.as_os_str().to_owned()],
        repo_root,
        &[("LC_ALL".to_owned(), "C".to_owned())],
        &mode_directory.join("binary-hash-command"),
        false,
    );
    assert_success(&hash_output, &format!("{label} SHA-256"));
    let hash_output_text = text(&hash_output.stdout);
    let hash = hash_output_text
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("{label} sha256sum returned no digest"));
    assert_digest(hash, &format!("{label} binary SHA-256"));
    write_new(
        &mode_directory.join("program.sha256"),
        format!("{hash}\n").as_bytes(),
    );

    let memcheck_directory = mode_directory.join("memcheck");
    fs::create_dir(&memcheck_directory).unwrap_or_else(|error| {
        panic!(
            "create fresh Memcheck evidence directory {}: {error}",
            memcheck_directory.display()
        )
    });
    let isolated_home = memcheck_directory.join("isolated-home");
    fs::create_dir(&isolated_home).unwrap_or_else(|error| {
        panic!(
            "create isolated Valgrind HOME {}: {error}",
            isolated_home.display()
        )
    });
    assert!(
        !isolated_home.join(".valgrindrc").exists(),
        "fresh isolated Valgrind HOME unexpectedly contains .valgrindrc"
    );
    let log_path = memcheck_directory.join("valgrind.log");
    let valgrind_arguments = vec![
        OsString::from("--command-line-only=yes"),
        OsString::from("--default-suppressions=no"),
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=97"),
        OsString::from(format!("--log-file={}", log_path.display())),
        binary.as_os_str().to_owned(),
    ];
    let home = isolated_home
        .to_str()
        .expect("isolated Valgrind HOME path must be UTF-8");
    let valgrind_environment = vec![
        ("HOME".to_owned(), home.to_owned()),
        ("LANG".to_owned(), "C".to_owned()),
        ("LC_ALL".to_owned(), "C".to_owned()),
    ];
    let valgrind_output = run_recorded(
        OsStr::new("valgrind"),
        &valgrind_arguments,
        &isolated_home,
        &valgrind_environment,
        &memcheck_directory.join("valgrind-command"),
        true,
    );
    let memcheck_label = format!("{case} {mode} strict Valgrind Memcheck");
    assert_success(&valgrind_output, &memcheck_label);
    assert_eq!(
        valgrind_output.stdout.as_slice(),
        expected_stdout,
        "{memcheck_label} stdout changed"
    );
    assert!(
        valgrind_output.stderr.is_empty(),
        "{memcheck_label} emitted application stderr:\n{}",
        text(&valgrind_output.stderr)
    );
    let report = fs::read_to_string(&log_path)
        .unwrap_or_else(|error| panic!("read raw Memcheck report {}: {error}", log_path.display()));
    let summary = memcheck_summary::parse(&report);
    for expected in [
        "definitely lost: 0 bytes in 0 blocks",
        "indirectly lost: 0 bytes in 0 blocks",
        "possibly lost: 0 bytes in 0 blocks",
        "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
    ] {
        assert!(
            summary.rendered.contains(expected),
            "{memcheck_label} omitted strict summary item {expected:?}:\n{}",
            summary.rendered
        );
    }
    write_new(
        &memcheck_directory.join("memcheck-summary.txt"),
        summary.rendered.as_bytes(),
    );
    identity
}

#[test]
#[ignore = "strict Values producer/caller native Memcheck L3; run scripts/verify-ordinary-map-owned-any-v2-values-producer-caller-l3.sh"]
fn ordinary_map_owned_any_v2_values_producer_caller_default_and_explicit_native_builds_are_strict_memcheck_clean(
) {
    let root = artifact_root();
    let mut summary_rows = vec![
        format!("route_profile={ROUTE_PROFILE}"),
        "fixture_count=1".to_owned(),
        "native_build_count=2".to_owned(),
        "strict_memcheck_count=2".to_owned(),
        "case_count=2".to_owned(),
    ];

    {
        let (case, source_name, expected_stdout) = (
            "result-one-len",
            "mir_ordinary_map_owned_any_v2_values_producer_caller.mimi",
            b"1\n".as_slice(),
        );
        let source = fixture(source_name);
        summary_rows.push(format!("fixture_source_sha256={}", sha256_file(&source)));
        let default = build_and_memcheck(&root, &source, case, "default", false, expected_stdout);
        let explicit =
            build_and_memcheck(&root, &source, case, "explicit-mir", true, expected_stdout);
        assert_eq!(
            default.mir_digest, explicit.mir_digest,
            "{case} default and explicit --mir native builds must consume the same MIR"
        );
        assert_eq!(
            default.receipt_identity_digest, explicit.receipt_identity_digest,
            "{case} default and explicit --mir native builds must consume the same receipt"
        );
        summary_rows.push(format!(
            "{case}_expected_stdout_hex={}",
            hex(expected_stdout)
        ));
        summary_rows.push(format!("{case}_mir_digest={}", default.mir_digest));
        summary_rows.push(format!(
            "{case}_receipt_identity_digest={}",
            default.receipt_identity_digest
        ));
        summary_rows.push(format!("{case}_default_explicit_identity_equal=true"));
    }

    write_new(
        &root.join("test-summary.txt"),
        format!("{}\n", summary_rows.join("\n")).as_bytes(),
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut encoded, byte| {
        encoded.push_str(&format!("{byte:02x}"));
        encoded
    })
}
