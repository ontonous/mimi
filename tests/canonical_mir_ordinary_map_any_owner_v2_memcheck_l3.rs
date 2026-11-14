use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;
use memcheck_summary::MemcheckSummary;

const ROUTE_PROFILE: &str = "map-any-owner-v2";
const DIRECT_PRINT_RESULT_ROUTE_PROFILE: &str = "map-any-owner-v2-direct-print-result-v1";
const COMPUTED_KEY_GET_ROUTE_PROFILE: &str = "map-any-owner-v2-computed-key-get-v1";
const KEYS_SNAPSHOT_ROUTE_PROFILE: &str = "map-any-owner-v2-keys-snapshot-v1";
const HETEROGENEOUS_VALUES_ROUTE_PROFILE: &str =
    "map-any-owner-v2-values-heterogeneous-string-index1-v1";
const DIRECT_TUPLE_GET_L1_ROUTE_PROFILE: &str =
    "ordinary-map-owned-any-v2-direct-tuple-get-unpack-l1-core-v1";
const EXPECTED_STDOUT: &str = "owned label\n";
const EMPTY_ROOT_EXPECTED_STDOUT: &str = "false\n0\n";
const POPULATED_VALUES_EXPECTED_STDOUT: &str = "2\n";
const KEYS_SNAPSHOT_EXPECTED_STDOUT: &str = "answer\n";
const RECEIPT_FIELDS: &[&str] = &[
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

fn mimi_bin_with_source() -> (PathBuf, &'static str) {
    (mimi_bin(), "Cargo-built CARGO_BIN_EXE_mimi")
}

fn cargo_built_mimi_bin() -> PathBuf {
    mimi_bin()
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_record.mimi")
}

fn computed_key_string_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_computed_key_get_string.mimi")
}

fn computed_key_i32_control_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_computed_key_get.mimi")
}

fn empty_root_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_empty_root.mimi")
}

fn populated_values_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_release.mimi")
}

fn keys_snapshot_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_keys_snapshot.mimi")
}

fn heterogeneous_values_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_heterogeneous_string_index1.mimi")
}

fn direct_tuple_get_l1_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owned_any_v2_direct_tuple_get_l1.mimi")
}

fn live_values_trap_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_any_owner_v2_values_live_trap.mimi")
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
    let status = match output.status.code() {
        Some(code) => format!("exit_code={code}\nsuccess={}\n", output.status.success()),
        None => format!(
            "exit_code=signal\nsuccess={}\nstatus={:?}\n",
            output.status.success(),
            output.status
        ),
    };
    write_new(&directory.join("process.status"), status.as_bytes());
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

fn assert_default_route(stderr: &str, expected_digest: &str, mode: &str) {
    let marker = format!("canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest=");
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one MapAnyOwner v2 route receipt: {stderr}"
    );
    let observed_digest = matching[0]
        .split_once("mir_digest=")
        .expect("canonical route record contains a MIR digest")
        .1
        .trim();
    assert_eq!(
        observed_digest, expected_digest,
        "{mode} MIR digest differs"
    );
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_explicit_mir_census(stderr: &str, expected_digest: &str, mode: &str) {
    // The explicit --mir command has no route-receipt output channel yet.
    // Assert only the actual native consumer census and its digest; never
    // compose a claimed explicit receipt from the earlier default-route run.
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one complete canonical MIR native census: {stderr}"
    );
    let summary = matching[0];
    let (counts, suffix) = summary
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("canonical MIR census has its stable prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("canonical MIR census has complete emission counts and no-fallback marker");
    let (emitted, expected) = counts
        .split_once('/')
        .expect("canonical MIR census reports emitted/expected counts");
    let emitted = emitted
        .parse::<usize>()
        .expect("canonical MIR census emitted count is an integer");
    let expected = expected
        .parse::<usize>()
        .expect("canonical MIR census expected count is an integer");
    assert!(
        expected > 0,
        "{mode} MIR census must cover a nonempty program"
    );
    assert_eq!(
        emitted, expected,
        "{mode} omitted a MIR function: {summary}"
    );
    assert_eq!(suffix, expected_digest, "{mode} MIR census digest differs");
    assert_no_legacy_fallback(stderr, mode);
}

#[test]
fn ordinary_map_owned_any_v2_default_and_explicit_entries_report_closed_readiness() {
    let mimi = mimi_bin();
    let source = fixture();
    let directory = TemporaryDirectory::new();

    for direct in [false, true] {
        let mode = if direct {
            "explicit --mir production entry"
        } else {
            "default production entry"
        };
        let binary = directory.0.join(if direct {
            "explicit-map-v2"
        } else {
            "default-map-v2"
        });
        let mut command = Command::new(&mimi);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if direct {
            command.arg("--mir");
        }
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("invoke {mode}: {error}"));
        assert!(
            !output.status.success(),
            "{mode} must remain blocked while Map/Any readiness is closed"
        );
        let stderr = text(&output.stderr);
        if direct {
            assert!(
                stderr.contains("MIR-COVERAGE-001")
                    && stderr.contains("ordinary Map/Any owner-v2 route is not production-ready for explicit canonical execution or verification"),
                "{mode} failed for an unrelated reason instead of the explicit readiness gate: {stderr}"
            );
        } else {
            assert!(
                stderr.contains("default Canonical MIR route rejected")
                    && stderr.contains("CompleteCoverage")
                    && stderr.contains("production-readiness gate is closed"),
                "{mode} did not expose the closed CompleteCoverage readiness boundary: {stderr}"
            );
        }
        assert!(
            !stderr.contains("canonical MIR native compiled "),
            "{mode} reached native emission despite the closed readiness gate: {stderr}"
        );
        assert!(
            !binary.exists(),
            "{mode} produced a native binary despite the closed readiness gate"
        );

        if direct {
            assert!(
                !stderr.contains("canonical route disposition: canonical"),
                "{mode} cannot claim a successful route receipt after rejecting the direct call: {stderr}"
            );
        } else {
            assert!(
                !stderr.contains("canonical route disposition: canonical"),
                "{mode} cannot claim a successful default route receipt after rejection: {stderr}"
            );
        }
    }
}

fn parse_manifest(output: &Output) -> BTreeMap<String, String> {
    assert_success(output, "mimi mir --receipt");
    let fields = text(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some("cli-mir-v1"),
        "the CLI receipt schema changed"
    );
    for field in RECEIPT_FIELDS {
        let digest = fields
            .get(*field)
            .unwrap_or_else(|| panic!("MIR receipt omitted {field}"));
        assert!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "MIR receipt has malformed {field}: {digest}"
        );
    }
    fields
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-map-any-owner-v2-l3-{}-{}",
            std::process::id(),
            stamp
        ));
        std::fs::create_dir(&path).expect("create isolated MapAnyOwner v2 L3 directory");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct L3RunDirectory {
    path: PathBuf,
    _temporary: Option<TemporaryDirectory>,
}

fn prospective_canonical_l3_path(path: &Path) -> PathBuf {
    if fs::symlink_metadata(path).is_ok() {
        return fs::canonicalize(path).unwrap_or_else(|error| {
            panic!("resolve L3 artifact path {}: {error}", path.display())
        });
    }

    let mut missing = Vec::new();
    let mut ancestor = path.to_path_buf();
    while fs::symlink_metadata(&ancestor).is_err() {
        let component = ancestor.file_name().unwrap_or_else(|| {
            panic!(
                "L3 artifact path has no existing ancestor: {}",
                path.display()
            )
        });
        assert_ne!(
            component,
            OsStr::new(".."),
            "L3 artifact paths must be normalized before use: {}",
            path.display()
        );
        missing.push(component.to_os_string());
        assert!(
            ancestor.pop(),
            "L3 artifact path has no parent: {}",
            path.display()
        );
    }
    let mut canonical = fs::canonicalize(&ancestor).unwrap_or_else(|error| {
        panic!(
            "resolve L3 artifact ancestor {}: {error}",
            ancestor.display()
        )
    });
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    canonical
}

fn assert_l3_artifacts_outside_repository(path: &Path) {
    let repository = fs::canonicalize(env!("CARGO_MANIFEST_DIR"))
        .expect("resolve repository root for L3 artifact policy");
    let candidate = prospective_canonical_l3_path(path);
    assert!(
        !candidate.starts_with(&repository),
        "L3 artifacts must be stored outside the repository tree: {}",
        candidate.display()
    );
}

impl L3RunDirectory {
    fn new_direct_print_result() -> Self {
        if let Some(configured) =
            std::env::var_os("MIMI_MAP_V2_L3_ARTIFACT_DIR").filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let root = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for DirectPrintResult artifacts")
                    .join(configured)
            };
            assert!(
                !root.exists() && fs::symlink_metadata(&root).is_err(),
                "refusing to reuse existing DirectPrintResult artifact path {}",
                root.display()
            );
        }
        Self::new()
    }

    fn new() -> Self {
        if let Some(configured) =
            std::env::var_os("MIMI_MAP_V2_L3_ARTIFACT_DIR").filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let root = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for L3 artifacts")
                    .join(configured)
            };
            assert_l3_artifacts_outside_repository(&root);
            if !root.exists() {
                if let Some(parent) = root.parent() {
                    fs::create_dir_all(parent).expect("create parent for L3 artifact directory");
                }
                fs::create_dir(&root).expect("create task-specific MapAnyOwner v2 L3 directory");
            }
            let root =
                fs::canonicalize(&root).expect("resolve MapAnyOwner v2 L3 artifact directory");
            let path = root.join("native");
            fs::create_dir(&path).unwrap_or_else(|error| {
                panic!(
                    "create fresh native evidence directory {} without replacing prior evidence: {error}",
                    path.display()
                )
            });
            Self {
                path,
                _temporary: None,
            }
        } else {
            let temporary = TemporaryDirectory::new();
            let path = temporary.0.clone();
            Self {
                path,
                _temporary: Some(temporary),
            }
        }
    }
}

fn create_capture_directory(path: &Path) {
    fs::create_dir(path).unwrap_or_else(|error| {
        panic!(
            "create fresh command evidence directory {} without replacing prior evidence: {error}",
            path.display()
        )
    });
}

fn retain_mimi_binary_identity(mimi: &Path, source: &str, evidence_root: &Path) {
    let canonical = fs::canonicalize(mimi).unwrap_or_else(|error| {
        panic!("canonicalize Cargo mimi binary {}: {error}", mimi.display())
    });
    let directory = evidence_root.join("mimi-binary-sha256");
    create_capture_directory(&directory);

    let arguments = vec![OsString::from("--"), canonical.as_os_str().to_owned()];
    let output = Command::new("sha256sum")
        .args(&arguments)
        .output()
        .expect("hash the actual Cargo mimi executable for L3 evidence");
    retain_command_output(&directory, OsStr::new("sha256sum"), &arguments, &output);
    assert_success(&output, "sha256sum CARGO_BIN_EXE_mimi");

    let hash_output = text(&output.stdout);
    let line = hash_output.trim_end_matches(['\r', '\n']);
    let (digest, hashed_path) = line
        .split_once("  ")
        .expect("sha256sum output contains a digest and path");
    let canonical_path = canonical.to_string_lossy();
    assert_eq!(hashed_path, canonical_path.as_ref());
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "sha256sum returned a malformed SHA-256 digest: {digest}"
    );
    write_new(
        &evidence_root.join("mimi-binary-identity.txt"),
        format!(
            "binary_source={source}\ncanonical_path={}\nsha256={digest}\n",
            canonical.display()
        )
        .as_bytes(),
    );
}

#[derive(Debug)]
struct StrictValgrind {
    launcher: PathBuf,
    engine: PathBuf,
    version: String,
}

fn executable_on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("strict Memcheck requires PATH");
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
                "canonicalize executable {name} from PATH candidate {}: {error}",
                candidate.display()
            )
        });
    }
    panic!("strict Memcheck requires executable {name} on PATH")
}

fn is_elf_executable(path: &Path) -> bool {
    fs::read(path)
        .unwrap_or_else(|error| panic!("read executable {}: {error}", path.display()))
        .starts_with(b"\x7fELF")
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
        .expect("sha256sum output contains a digest and path");
    assert_eq!(hashed_path, canonical.to_string_lossy());
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "sha256sum returned malformed digest {digest}"
    );
    digest.to_owned()
}

fn record_strict_valgrind_identity(evidence_root: &Path) -> StrictValgrind {
    let launcher = executable_on_path("valgrind");
    let engine = if is_elf_executable(&launcher) {
        launcher.clone()
    } else {
        let launcher_text = fs::read_to_string(&launcher).unwrap_or_else(|error| {
            panic!("read Valgrind launcher {}: {error}", launcher.display())
        });
        let engine_invocations = launcher_text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("exec "))
            .collect::<Vec<_>>();
        assert_eq!(
            engine_invocations,
            ["exec $0.bin \"$@\""],
            "Valgrind script launcher must exec only its adjacent packaged .bin engine; refusing a PATH script shim"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = fs::metadata(&launcher)
                .unwrap_or_else(|error| panic!("inspect {}: {error}", launcher.display()));
            assert_eq!(
                metadata.uid(),
                0,
                "script Valgrind launcher must be root-owned; refusing a user PATH shim"
            );
            assert_eq!(
                metadata.permissions().mode() & 0o022,
                0,
                "script Valgrind launcher must not be group/world writable"
            );
        }
        let launcher_name = launcher
            .file_name()
            .and_then(OsStr::to_str)
            .expect("Valgrind launcher filename is UTF-8");
        assert_eq!(
            launcher_name, "valgrind",
            "only the packaged valgrind launcher may be a script"
        );
        let companion = launcher.with_file_name("valgrind.bin");
        let companion = fs::canonicalize(&companion).unwrap_or_else(|error| {
            panic!(
                "resolve packaged Valgrind engine next to {}: {error}",
                launcher.display()
            )
        });
        assert!(
            is_elf_executable(&companion),
            "Valgrind script launcher companion {} is not an ELF engine",
            companion.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = fs::metadata(&companion)
                .unwrap_or_else(|error| panic!("inspect {}: {error}", companion.display()));
            assert_eq!(
                metadata.uid(),
                0,
                "script Valgrind engine must be root-owned; refusing a user PATH shim"
            );
            assert_eq!(
                metadata.permissions().mode() & 0o022,
                0,
                "script Valgrind engine must not be group/world writable"
            );
        }
        companion
    };

    let version_directory = evidence_root.join("valgrind-version");
    create_capture_directory(&version_directory);
    let version_arguments = vec![OsString::from("--version")];
    let version_output = Command::new(&launcher)
        .args(&version_arguments)
        .output()
        .unwrap_or_else(|error| panic!("query strict Valgrind version: {error}"));
    retain_command_output(
        &version_directory,
        launcher.as_os_str(),
        &version_arguments,
        &version_output,
    );
    assert_success(&version_output, "Valgrind version query");
    let version = text(&version_output.stdout).trim().to_owned();
    assert!(
        version.starts_with("valgrind-"),
        "Valgrind launcher returned an unexpected version string: {version:?}"
    );

    let identity_directory = evidence_root.join("valgrind-identity");
    create_capture_directory(&identity_directory);
    let launcher_hash = retain_sha256(&launcher, "launcher", &identity_directory);
    let engine_hash = if launcher == engine {
        launcher_hash.clone()
    } else {
        retain_sha256(&engine, "engine", &identity_directory)
    };
    let engine_version = if launcher == engine {
        version.clone()
    } else {
        let engine_version_directory = identity_directory.join("engine-version");
        create_capture_directory(&engine_version_directory);
        let arguments = vec![OsString::from("--version")];
        let output = Command::new(&engine)
            .args(&arguments)
            .output()
            .unwrap_or_else(|error| panic!("query Valgrind engine version: {error}"));
        retain_command_output(
            &engine_version_directory,
            engine.as_os_str(),
            &arguments,
            &output,
        );
        assert_success(&output, "Valgrind engine version query");
        text(&output.stdout).trim().to_owned()
    };
    assert_eq!(
        version, engine_version,
        "Valgrind launcher and canonical ELF engine report different versions"
    );
    write_new(
        &identity_directory.join("valgrind-identity.txt"),
        format!(
            "launcher_path={}\nlauncher_sha256={launcher_hash}\nengine_path={}\nengine_sha256={engine_hash}\nversion={version}\n",
            launcher.display(),
            engine.display()
        )
        .as_bytes(),
    );
    StrictValgrind {
        launcher,
        engine,
        version,
    }
}

fn assert_strict_memcheck(
    binary: &Path,
    mode: &str,
    expected_stdout: &str,
    expected_status: i32,
    evidence_directory: &Path,
    retain_valgrind_log: bool,
) -> Vec<u8> {
    assert_strict_memcheck_with_valgrind(
        Path::new("valgrind"),
        binary,
        mode,
        expected_stdout,
        expected_status,
        evidence_directory,
        retain_valgrind_log,
    )
    .0
}

fn assert_strict_memcheck_with_valgrind(
    valgrind: &Path,
    binary: &Path,
    mode: &str,
    expected_stdout: &str,
    expected_status: i32,
    evidence_directory: &Path,
    retain_valgrind_log: bool,
) -> (Vec<u8>, MemcheckSummary) {
    write_new(
        &evidence_directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code={expected_status}\nexpected_stdout_bytes={}\n",
            expected_stdout.len()
        )
        .as_bytes(),
    );
    write_new(
        &evidence_directory.join("expected-native-stdout.bin"),
        expected_stdout.as_bytes(),
    );
    let valgrind_log = evidence_directory.join("valgrind.log");
    let mut arguments = vec![
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=97"),
    ];
    if retain_valgrind_log {
        arguments.push(OsString::from(format!(
            "--log-file={}",
            valgrind_log.display()
        )));
    }
    arguments.push(binary.as_os_str().to_owned());
    let output = Command::new(valgrind)
        .args(&arguments)
        .output()
        .expect("run MapAnyOwner v2 native binary under strict Valgrind Memcheck");
    retain_command_output(
        evidence_directory,
        valgrind.as_os_str(),
        &arguments,
        &output,
    );
    let valgrind_output = if retain_valgrind_log {
        fs::read(&valgrind_log).unwrap_or_else(|error| {
            panic!("read raw Valgrind log {}: {error}", valgrind_log.display())
        })
    } else {
        output.stderr.clone()
    };
    let valgrind_report = text(&valgrind_output);
    let application_stderr = text(&output.stderr);
    let report = if retain_valgrind_log {
        format!("{valgrind_report}\napplication stderr:\n{application_stderr}")
    } else {
        valgrind_report.clone()
    };
    let summary = memcheck_summary::parse(&valgrind_report);
    write_new(
        &evidence_directory.join("memcheck-summary.txt"),
        summary.rendered.as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{mode} changed exit status or Memcheck found an error:\n{report}"
    );
    assert_eq!(
        output.stdout.as_slice(),
        expected_stdout.as_bytes(),
        "{mode} native stdout"
    );
    for kind in ["definitely lost", "indirectly lost", "possibly lost"] {
        assert!(
            summary
                .rendered
                .lines()
                .any(|line| line.contains(&format!("{kind}: 0 bytes in 0 blocks"))),
            "{mode} reported a {kind} owner leak:\n{report}"
        );
    }
    assert!(
        summary.rendered.contains("ERROR SUMMARY: 0 errors"),
        "{mode} native execution had Memcheck errors:\n{report}"
    );
    eprintln!("[mimi-test] strict MapAnyOwner v2 Memcheck {mode} report:\n{report}");
    (output.stdout, summary)
}

fn assert_direct_print_result_route_census(
    stderr: &str,
    expected_mode: &str,
    mode: &str,
) -> (String, String) {
    let admission_rows = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        admission_rows.len(),
        1,
        "{mode} must report one Checker-bound DirectPrintResult admission: {stderr}"
    );
    let admission = admission_rows[0];
    assert_eq!(
        census_field(admission, "mode"),
        Some(expected_mode),
        "{mode} route admission mode"
    );
    assert_eq!(
        census_field(admission, "checker_profile"),
        Some(DIRECT_PRINT_RESULT_ROUTE_PROFILE),
        "{mode} Checker route profile"
    );
    assert_eq!(
        census_field(admission, "build_route_profile"),
        Some(DIRECT_PRINT_RESULT_ROUTE_PROFILE),
        "{mode} native build route profile"
    );
    let digest = census_field(admission, "mir_digest")
        .expect("DirectPrintResult admission carries the MIR digest")
        .to_owned();
    let receipt_identity = census_field(admission, "receipt_identity_digest")
        .expect("DirectPrintResult admission carries the Checker receipt identity")
        .to_owned();
    for (field, expected) in [
        ("mir_route_identity_digest", receipt_identity.as_str()),
        ("build_route_identity_digest", receipt_identity.as_str()),
    ] {
        assert_eq!(
            census_field(admission, field),
            Some(expected),
            "{mode} {field} differs from the Checker receipt identity"
        );
    }
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{mode} has malformed DirectPrintResult MIR digest: {digest}"
    );
    assert!(
        receipt_identity.len() == 64
            && receipt_identity
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "{mode} has malformed DirectPrintResult route identity: {receipt_identity}"
    );

    let native_rows = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        native_rows.len(),
        1,
        "{mode} must report one DirectPrintResult native consumer receipt: {stderr}"
    );
    let native = native_rows[0];
    for field in ["route_id", "profile", "build_route_profile"] {
        assert_eq!(
            census_field(native, field),
            Some(DIRECT_PRINT_RESULT_ROUTE_PROFILE),
            "{mode} native consumer {field}"
        );
    }
    for (field, expected) in [
        ("mir_digest", digest.as_str()),
        ("mir_route_identity_digest", receipt_identity.as_str()),
        ("route_identity_digest", receipt_identity.as_str()),
    ] {
        assert_eq!(
            census_field(native, field),
            Some(expected),
            "{mode} native consumer {field}"
        );
    }

    if expected_mode == "default" {
        let route_lines = stderr
            .lines()
            .filter(|line| line.contains("canonical route disposition:"))
            .collect::<Vec<_>>();
        assert_eq!(
            route_lines.len(),
            1,
            "{mode} must emit one default route disposition: {stderr}"
        );
        let marker = format!(
            "canonical route disposition: canonical ({DIRECT_PRINT_RESULT_ROUTE_PROFILE}) mir_digest={digest}"
        );
        assert_eq!(
            route_lines[0], marker,
            "{mode} disposition does not carry its exact DirectPrintResult receipt"
        );
    }
    assert_explicit_mir_census(stderr, &digest, mode);
    assert_no_legacy_fallback(stderr, mode);
    assert!(
        !stderr.contains("VerifierOnly"),
        "{mode} execution cannot use a verifier-only projection: {stderr}"
    );
    (digest, receipt_identity)
}

fn assert_computed_key_get_default_route(stderr: &str, expected_digest: &str, mode: &str) {
    let marker = format!(
        "canonical route disposition: canonical ({COMPUTED_KEY_GET_ROUTE_PROFILE}) mir_digest="
    );
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one ComputedKeyGet route receipt: {stderr}"
    );
    let digest = matching[0]
        .split_once("mir_digest=")
        .expect("ComputedKeyGet route record has its MIR digest")
        .1
        .trim();
    assert_eq!(digest, expected_digest, "{mode} MIR digest differs");
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_keys_snapshot_default_route(stderr: &str, expected_digest: &str, mode: &str) {
    let marker = format!(
        "canonical route disposition: canonical ({KEYS_SNAPSHOT_ROUTE_PROFILE}) mir_digest="
    );
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one KeysSnapshot route receipt: {stderr}"
    );
    let digest = matching[0]
        .split_once("mir_digest=")
        .expect("KeysSnapshot route receipt contains a MIR digest")
        .1
        .trim();
    assert_eq!(digest, expected_digest, "{mode} MIR digest differs");
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_heterogeneous_values_default_route(stderr: &str, expected_digest: &str, mode: &str) {
    let marker = format!(
        "canonical route disposition: canonical ({HETEROGENEOUS_VALUES_ROUTE_PROFILE}) mir_digest="
    );
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one heterogeneous Values route receipt: {stderr}"
    );
    let digest = matching[0]
        .split_once("mir_digest=")
        .expect("heterogeneous Values route receipt contains a MIR digest")
        .1
        .trim();
    assert_eq!(digest, expected_digest, "{mode} MIR digest differs");
    assert_no_legacy_fallback(stderr, mode);
}

fn census_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|field| field.strip_prefix(&format!("{name}=")))
}

fn assert_direct_tuple_get_l1_route_census(stderr: &str, mode: &str) -> (String, String) {
    let admission_rows = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        admission_rows.len(),
        1,
        "{mode} must report its exact source-bound route admission: {stderr}"
    );
    let admission = admission_rows[0];
    assert_eq!(
        census_field(admission, "checker_profile"),
        Some(DIRECT_TUPLE_GET_L1_ROUTE_PROFILE),
        "{mode} Checker profile"
    );
    assert_eq!(
        census_field(admission, "build_route_profile"),
        Some(DIRECT_TUPLE_GET_L1_ROUTE_PROFILE),
        "{mode} build profile"
    );
    let digest = census_field(admission, "mir_digest")
        .expect("route admission carries the MIR digest")
        .to_owned();
    let receipt_identity = census_field(admission, "receipt_identity_digest")
        .expect("route admission carries the source receipt identity")
        .to_owned();
    assert_eq!(
        census_field(admission, "mir_route_identity_digest"),
        Some(receipt_identity.as_str()),
        "{mode} MIR and Checker route identities differ"
    );
    assert_eq!(
        census_field(admission, "build_route_identity_digest"),
        Some(receipt_identity.as_str()),
        "{mode} build and Checker route identities differ"
    );

    let native_rows = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        native_rows.len(),
        1,
        "{mode} must report the native consumer receipt: {stderr}"
    );
    assert_eq!(
        census_field(native_rows[0], "route_id"),
        Some(DIRECT_TUPLE_GET_L1_ROUTE_PROFILE),
        "{mode} native route identity"
    );
    assert_eq!(
        census_field(native_rows[0], "profile"),
        Some(DIRECT_TUPLE_GET_L1_ROUTE_PROFILE),
        "{mode} native route profile"
    );
    assert_eq!(
        census_field(native_rows[0], "mir_digest"),
        Some(digest.as_str()),
        "{mode} native MIR digest"
    );
    assert_no_legacy_fallback(stderr, mode);
    assert!(
        !stderr.contains("VerifierOnly"),
        "{mode} execution cannot use the L2-only projection: {stderr}"
    );
    (digest, receipt_identity)
}

fn assert_direct_print_result_strict_memcheck(
    binary: &Path,
    mode: &str,
    expected_stdout: &str,
    expected_status: i32,
    evidence_directory: &Path,
) -> (i32, Vec<u8>) {
    write_new(
        &evidence_directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code={expected_status}\nexpected_stdout_bytes={}\n",
            expected_stdout.len()
        )
        .as_bytes(),
    );
    write_new(
        &evidence_directory.join("expected-native-stdout.bin"),
        expected_stdout.as_bytes(),
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
    let output = Command::new("valgrind")
        .args(&arguments)
        .output()
        .expect("run DirectPrintResult native binary under required strict Valgrind Memcheck");
    retain_command_output(
        evidence_directory,
        OsStr::new("valgrind"),
        &arguments,
        &output,
    );
    let raw_report = fs::read(&valgrind_log).unwrap_or_else(|error| {
        panic!(
            "read complete Valgrind report {}: {error}",
            valgrind_log.display()
        )
    });
    let report = text(&raw_report);
    let summary = memcheck_summary::parse(&report).rendered;
    write_new(
        &evidence_directory.join("memcheck-summary.txt"),
        summary.as_bytes(),
    );
    for loss_kind in ["definitely lost", "indirectly lost", "possibly lost"] {
        assert!(
            summary
                .lines()
                .any(|line| { line.contains(&format!("{loss_kind}: 0 bytes in 0 blocks")) }),
            "{mode} reported {loss_kind} bytes:\n{report}"
        );
    }
    assert!(
        summary
            .lines()
            .any(|line| line.contains("ERROR SUMMARY: 0 errors")),
        "{mode} reported Memcheck errors:\n{report}"
    );
    assert!(
        summary
            .lines()
            .any(|line| line.contains("still reachable:")),
        "{mode} omitted the still-reachable portion of the Memcheck breakdown:\n{report}"
    );
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "{mode} changed its exact exit status or Memcheck found an error:\n{report}\napplication stderr:\n{}",
        text(&output.stderr)
    );
    assert_eq!(
        output.stdout.as_slice(),
        expected_stdout.as_bytes(),
        "{mode} native stdout"
    );
    eprintln!("[mimi-test] complete DirectPrintResult Memcheck {mode} report:\n{report}");
    (
        output
            .status
            .code()
            .expect("validated DirectPrintResult Memcheck process status"),
        output.stdout,
    )
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn map_any_owner_v2_default_and_explicit_native_builds_are_strict_memcheck_clean() {
    let (mimi, mimi_source) = mimi_bin_with_source();
    let directory = L3RunDirectory::new();
    let artifact_mode = directory._temporary.is_none();

    if artifact_mode {
        retain_mimi_binary_identity(&mimi, mimi_source, &directory.path);
    }

    let version_directory = directory.path.join("valgrind-version");
    create_capture_directory(&version_directory);
    let version_arguments = vec![OsString::from("--version")];
    let version = Command::new("valgrind")
        .args(&version_arguments)
        .output()
        .expect("query strict Memcheck Valgrind version");
    retain_command_output(
        &version_directory,
        OsStr::new("valgrind"),
        &version_arguments,
        &version,
    );
    assert_success(&version, "Valgrind version query");

    let cases = [
        ("record", fixture(), EXPECTED_STDOUT, 0),
        (
            "empty-root",
            empty_root_fixture(),
            EMPTY_ROOT_EXPECTED_STDOUT,
            0,
        ),
        (
            "values-release",
            populated_values_fixture(),
            POPULATED_VALUES_EXPECTED_STDOUT,
            0,
        ),
        (
            "values-live-trap",
            live_values_trap_fixture(),
            POPULATED_VALUES_EXPECTED_STDOUT,
            1,
        ),
    ];
    let case_count = cases.len();
    for (case_index, (label, source, expected_stdout, expected_status)) in
        cases.into_iter().enumerate()
    {
        let case_directory = directory.path.join(format!("case-{case_index:02}-{label}"));
        create_capture_directory(&case_directory);
        let fixture_bytes = fs::read(&source)
            .unwrap_or_else(|error| panic!("read L3 fixture {}: {error}", source.display()));
        write_new(&case_directory.join("source.fixture.mimi"), &fixture_bytes);

        let receipt_directory = case_directory.join("receipt");
        create_capture_directory(&receipt_directory);
        let receipt_arguments = vec![
            OsString::from("mir"),
            OsString::from("--all"),
            OsString::from("--receipt"),
            source.as_os_str().to_owned(),
        ];
        let receipt = Command::new(&mimi)
            .args(&receipt_arguments)
            .output()
            .expect("materialize MapAnyOwner v2 MIR receipt for L3");
        retain_command_output(
            &receipt_directory,
            mimi.as_os_str(),
            &receipt_arguments,
            &receipt,
        );
        let manifest = parse_manifest(&receipt);
        let expected_digest = manifest
            .get("mir_digest")
            .expect("validated MIR receipt contains its graph digest")
            .clone();

        let mut outputs = Vec::new();
        // Exercise each admitted V2 shape through explicit and default native
        // builds. Keep the empty-root GetAny miss and empty Values snapshot in
        // Memcheck so their zero-payload owner cleanup has real L3 evidence.
        for direct in [false, true] {
            let mode = if direct {
                "explicit MIR native binary"
            } else {
                "default native binary"
            };
            let build_directory = case_directory.join(if direct {
                "explicit-build"
            } else {
                "default-build"
            });
            create_capture_directory(&build_directory);
            let binary = build_directory.join("program");
            let mut build_arguments = vec![
                OsString::from("build"),
                source.as_os_str().to_owned(),
                OsString::from("--output"),
                binary.as_os_str().to_owned(),
            ];
            if direct {
                build_arguments.push(OsString::from("--mir"));
            }
            let mut command = Command::new(&mimi);
            command.env("MIMI_ROUTE_CENSUS", "1").args(&build_arguments);
            let build = command
                .output()
                .unwrap_or_else(|error| panic!("build {mode}: {error}"));
            retain_command_output(&build_directory, mimi.as_os_str(), &build_arguments, &build);
            write_new(
                &build_directory.join("command.environment.txt"),
                b"MIMI_ROUTE_CENSUS=1\n",
            );
            assert_success(&build, mode);
            assert!(binary.is_file(), "{mode} did not leave its native binary");
            let stderr = text(&build.stderr);
            if direct {
                assert_explicit_mir_census(&stderr, &expected_digest, mode);
            } else {
                assert_default_route(&stderr, &expected_digest, mode);
            }

            let memcheck_directory = case_directory.join(if direct {
                "explicit-memcheck"
            } else {
                "default-memcheck"
            });
            create_capture_directory(&memcheck_directory);
            outputs.push(assert_strict_memcheck(
                &binary,
                mode,
                expected_stdout,
                expected_status,
                &memcheck_directory,
                artifact_mode,
            ));
        }
        assert_eq!(outputs[0], outputs[1], "default/direct native output");
    }

    write_new(
        &directory.path.join("run-summary.txt"),
        format!(
            "route_profile={ROUTE_PROFILE}\ncase_count={}\nnative_build_count={}\nmemcheck_run_count={}\ndefault_route_receipt_count={}\nexplicit_mir_census_count={}\nlegacy_fallback_count=0\n",
            case_count,
            case_count * 2,
            case_count * 2,
            case_count,
            case_count,
        )
        .as_bytes(),
    );
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_owned_any_v2_direct_print_result_default_and_explicit_native_builds_are_strict_memcheck_clean(
) {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "DirectPrintResult L3 must not open broad Map/Any production readiness"
        );
    };
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict L3 execution requires MIMI_REQUIRE_VALGRIND=1"
    );

    let (mimi, mimi_source) = mimi_bin_with_source();
    let directory = L3RunDirectory::new_direct_print_result();
    let artifact_mode = directory._temporary.is_none();

    if artifact_mode {
        retain_mimi_binary_identity(&mimi, mimi_source, &directory.path);
    }

    let version_directory = directory.path.join("valgrind-version");
    create_capture_directory(&version_directory);
    let version_arguments = vec![OsString::from("--version")];
    let version = Command::new("valgrind")
        .args(&version_arguments)
        .output()
        .expect("query strict DirectPrintResult Memcheck Valgrind version");
    retain_command_output(
        &version_directory,
        OsStr::new("valgrind"),
        &version_arguments,
        &version,
    );
    assert_success(&version, "DirectPrintResult Valgrind version query");

    let fixture_directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let cases = [
        (
            "hit-i32",
            "mir_ordinary_map_any_owner_v2_direct_print_hit_i32.mimi",
            "42\n",
            0,
        ),
        (
            "hit-string",
            "mir_ordinary_map_any_owner_v2_direct_print_hit_string.mimi",
            "borrowed String λ\n",
            0,
        ),
        (
            "miss-i32",
            "mir_ordinary_map_any_owner_v2_direct_print_miss_i32.mimi",
            "0\n",
            0,
        ),
        (
            "same-root-map-size-i32",
            "mir_ordinary_map_any_owner_v2_direct_print_same_root_map_size_i32.mimi",
            "42\n",
            1,
        ),
    ];

    let case_count = cases.len();
    assert_eq!(
        case_count, 4,
        "DirectPrintResult L3 covers all four fixtures"
    );
    for (case_index, (label, fixture_name, expected_stdout, expected_status)) in
        cases.into_iter().enumerate()
    {
        let source = fixture_directory.join(fixture_name);
        let case_directory = directory.path.join(format!("case-{case_index:02}-{label}"));
        create_capture_directory(&case_directory);

        let fixture_bytes = fs::read(&source).unwrap_or_else(|error| {
            panic!(
                "read DirectPrintResult L3 fixture {}: {error}",
                source.display()
            )
        });
        write_new(&case_directory.join("source.fixture.mimi"), &fixture_bytes);

        // Complete both compile-capable production routes and validate their
        // Checker/MIR/native receipt identities before starting either L3
        // Memcheck execution. The build census itself is the authoritative
        // profile-specific receipt; a generic `mimi mir --receipt` is not.
        let mut builds = Vec::with_capacity(2);
        for direct in [false, true] {
            let mode = if direct {
                "explicit --mir DirectPrintResult native binary"
            } else {
                "default DirectPrintResult native binary"
            };
            let build_directory = case_directory.join(if direct {
                "explicit-build"
            } else {
                "default-build"
            });
            create_capture_directory(&build_directory);

            let binary = build_directory.join("program");
            let mut build_arguments = vec![
                OsString::from("build"),
                source.as_os_str().to_owned(),
                OsString::from("--output"),
                binary.as_os_str().to_owned(),
            ];
            if direct {
                build_arguments.push(OsString::from("--mir"));
            }

            let mut command = Command::new(&mimi);
            for (key, _) in
                std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("MIMI_"))
            {
                command.env_remove(key);
            }
            command.env("MIMI_ROUTE_CENSUS", "1").args(&build_arguments);
            let build = command
                .output()
                .unwrap_or_else(|error| panic!("build {mode}: {error}"));
            retain_command_output(&build_directory, mimi.as_os_str(), &build_arguments, &build);
            write_new(
                &build_directory.join("command.environment.txt"),
                b"MIMI_ROUTE_CENSUS=1\n",
            );
            assert_success(&build, mode);
            assert!(binary.is_file(), "{mode} did not leave its native binary");

            let stderr = text(&build.stderr);
            let expected_mode = if direct { "explicit-mir" } else { "default" };
            let (mir_digest, receipt_identity) =
                assert_direct_print_result_route_census(&stderr, expected_mode, mode);
            builds.push((
                binary,
                mode.to_string(),
                build_directory,
                mir_digest,
                receipt_identity,
            ));
        }

        assert_eq!(
            builds[0].3, builds[1].3,
            "default and explicit DirectPrintResult builds must consume the same MIR graph"
        );
        assert_eq!(
            builds[0].4, builds[1].4,
            "default and explicit DirectPrintResult builds must consume the same Checker-bound route receipt"
        );

        let mut outputs = Vec::with_capacity(2);
        for (binary, mode, _build_directory, _mir_digest, _receipt_identity) in builds {
            let direct = mode.starts_with("explicit");
            let memcheck_directory = case_directory.join(if direct {
                "explicit-memcheck"
            } else {
                "default-memcheck"
            });
            create_capture_directory(&memcheck_directory);
            outputs.push(assert_direct_print_result_strict_memcheck(
                &binary,
                &mode,
                expected_stdout,
                expected_status,
                &memcheck_directory,
            ));
        }

        assert_eq!(
            &outputs[0], &outputs[1],
            "default/explicit DirectPrintResult native status and stdout differ"
        );
    }

    write_new(
        &directory.path.join("run-summary.txt"),
        format!(
            "route_profile={DIRECT_PRINT_RESULT_ROUTE_PROFILE}\nglobal_map_any_v2_production_ready=false\ncase_count={case_count}\nnative_build_count={}\nmemcheck_run_count={}\ndefault_route_receipt_count={case_count}\nexplicit_route_receipt_count={case_count}\nexplicit_mir_census_count={case_count}\nchecker_mir_build_identity_match_count={case_count}\nlegacy_fallback_count=0\n",
            case_count * 2,
            case_count * 2,
        )
        .as_bytes(),
    );
}

#[test]
#[ignore = "strict native String-result Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_owned_any_v2_computed_key_string_get_default_and_explicit_builds_are_strict_memcheck_clean(
) {
    fn capture_source(
        source: &Path,
        case_directory: &Path,
        label: &str,
        source_hash_root: &Path,
    ) -> String {
        let bytes = fs::read(source)
            .unwrap_or_else(|error| panic!("read fixture {}: {error}", source.display()));
        let captured = case_directory.join("source.fixture.mimi");
        write_new(&captured, &bytes);
        assert_eq!(
            fs::read(&captured).expect("read captured fixture"),
            bytes,
            "captured fixture bytes changed"
        );

        // Keep fixture hash commands outside native/: the suite script checks
        // exact command inventories for native builds and Memcheck runs.
        let hash = retain_sha256(
            &captured,
            &format!("{label}-fixture-source"),
            source_hash_root,
        );
        write_new(
            &case_directory.join("source-hash.expected.txt"),
            format!(
                "source_path={}\ncaptured_source_sha256={hash}\n",
                source.display()
            )
            .as_bytes(),
        );
        hash
    }

    fn receipt_digest(mimi: &Path, source: &Path, case_directory: &Path, mode: &str) -> String {
        let receipt_directory = case_directory.join("receipt");
        create_capture_directory(&receipt_directory);
        let arguments = vec![
            OsString::from("mir"),
            OsString::from("--all"),
            OsString::from("--receipt"),
            source.as_os_str().to_owned(),
        ];
        let receipt = Command::new(mimi)
            .args(&arguments)
            .output()
            .unwrap_or_else(|error| panic!("invoke {mode} MIR receipt: {error}"));
        retain_command_output(&receipt_directory, mimi.as_os_str(), &arguments, &receipt);
        parse_manifest(&receipt)
            .get("mir_digest")
            .expect("validated MIR receipt contains its graph digest")
            .clone()
    }

    fn build_and_memcheck(
        mimi: &Path,
        valgrind: &StrictValgrind,
        source: &Path,
        case_directory: &Path,
        expected_digest: &str,
        direct: bool,
    ) -> (Vec<u8>, MemcheckSummary) {
        let mode = if direct {
            "explicit --mir native build"
        } else {
            "default native build"
        };
        let build_directory = case_directory.join(if direct {
            "explicit-build"
        } else {
            "default-build"
        });
        create_capture_directory(&build_directory);
        let binary = build_directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            arguments.push(OsString::from("--mir"));
        }

        let mut command = Command::new(mimi);
        command.env("MIMI_ROUTE_CENSUS", "1").args(&arguments);
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("invoke {mode}: {error}"));
        retain_command_output(&build_directory, mimi.as_os_str(), &arguments, &build);
        write_new(
            &build_directory.join("command.environment.txt"),
            b"MIMI_ROUTE_CENSUS=1\n",
        );
        assert_success(&build, mode);
        assert!(binary.is_file(), "{mode} did not leave its native binary");

        let stderr = text(&build.stderr);
        if direct {
            assert_explicit_mir_census(&stderr, expected_digest, mode);
        } else {
            assert_computed_key_get_default_route(&stderr, expected_digest, mode);
        }

        let memcheck_directory = case_directory.join(if direct {
            "explicit-memcheck"
        } else {
            "default-memcheck"
        });
        create_capture_directory(&memcheck_directory);
        assert_strict_memcheck_with_valgrind(
            &valgrind.launcher,
            &binary,
            mode,
            "",
            0,
            &memcheck_directory,
            true,
        )
    }

    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "ComputedKeyGet must not open global Map/Any v2 readiness"
        );
    };
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict L3 execution requires MIMI_REQUIRE_VALGRIND=1"
    );

    let mimi = cargo_built_mimi_bin();
    let directory = L3RunDirectory::new();
    let artifact_mode = directory._temporary.is_none();
    if artifact_mode {
        retain_mimi_binary_identity(&mimi, "Cargo-built CARGO_BIN_EXE_mimi", &directory.path);
    }

    let source_hash_root = if artifact_mode {
        directory
            .path
            .parent()
            .expect("native evidence directory has an artifact root")
            .join("source-hashes")
    } else {
        directory.path.join("source-hashes")
    };
    create_capture_directory(&source_hash_root);

    let valgrind = record_strict_valgrind_identity(&directory.path);
    let target_source = computed_key_string_fixture();
    let control_source = computed_key_i32_control_fixture();
    let target_case = directory.path.join("case-00-owned-string");
    let control_case = directory.path.join("control-i32-computed-key-get");
    create_capture_directory(&target_case);
    create_capture_directory(&control_case);

    let target_source_sha256 = capture_source(
        &target_source,
        &target_case,
        "owned-string",
        &source_hash_root,
    );
    let control_source_sha256 = capture_source(
        &control_source,
        &control_case,
        "i32-control",
        &source_hash_root,
    );

    let target_digest =
        receipt_digest(&mimi, &target_source, &target_case, "String ComputedKeyGet");
    let control_digest = receipt_digest(&mimi, &control_source, &control_case, "i32 control");

    let (control_stdout, control_summary) = build_and_memcheck(
        &mimi,
        &valgrind,
        &control_source,
        &control_case,
        &control_digest,
        false,
    );
    assert_eq!(control_stdout.as_slice(), b"");

    let (default_stdout, default_summary) = build_and_memcheck(
        &mimi,
        &valgrind,
        &target_source,
        &target_case,
        &target_digest,
        false,
    );
    let (explicit_stdout, explicit_summary) = build_and_memcheck(
        &mimi,
        &valgrind,
        &target_source,
        &target_case,
        &target_digest,
        true,
    );
    assert_eq!(
        default_stdout, explicit_stdout,
        "default/explicit stdout differs"
    );
    assert_eq!(default_stdout.as_slice(), b"");

    let baseline_bytes = control_summary.still_reachable_bytes;
    let baseline_blocks = control_summary.still_reachable_blocks;
    for (mode, summary) in [
        ("default", &default_summary),
        ("explicit", &explicit_summary),
    ] {
        assert!(
            summary.still_reachable_bytes <= baseline_bytes,
            "{mode} retained more reachable bytes than the i32 control"
        );
        assert!(
            summary.still_reachable_blocks <= baseline_blocks,
            "{mode} retained more reachable blocks than the i32 control"
        );
    }

    let default_added_bytes = default_summary
        .still_reachable_bytes
        .saturating_sub(baseline_bytes);
    let default_added_blocks = default_summary
        .still_reachable_blocks
        .saturating_sub(baseline_blocks);
    let explicit_added_bytes = explicit_summary
        .still_reachable_bytes
        .saturating_sub(baseline_bytes);
    let explicit_added_blocks = explicit_summary
        .still_reachable_blocks
        .saturating_sub(baseline_blocks);
    assert_eq!((default_added_bytes, default_added_blocks), (0, 0));
    assert_eq!((explicit_added_bytes, explicit_added_blocks), (0, 0));

    write_new(
        &directory.path.join("still-reachable-comparison.txt"),
        format!(
            "baseline_still_reachable_bytes={baseline_bytes}\nbaseline_still_reachable_blocks={baseline_blocks}\ndefault_still_reachable_bytes={}\ndefault_still_reachable_blocks={}\ndefault_added_retained_bytes={default_added_bytes}\ndefault_added_retained_blocks={default_added_blocks}\nexplicit_still_reachable_bytes={}\nexplicit_still_reachable_blocks={}\nexplicit_added_retained_bytes={explicit_added_bytes}\nexplicit_added_retained_blocks={explicit_added_blocks}\n",
            default_summary.still_reachable_bytes,
            default_summary.still_reachable_blocks,
            explicit_summary.still_reachable_bytes,
            explicit_summary.still_reachable_blocks,
        )
        .as_bytes(),
    );
    write_new(
        &directory.path.join("run-summary.txt"),
        format!(
            "route_profile={COMPUTED_KEY_GET_ROUTE_PROFILE}\nglobal_map_any_v2_production_ready=false\ncase_count=1\nnative_build_count=2\ncontrol_native_build_count=1\ntotal_native_build_count=3\nmemcheck_run_count=3\ntarget_memcheck_run_count=2\ncontrol_memcheck_run_count=1\ndefault_route_receipt_count=1\ncontrol_route_receipt_count=1\nexplicit_mir_census_count=1\nlegacy_fallback_count=0\nstill_reachable_baseline_bytes={baseline_bytes}\nstill_reachable_baseline_blocks={baseline_blocks}\ntarget_source_sha256={target_source_sha256}\ncontrol_source_sha256={control_source_sha256}\ntarget_mir_digest={target_digest}\ncontrol_mir_digest={control_digest}\nvalgrind_launcher_path={}\nvalgrind_engine_path={}\nvalgrind_version={}\n",
            valgrind.launcher.display(),
            valgrind.engine.display(),
            valgrind.version,
        )
        .as_bytes(),
    );
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_owned_any_v2_keys_snapshot_default_and_explicit_builds_are_strict_memcheck_clean() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "KeysSnapshot L3 must not open global Map/Any v2 readiness"
        );
    };
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict L3 execution requires MIMI_REQUIRE_VALGRIND=1"
    );

    let mimi = cargo_built_mimi_bin();
    let directory = L3RunDirectory::new();
    let artifact_mode = directory._temporary.is_none();
    if artifact_mode {
        retain_mimi_binary_identity(&mimi, "Cargo-built CARGO_BIN_EXE_mimi", &directory.path);
    }

    let source_hash_root = if artifact_mode {
        directory
            .path
            .parent()
            .expect("native evidence directory has an artifact root")
            .join("source-hashes")
    } else {
        directory.path.join("source-hashes")
    };
    create_capture_directory(&source_hash_root);

    let valgrind = record_strict_valgrind_identity(&directory.path);
    let source = keys_snapshot_fixture();
    let case_directory = directory.path.join("case-00-keys-snapshot");
    create_capture_directory(&case_directory);

    let fixture_bytes = fs::read(&source)
        .unwrap_or_else(|error| panic!("read KeysSnapshot fixture {}: {error}", source.display()));
    let captured_source = case_directory.join("source.fixture.mimi");
    write_new(&captured_source, &fixture_bytes);
    assert_eq!(
        fs::read(&captured_source).expect("read captured KeysSnapshot fixture"),
        fixture_bytes,
        "captured KeysSnapshot fixture bytes changed"
    );
    let source_sha256 = retain_sha256(
        &captured_source,
        "keys-snapshot-fixture-source",
        &source_hash_root,
    );
    write_new(
        &case_directory.join("source-hash.expected.txt"),
        format!(
            "source_path={}\ncaptured_source_sha256={source_sha256}\n",
            source.display()
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
    let receipt = Command::new(&mimi)
        .args(&receipt_arguments)
        .output()
        .expect("materialize the source-bound KeysSnapshot MIR receipt");
    retain_command_output(
        &receipt_directory,
        mimi.as_os_str(),
        &receipt_arguments,
        &receipt,
    );
    let expected_digest = parse_manifest(&receipt)
        .get("mir_digest")
        .expect("validated MIR receipt contains its graph digest")
        .clone();

    let mut results = Vec::new();
    for direct in [false, true] {
        let mode = if direct {
            "explicit --mir KeysSnapshot native build"
        } else {
            "default KeysSnapshot native build"
        };
        let build_directory = case_directory.join(if direct {
            "explicit-build"
        } else {
            "default-build"
        });
        create_capture_directory(&build_directory);
        let binary = build_directory.join("program");
        let mut build_arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            build_arguments.push(OsString::from("--mir"));
        }
        let mut build_command = Command::new(&mimi);
        build_command
            .env("MIMI_ROUTE_CENSUS", "1")
            .args(&build_arguments);
        let build = build_command
            .output()
            .unwrap_or_else(|error| panic!("build {mode}: {error}"));
        retain_command_output(&build_directory, mimi.as_os_str(), &build_arguments, &build);
        write_new(
            &build_directory.join("command.environment.txt"),
            b"MIMI_ROUTE_CENSUS=1\n",
        );
        assert_success(&build, mode);
        assert!(binary.is_file(), "{mode} did not leave its native binary");

        let stderr = text(&build.stderr);
        if direct {
            assert_explicit_mir_census(&stderr, &expected_digest, mode);
        } else {
            assert_keys_snapshot_default_route(&stderr, &expected_digest, mode);
        }

        let memcheck_directory = case_directory.join(if direct {
            "explicit-memcheck"
        } else {
            "default-memcheck"
        });
        create_capture_directory(&memcheck_directory);
        results.push(assert_strict_memcheck_with_valgrind(
            &valgrind.launcher,
            &binary,
            mode,
            KEYS_SNAPSHOT_EXPECTED_STDOUT,
            0,
            &memcheck_directory,
            true,
        ));
    }
    assert_eq!(
        results[0].0, results[1].0,
        "default and explicit KeysSnapshot native stdout differ"
    );

    write_new(
        &directory.path.join("run-summary.txt"),
        format!(
            "route_profile={KEYS_SNAPSHOT_ROUTE_PROFILE}\nglobal_map_any_v2_production_ready=false\ncase_count=1\nnative_build_count=2\nmemcheck_run_count=2\ndefault_route_receipt_count=1\nexplicit_mir_census_count=1\nlegacy_fallback_count=0\ndefault_still_reachable_bytes={}\ndefault_still_reachable_blocks={}\nexplicit_still_reachable_bytes={}\nexplicit_still_reachable_blocks={}\ntarget_source_sha256={source_sha256}\ntarget_mir_digest={expected_digest}\nvalgrind_launcher_path={}\nvalgrind_engine_path={}\nvalgrind_version={}\n",
            results[0].1.still_reachable_bytes,
            results[0].1.still_reachable_blocks,
            results[1].1.still_reachable_bytes,
            results[1].1.still_reachable_blocks,
            valgrind.launcher.display(),
            valgrind.engine.display(),
            valgrind.version,
        )
        .as_bytes(),
    );
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_owned_any_v2_values_heterogeneous_string_index1_default_and_explicit_builds_are_strict_memcheck_clean(
) {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "heterogeneous Values L3 must not open global Map/Any v2 readiness"
        );
    };
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict L3 execution requires MIMI_REQUIRE_VALGRIND=1"
    );

    let mimi = cargo_built_mimi_bin();
    let directory = L3RunDirectory::new();
    let artifact_mode = directory._temporary.is_none();
    if artifact_mode {
        retain_mimi_binary_identity(&mimi, "Cargo-built CARGO_BIN_EXE_mimi", &directory.path);
    }

    let source_hash_root = if artifact_mode {
        directory
            .path
            .parent()
            .expect("native evidence directory has an artifact root")
            .join("source-hashes")
    } else {
        directory.path.join("source-hashes")
    };
    create_capture_directory(&source_hash_root);

    let valgrind = record_strict_valgrind_identity(&directory.path);
    let source = heterogeneous_values_fixture();
    let case_directory = directory.path.join("case-00-heterogeneous-values");
    create_capture_directory(&case_directory);
    let fixture_bytes = fs::read(&source).unwrap_or_else(|error| {
        panic!(
            "read heterogeneous Values fixture {}: {error}",
            source.display()
        )
    });
    let captured_source = case_directory.join("source.fixture.mimi");
    write_new(&captured_source, &fixture_bytes);
    assert_eq!(
        fs::read(&captured_source).expect("read captured heterogeneous Values fixture"),
        fixture_bytes,
        "captured heterogeneous Values fixture bytes changed"
    );
    let source_sha256 = retain_sha256(
        &captured_source,
        "heterogeneous-values-fixture-source",
        &source_hash_root,
    );
    write_new(
        &case_directory.join("source-hash.expected.txt"),
        format!(
            "source_path={}\ncaptured_source_sha256={source_sha256}\n",
            source.display()
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
    let receipt = Command::new(&mimi)
        .args(&receipt_arguments)
        .output()
        .expect("materialize the source-bound heterogeneous Values MIR receipt");
    retain_command_output(
        &receipt_directory,
        mimi.as_os_str(),
        &receipt_arguments,
        &receipt,
    );
    let expected_digest = parse_manifest(&receipt)
        .get("mir_digest")
        .expect("validated MIR receipt contains its graph digest")
        .clone();

    let mut results = Vec::new();
    for direct in [false, true] {
        let mode = if direct {
            "explicit --mir heterogeneous Values native build"
        } else {
            "default heterogeneous Values native build"
        };
        let build_directory = case_directory.join(if direct {
            "explicit-build"
        } else {
            "default-build"
        });
        create_capture_directory(&build_directory);
        let binary = build_directory.join("program");
        let mut build_arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            build_arguments.push(OsString::from("--mir"));
        }
        let mut build_command = Command::new(&mimi);
        build_command
            .env("MIMI_ROUTE_CENSUS", "1")
            .args(&build_arguments);
        let build = build_command
            .output()
            .unwrap_or_else(|error| panic!("build {mode}: {error}"));
        retain_command_output(&build_directory, mimi.as_os_str(), &build_arguments, &build);
        write_new(
            &build_directory.join("command.environment.txt"),
            b"MIMI_ROUTE_CENSUS=1\n",
        );
        assert_success(&build, mode);
        assert!(binary.is_file(), "{mode} did not leave its native binary");
        let stderr = text(&build.stderr);
        if direct {
            assert_explicit_mir_census(&stderr, &expected_digest, mode);
        } else {
            assert_heterogeneous_values_default_route(&stderr, &expected_digest, mode);
        }

        let memcheck_directory = case_directory.join(if direct {
            "explicit-memcheck"
        } else {
            "default-memcheck"
        });
        create_capture_directory(&memcheck_directory);
        results.push(assert_strict_memcheck_with_valgrind(
            &valgrind.launcher,
            &binary,
            mode,
            "true\n",
            0,
            &memcheck_directory,
            true,
        ));
    }
    assert_eq!(
        results[0].0, results[1].0,
        "default and explicit heterogeneous Values native stdout differ"
    );

    write_new(
        &directory.path.join("run-summary.txt"),
        format!(
            "route_profile={HETEROGENEOUS_VALUES_ROUTE_PROFILE}\nglobal_map_any_v2_production_ready=false\ncase_count=1\nnative_build_count=2\nmemcheck_run_count=2\ndefault_route_receipt_count=1\nexplicit_mir_census_count=1\nlegacy_fallback_count=0\ndefault_still_reachable_bytes={}\ndefault_still_reachable_blocks={}\nexplicit_still_reachable_bytes={}\nexplicit_still_reachable_blocks={}\ntarget_source_sha256={source_sha256}\ntarget_mir_digest={expected_digest}\nvalgrind_launcher_path={}\nvalgrind_engine_path={}\nvalgrind_version={}\n",
            results[0].1.still_reachable_bytes,
            results[0].1.still_reachable_blocks,
            results[1].1.still_reachable_bytes,
            results[1].1.still_reachable_blocks,
            valgrind.launcher.display(),
            valgrind.engine.display(),
            valgrind.version,
        )
        .as_bytes(),
    );
}

#[test]
#[ignore = "strict native Memcheck L3; run scripts/verify-ordinary-map-v2-l3.sh"]
fn ordinary_map_owned_any_v2_direct_tuple_get_l1_default_and_explicit_builds_are_strict_memcheck_clean(
) {
    let (mimi, mimi_source) = mimi_bin_with_source();
    let source = direct_tuple_get_l1_fixture();
    let directory = L3RunDirectory::new();
    let artifact_mode = directory._temporary.is_none();
    if artifact_mode {
        retain_mimi_binary_identity(&mimi, mimi_source, &directory.path);
    }

    let valgrind = record_strict_valgrind_identity(&directory.path);
    let source_bytes = fs::read(&source).unwrap_or_else(|error| {
        panic!(
            "read direct tuple Get L1 fixture {}: {error}",
            source.display()
        )
    });
    write_new(&directory.path.join("source.fixture.mimi"), &source_bytes);

    let mut outputs = Vec::new();
    let mut build_identities = Vec::new();
    let mut mir_digests = Vec::new();
    for direct in [false, true] {
        let mode = if direct {
            "explicit --mir direct tuple Get L1 native binary"
        } else {
            "default direct tuple Get L1 native binary"
        };
        let case_directory = directory
            .path
            .join(if direct { "explicit" } else { "default" });
        create_capture_directory(&case_directory);
        let binary = case_directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let mut command = Command::new(&mimi);
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("MIMI_"))
        {
            command.env_remove(key);
        }
        command.env("MIMI_ROUTE_CENSUS", "1").args(&arguments);
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("build {mode}: {error}"));
        retain_command_output(&case_directory, mimi.as_os_str(), &arguments, &build);
        write_new(
            &case_directory.join("command.environment.txt"),
            b"MIMI_ROUTE_CENSUS=1\n",
        );
        assert_success(&build, mode);
        assert!(
            binary.is_file(),
            "{mode} did not leave its native executable"
        );

        let (mir_digest, receipt_identity) =
            assert_direct_tuple_get_l1_route_census(&text(&build.stderr), mode);
        mir_digests.push(mir_digest);
        build_identities.push(receipt_identity);

        let memcheck_directory = case_directory.join("memcheck");
        create_capture_directory(&memcheck_directory);
        outputs.push(assert_strict_memcheck_with_valgrind(
            &valgrind.launcher,
            &binary,
            mode,
            "",
            0,
            &memcheck_directory,
            artifact_mode,
        ));
    }

    assert_eq!(
        mir_digests[0], mir_digests[1],
        "default and explicit builds must consume the same Checker-derived L1 MIR"
    );
    assert_eq!(
        build_identities[0], build_identities[1],
        "default and explicit builds must consume the same direct tuple route receipt"
    );
    assert_eq!(
        outputs[0].0, outputs[1].0,
        "default and explicit direct tuple native stdout differ"
    );

    write_new(
        &directory.path.join("run-summary.txt"),
        format!(
            "route_profile={DIRECT_TUPLE_GET_L1_ROUTE_PROFILE}\ncase_count=1\nnative_build_count=2\nmemcheck_run_count=2\ndefault_route_receipt_count=1\nexplicit_route_receipt_count=1\nlegacy_fallback_count=0\ndefault_still_reachable_bytes={}\ndefault_still_reachable_blocks={}\nexplicit_still_reachable_bytes={}\nexplicit_still_reachable_blocks={}\nmir_digest={}\nreceipt_identity_digest={}\nvalgrind_launcher_path={}\nvalgrind_engine_path={}\nvalgrind_version={}\n",
            outputs[0].1.still_reachable_bytes,
            outputs[0].1.still_reachable_blocks,
            outputs[1].1.still_reachable_bytes,
            outputs[1].1.still_reachable_blocks,
            mir_digests[0],
            build_identities[0],
            valgrind.launcher.display(),
            valgrind.engine.display(),
            valgrind.version,
        )
        .as_bytes(),
    );
}
