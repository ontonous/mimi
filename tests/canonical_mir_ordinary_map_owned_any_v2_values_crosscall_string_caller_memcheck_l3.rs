use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const ARTIFACT_DIR_ENV: &str = "MIMI_VALUES_CROSSCALL_STRING_CALLER_L3_ARTIFACT_DIR";
const ROUTE_PROFILE_V1: &str = "ordinary-map-owned-any-v2-values-crosscall-string-caller-v1";
const ROUTE_PROFILE_V2: &str = "ordinary-map-owned-any-v2-values-crosscall-string-caller-v2";
const MIR_RECEIPT_SCHEMA_V1: &str =
    "mimi-ordinary-map-owned-any-v2-values-crosscall-string-caller-v1";
const MIR_RECEIPT_SCHEMA_V2: &str =
    "mimi-ordinary-map-owned-any-v2-values-crosscall-string-caller-v2";
const ROUTE_CENSUS_SCHEMA: &str = "mimi-mir-route-receipt-v1";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const ROUTE_SPECIALIZED_SCHEMA_PREFIX: &str = "MIMI_ROUTE_CENSUS route-specialized-schema ";
const NATIVE_BUILD_PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";
const RELEVANT_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTDOC",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "TMPDIR",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "CARGO_NET_OFFLINE",
    "LLVM_SYS_181_PREFIX",
    "LLVM_CONFIG",
    "LLVM_CONFIG_PATH",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "RUSTC",
    "RUSTDOC",
    "RUSTC_BOOTSTRAP",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTC_LINKER",
    "CC",
    "CXX",
    "AR",
    "RANLIB",
    "LD",
    "CPPFLAGS",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "LIBRARY_PATH",
    "CPATH",
    "LIBCLANG_PATH",
    "BINDGEN_EXTRA_CLANG_ARGS",
    "PKG_CONFIG",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "PKG_CONFIG_SYSROOT_DIR",
    "LD_PRELOAD",
    "LC_ALL",
    "LANG",
    "CARGO_TERM_COLOR",
    "RUST_TEST_THREADS",
    "RUST_TEST_NOCAPTURE",
];

struct FixtureCase {
    id: &'static str,
    fixture_name: &'static str,
    route_profile: &'static str,
    expected_values_receipt_schema: &'static str,
    expected_stdout: &'static [u8],
}

const FIXTURE_CASES: [FixtureCase; 2] = [
    FixtureCase {
        id: "v1",
        fixture_name: "mir_ordinary_map_owned_any_v2_values_crosscall_string_caller.mimi",
        route_profile: ROUTE_PROFILE_V1,
        expected_values_receipt_schema: MIR_RECEIPT_SCHEMA_V1,
        expected_stdout: b"true\n",
    },
    FixtureCase {
        id: "v2-index1",
        fixture_name: "mir_ordinary_map_owned_any_v2_values_crosscall_string_caller_index1.mimi",
        route_profile: ROUTE_PROFILE_V2,
        expected_values_receipt_schema: MIR_RECEIPT_SCHEMA_V2,
        expected_stdout: b"true\n",
    },
];

struct RouteIdentity {
    profile: String,
    route_receipt_schema: String,
    values_receipt_schema: String,
    mir_digest: String,
    receipt_identity_digest: String,
}

struct RunEvidence {
    identity: RouteIdentity,
    still_reachable_line: String,
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
    for fresh_path in [path.join("native"), path.join("test-summary.txt")] {
        match fs::symlink_metadata(&fresh_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "inspect fresh L3 evidence path {}: {error}",
                fresh_path.display()
            ),
            Ok(_) => panic!(
                "refusing stale Values cross-call String caller L3 evidence at {}",
                fresh_path.display()
            ),
        }
    }
    path
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

fn effective_environment(
    overrides: &[(String, String)],
    valgrind: bool,
) -> BTreeMap<OsString, OsString> {
    let mut values = std::env::vars_os().collect::<BTreeMap<_, _>>();
    values.retain(|key, _| {
        let name = key.to_string_lossy();
        !name.starts_with("MIMI_") && !(valgrind && name.starts_with("VALGRIND_"))
    });
    values.remove(OsStr::new("LD_PRELOAD"));
    if valgrind {
        values.remove(OsStr::new("VALGRIND_OPTS"));
        values.remove(OsStr::new("VALGRIND_LIB"));
    }
    for (key, value) in overrides {
        values.insert(OsString::from(key), OsString::from(value));
    }
    values
}

fn environment_digest(environment: &BTreeMap<OsString, OsString>) -> String {
    let mut hasher = blake3::Hasher::new();
    for (key, value) in environment {
        for component in [key.as_encoded_bytes(), value.as_encoded_bytes()] {
            hasher.update(&(component.len() as u64).to_le_bytes());
            hasher.update(component);
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn is_relevant_environment_name(name: &str) -> bool {
    RELEVANT_ENVIRONMENT.contains(&name)
        || [
            "CARGO_BUILD_",
            "CARGO_PROFILE_",
            "CARGO_TARGET_",
            "CARGO_ALIAS_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

fn retained_environment(
    overrides: &[(String, String)],
    environment: &BTreeMap<OsString, OsString>,
    valgrind: bool,
) -> String {
    let mut rows = RELEVANT_ENVIRONMENT
        .iter()
        .map(|key| {
            let key = *key;
            match environment.get(OsStr::new(key)) {
                Some(value) => format!("{key}={}", json_string(&value.to_string_lossy())),
                None => format!("{key}=<unset>"),
            }
        })
        .collect::<Vec<_>>();
    rows.extend(environment.iter().filter_map(|(key, value)| {
        let name = key.to_string_lossy();
        (is_relevant_environment_name(&name) && !RELEVANT_ENVIRONMENT.contains(&name.as_ref()))
            .then(|| format!("{name}={}", json_string(&value.to_string_lossy())))
    }));
    rows.extend(
        overrides
            .iter()
            .filter(|(key, _)| !is_relevant_environment_name(key))
            .map(|(key, value)| format!("{key}={}", json_string(value))),
    );
    rows.extend([
        "ambient_MIMI_*_variables=removed".to_owned(),
        "LD_PRELOAD=removed".to_owned(),
        format!(
            "effective_environment_blake3={}",
            environment_digest(environment)
        ),
        "other_environment=preserved_and_bound_by_blake3".to_owned(),
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

struct CommandInvocation<'a> {
    program: &'a OsStr,
    arguments: &'a [OsString],
    cwd: &'a Path,
    overrides: &'a [(String, String)],
    environment: &'a BTreeMap<OsString, OsString>,
    valgrind: bool,
}

fn retain_command(directory: &Path, invocation: &CommandInvocation<'_>, output: &Output) {
    write_new(
        &directory.join("command.argv.json"),
        command_json(invocation.program, invocation.arguments).as_bytes(),
    );
    write_new(
        &directory.join("command.cwd.txt"),
        format!("{}\n", invocation.cwd.display()).as_bytes(),
    );
    write_new(
        &directory.join("command.environment.txt"),
        retained_environment(
            invocation.overrides,
            invocation.environment,
            invocation.valgrind,
        )
        .as_bytes(),
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
    let effective_environment = effective_environment(environment, valgrind);
    let mut command = Command::new(program);
    command.args(arguments).current_dir(cwd).env_clear();
    for (key, value) in &effective_environment {
        command.env(key, value);
    }

    let invocation = CommandInvocation {
        program,
        arguments,
        cwd,
        overrides: environment,
        environment: &effective_environment,
        valgrind,
    };
    match command.output() {
        Ok(output) => {
            retain_command(directory, &invocation, &output);
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
                retained_environment(environment, &effective_environment, valgrind).as_bytes(),
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

fn route_identity(
    output: &Output,
    explicit_mir: bool,
    profile: &str,
    expected_values_receipt_schema: &str,
    label: &str,
) -> RouteIdentity {
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
        Some(&profile),
        "{label} must use the dedicated cross-call String caller route"
    );
    let observed_profile = admission_fields
        .get("checker_profile")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its checker route profile"));

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

    let specialized_schema = prefixed_line(&stderr, ROUTE_SPECIALIZED_SCHEMA_PREFIX, label);
    let specialized_schema_fields =
        fields(specialized_schema, ROUTE_SPECIALIZED_SCHEMA_PREFIX, label);
    let specialized_schema_field_order = specialized_schema
        .strip_prefix(ROUTE_SPECIALIZED_SCHEMA_PREFIX)
        .expect("specialized route schema prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("schema field key=value").0)
        .collect::<Vec<_>>();
    assert_eq!(
        specialized_schema_field_order,
        [
            "consumer",
            "checker_profile",
            "receipt_profile",
            "schema",
            "mir_digest",
            "receipt_identity_digest",
        ],
        "{label} specialized route schema field contract changed"
    );
    assert_eq!(
        specialized_schema_fields.get("consumer"),
        Some(if explicit_mir {
            &"explicit-mir"
        } else {
            &"default"
        }),
        "{label} specialized route schema consumer"
    );
    assert_eq!(
        specialized_schema_fields.get("checker_profile"),
        Some(&observed_profile),
        "{label} specialized schema must be Checker-selected for this route"
    );
    assert_eq!(
        specialized_schema_fields.get("receipt_profile"),
        Some(&observed_profile),
        "{label} specialized schema must describe the admitted MIR receipt"
    );
    let observed_values_receipt_schema = specialized_schema_fields
        .get("schema")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its specialized Values receipt schema"));
    assert_eq!(
        observed_values_receipt_schema, expected_values_receipt_schema,
        "{label} specialized Values receipt schema"
    );
    assert_ne!(
        observed_values_receipt_schema, ROUTE_CENSUS_SCHEMA,
        "{label} specialized Values receipt schema must remain distinct from the generic route manifest"
    );
    assert_eq!(
        specialized_schema_fields.get("mir_digest"),
        Some(&mir_digest),
        "{label} specialized schema must bind the admitted MIR digest"
    );
    assert_eq!(
        specialized_schema_fields.get("receipt_identity_digest"),
        Some(&receipt_identity_digest),
        "{label} specialized schema must bind the admitted receipt identity"
    );

    let native = prefixed_line(&stderr, NATIVE_BUILD_PREFIX, label);
    let native_fields = fields(native, NATIVE_BUILD_PREFIX, label);
    let native_field_order = native
        .strip_prefix(NATIVE_BUILD_PREFIX)
        .expect("native-build prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("native field key=value").0)
        .collect::<Vec<_>>();
    assert_eq!(
        native_field_order,
        [
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
        ],
        "{label} native-build census field contract changed"
    );
    assert_eq!(
        native_fields.get("route_id"),
        Some(&profile),
        "{label} native build must consume the cross-call String caller route"
    );
    assert_eq!(
        native_fields.get("schema"),
        Some(&ROUTE_CENSUS_SCHEMA),
        "{label} native route receipt schema"
    );
    let observed_route_receipt_schema = native_fields
        .get("schema")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its native route receipt schema"));
    let native_profile = native_fields
        .get("profile")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted its native route profile"));
    assert_eq!(
        native_profile, observed_profile,
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
    assert_eq!(
        native_fields.get("route_identity_digest"),
        Some(&receipt_identity_digest),
        "{label} native build must consume the admitted route identity"
    );

    RouteIdentity {
        profile: observed_profile.to_owned(),
        route_receipt_schema: observed_route_receipt_schema.to_owned(),
        values_receipt_schema: observed_values_receipt_schema.to_owned(),
        mir_digest: mir_digest.to_owned(),
        receipt_identity_digest: receipt_identity_digest.to_owned(),
    }
}

fn build_and_memcheck(
    root: &Path,
    source: &Path,
    case: &FixtureCase,
    mode: &str,
    explicit_mir: bool,
) -> RunEvidence {
    let mode_directory = root.join("native").join(case.id).join(mode);
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
    let label = format!("{} {mode} native build", case.id);
    assert_success(&build_output, &label);
    assert!(
        binary.is_file(),
        "{label} did not create {}",
        binary.display()
    );
    let identity = route_identity(
        &build_output,
        explicit_mir,
        case.route_profile,
        case.expected_values_receipt_schema,
        &label,
    );
    write_new(
        &mode_directory.join("route-identity.txt"),
        format!(
            "case={}\nmode={mode}\nprofile={}\nroute_receipt_schema={}\nobserved_mir_receipt_schema={}\nmir_digest={}\nreceipt_identity_digest={}\n",
            case.id,
            identity.profile,
            identity.route_receipt_schema,
            identity.values_receipt_schema,
            identity.mir_digest, identity.receipt_identity_digest
        )
        .as_bytes(),
    );

    write_new(
        &mode_directory.join("expected-stdout.bin"),
        case.expected_stdout,
    );
    let native_output = run_recorded(
        binary.as_os_str(),
        &[],
        repo_root,
        &[("LC_ALL".to_owned(), "C".to_owned())],
        &mode_directory.join("native-command"),
        false,
    );
    let native_label = format!("{} {mode} native run", case.id);
    assert_success(&native_output, &native_label);
    assert_eq!(
        native_output.stdout.as_slice(),
        case.expected_stdout,
        "{native_label} stdout changed"
    );
    assert!(
        native_output.stderr.is_empty(),
        "{native_label} emitted stderr:\n{}",
        text(&native_output.stderr)
    );

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
        OsString::from("--tool=memcheck"),
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
    let memcheck_label = format!("{} {mode} strict Valgrind Memcheck", case.id);
    assert_success(&valgrind_output, &memcheck_label);
    assert_eq!(
        valgrind_output.stdout.as_slice(),
        case.expected_stdout,
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
    assert_eq!(
        (
            summary.still_reachable_bytes,
            summary.still_reachable_blocks
        ),
        (0, 0),
        "{memcheck_label} must leave no still-reachable allocations:\n{}",
        summary.rendered
    );
    write_new(
        &memcheck_directory.join("memcheck-summary.txt"),
        summary.rendered.as_bytes(),
    );
    RunEvidence {
        identity,
        still_reachable_line: summary.still_reachable_line,
    }
}

#[test]
#[ignore = "strict Values cross-call String caller native Memcheck L3; run scripts/verify-ordinary-map-owned-any-v2-values-crosscall-string-caller-l3.sh"]
fn ordinary_map_owned_any_v2_values_crosscall_string_caller_default_and_explicit_native_builds_are_strict_memcheck_clean(
) {
    assert_ne!(
        FIXTURE_CASES[0].route_profile, FIXTURE_CASES[1].route_profile,
        "V1 and Index1 must retain distinct canonical route profiles"
    );
    assert_ne!(
        FIXTURE_CASES[0].expected_values_receipt_schema,
        FIXTURE_CASES[1].expected_values_receipt_schema,
        "V1 and Index1 must retain distinct Values receipt schemas"
    );
    let root = artifact_root();
    let mut summary = format!(
        "fixture_count={}\nnative_build_count=4\nstrict_memcheck_count=4\ncase_count=2\nall_default_explicit_identity_equal=true\n",
        FIXTURE_CASES.len()
    );

    for case in &FIXTURE_CASES {
        let source = fixture(case.fixture_name);
        let default = build_and_memcheck(&root, &source, case, "default", false);
        let explicit = build_and_memcheck(&root, &source, case, "explicit-mir", true);
        assert_eq!(
            default.identity.mir_digest, explicit.identity.mir_digest,
            "{} default and explicit --mir native builds must consume the same MIR",
            case.id
        );
        assert_eq!(
            default.identity.receipt_identity_digest, explicit.identity.receipt_identity_digest,
            "{} default and explicit --mir native builds must consume the same route receipt",
            case.id
        );
        assert_eq!(
            default.identity.profile, explicit.identity.profile,
            "{} default and explicit --mir native builds must report the same route profile",
            case.id
        );
        assert_eq!(
            default.identity.route_receipt_schema, explicit.identity.route_receipt_schema,
            "{} default and explicit --mir native builds must report the same route schema",
            case.id
        );
        assert_eq!(
            default.identity.values_receipt_schema, explicit.identity.values_receipt_schema,
            "{} default and explicit --mir native builds must report the same specialized Values schema",
            case.id
        );
        summary.push_str(&format!(
            "{}_fixture={}\n{}_route_profile={}\n{}_observed_route_profile={}\n{}_route_receipt_schema={}\n{}_observed_mir_receipt_schema={}\n{}_default_explicit_identity_equal=true\n{}_expected_stdout_hex={}\n{}_mir_digest={}\n{}_receipt_identity_digest={}\n{}_default_still_reachable={}\n{}_explicit_mir_still_reachable={}\n",
            case.id,
            case.fixture_name,
            case.id,
            case.route_profile,
            case.id,
            default.identity.profile,
            case.id,
            default.identity.route_receipt_schema,
            case.id,
            default.identity.values_receipt_schema,
            case.id,
            case.id,
            hex(case.expected_stdout),
            case.id,
            default.identity.mir_digest,
            case.id,
            default.identity.receipt_identity_digest,
            case.id,
            default.still_reachable_line,
            case.id,
            explicit.still_reachable_line,
        ));
    }
    summary.push_str(&format!(
        "expected_stdout_hex={}\n",
        hex(FIXTURE_CASES[0].expected_stdout)
    ));
    write_new(&root.join("test-summary.txt"), summary.as_bytes());
    assert_exact_native_evidence_layout(&root);
}

fn assert_exact_native_evidence_layout(root: &Path) {
    let mut expected = Vec::new();
    let mut expected_directories = Vec::new();
    for case in &FIXTURE_CASES {
        expected_directories.push(case.id.to_owned());
        for mode in ["default", "explicit-mir"] {
            let prefix = format!("{}/{mode}", case.id);
            expected_directories.extend([
                prefix.to_string(),
                format!("{prefix}/build-command"),
                format!("{prefix}/native-command"),
                format!("{prefix}/binary-hash-command"),
                format!("{prefix}/memcheck"),
                format!("{prefix}/memcheck/isolated-home"),
                format!("{prefix}/memcheck/valgrind-command"),
            ]);
            for directory in [
                "build-command",
                "native-command",
                "binary-hash-command",
                "memcheck/valgrind-command",
            ] {
                for file in [
                    "command.argv.json",
                    "command.cwd.txt",
                    "command.environment.txt",
                    "stdout.bin",
                    "stderr.bin",
                    "process.status",
                ] {
                    expected.push(format!("{prefix}/{directory}/{file}"));
                }
            }
            expected.extend([
                format!("{prefix}/expected-stdout.bin"),
                format!("{prefix}/program"),
                format!("{prefix}/program.sha256"),
                format!("{prefix}/route-identity.txt"),
                format!("{prefix}/memcheck/valgrind.log"),
                format!("{prefix}/memcheck/memcheck-summary.txt"),
            ]);
        }
    }
    expected.sort();

    fn collect_entries(
        root: &Path,
        current: &Path,
        files: &mut Vec<String>,
        directories: &mut Vec<String>,
    ) {
        for entry in fs::read_dir(current).unwrap_or_else(|error| {
            panic!("read evidence directory {}: {error}", current.display())
        }) {
            let entry = entry.unwrap_or_else(|error| panic!("read evidence entry: {error}"));
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("native evidence stays under its root")
                .to_string_lossy()
                .replace('\\', "/");
            let file_type = entry.file_type().unwrap_or_else(|error| {
                panic!("inspect evidence entry {}: {error}", path.display())
            });
            if file_type.is_dir() {
                directories.push(relative);
                collect_entries(root, &path, files, directories);
            } else if file_type.is_file() {
                files.push(relative);
            } else {
                panic!("unexpected non-file evidence entry: {}", path.display());
            }
        }
    }

    let native = root.join("native");
    let mut actual = Vec::new();
    let mut actual_directories = Vec::new();
    collect_entries(&native, &native, &mut actual, &mut actual_directories);
    actual.sort();
    expected_directories.sort();
    actual_directories.sort();
    assert_eq!(
        actual, expected,
        "native evidence has missing or unexpected files"
    );
    assert_eq!(
        actual_directories, expected_directories,
        "native evidence has missing or unexpected directories"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut encoded, byte| {
        encoded.push_str(&format!("{byte:02x}"));
        encoded
    })
}
