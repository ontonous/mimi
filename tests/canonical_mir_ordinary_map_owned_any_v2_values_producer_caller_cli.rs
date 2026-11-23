use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str =
    "ordinary-map-owned-any-v2-values-result-producer-caller-v1-one-length-return";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const NATIVE_BUILD_PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";
const VERIFIER_PROVENANCE_PREFIX: &str = "canonical MIR verifier provenance: ";
const EXPECTED_RUN_STDOUT: &[u8] = b"1\n";
const INCOMPLETE_VALUES_PRODUCER_CALLER_SOURCE: &str = r#"
func snapshot_values() -> List {
    let payload = "owned value"
    match map_new() {
        Ok(empty) => match map_set(empty, "key", payload) {
            Ok(root) => {
                let snapshot = values(root)
                drop(empty)
                drop(root)
                drop(payload)
                snapshot
            }
            Err(_) => {
                drop(empty)
                drop(payload)
                []
            }
        }
        Err(_) => {
            drop(payload)
            []
        }
    }
}

func direct_caller() -> i32 {
    let snapshot = snapshot_values()
    let first_count = snapshot.len()
    let second_count = snapshot.len()
    drop(snapshot)
    first_count + second_count
}

func main() -> i32 {
    let count = direct_caller()
    println(count)
    0
}
"#;

#[derive(Debug, PartialEq, Eq)]
struct RouteIdentity {
    mir_digest: String,
    receipt_identity_digest: String,
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mimi-values-producer-caller-cli-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated Values producer/caller CLI directory {}: {error}",
                path.display()
            )
        });
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owned_any_v2_values_producer_caller.mimi")
}

fn no_len_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owned_any_v2_values_producer_caller_no_len.mimi")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn sha256_file(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("hash fixture {}: {error}", path.display()));
    assert!(
        output.status.success(),
        "sha256sum failed for {}: {}",
        path.display(),
        text(&output.stderr)
    );
    text(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("sha256sum printed no digest for {}", path.display()))
        .to_owned()
}

fn fields<'a>(line: &'a str, prefix: &str, label: &str) -> BTreeMap<&'a str, &'a str> {
    let payload = line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{label} record is missing prefix {prefix:?}"));
    let mut result = BTreeMap::new();
    for field in payload.split_whitespace() {
        let (key, value) = field
            .split_once('=')
            .unwrap_or_else(|| panic!("{label} contains malformed field {field:?}"));
        assert!(
            result.insert(key, value).is_none(),
            "{label} repeats route field {key}"
        );
    }
    result
}

fn one_line<'a>(stderr: &'a str, prefix: &str, label: &str) -> &'a str {
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

fn assert_digest(value: &str, label: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} must be a lowercase SHA-256 digest, got {value:?}"
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

fn cli_for_fixture(mode: &str, explicit_mir: bool, output: Option<&Path>, source: &Path) -> Output {
    let mut command = Command::new(mimi_bin());
    remove_mimi_environment(&mut command);
    command
        .arg(mode)
        .arg(source)
        .env("MIMI_ROUTE_CENSUS", "1")
        .env("MIMI_VERBOSE", "1")
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if explicit_mir {
        command.arg("--mir");
    }
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("invoke mimi {mode}: {error}"))
}

fn assert_success(output: &Output, label: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} must succeed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn route_identity(
    output: &Output,
    consumer: &str,
    explicit_mir: bool,
    label: &str,
) -> RouteIdentity {
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback:\n{stderr}"
    );
    assert!(
        !stderr.contains("MIR-COVERAGE-001"),
        "{label} lost its canonical route admission:\n{stderr}"
    );

    let admission = one_line(&stderr, ROUTE_ADMISSION_PREFIX, label);
    let values = fields(admission, ROUTE_ADMISSION_PREFIX, label);
    let route_field_order = admission
        .strip_prefix(ROUTE_ADMISSION_PREFIX)
        .expect("route admission prefix")
        .split_whitespace()
        .map(|field| field.split_once('=').expect("route field key=value").0)
        .collect::<Vec<_>>();
    let expected_route_fields: &[&str] = if consumer == "build" {
        &[
            "consumer",
            "mode",
            "checker_profile",
            "build_route_profile",
            "mir_digest",
            "mir_route_identity_digest",
            "receipt_identity_digest",
            "build_route_identity_digest",
        ]
    } else {
        &[
            "consumer",
            "mode",
            "checker_profile",
            "mir_digest",
            "receipt_identity_digest",
        ]
    };
    assert_eq!(
        route_field_order.as_slice(),
        expected_route_fields,
        "{label} route census field contract changed"
    );
    assert_eq!(values.get("consumer").copied(), Some(consumer), "{label}");
    assert_eq!(
        values.get("mode").copied(),
        Some(if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        }),
        "{label}"
    );
    assert_eq!(
        values.get("checker_profile").copied(),
        Some(ROUTE_PROFILE),
        "{label} must use the dedicated producer/caller Checker profile"
    );
    let mir_digest = values
        .get("mir_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted the canonical MIR digest"));
    let receipt_identity_digest = values
        .get("receipt_identity_digest")
        .copied()
        .unwrap_or_else(|| panic!("{label} omitted the route receipt identity digest"));
    assert_digest(mir_digest, &format!("{label} MIR digest"));
    assert_digest(
        receipt_identity_digest,
        &format!("{label} receipt identity digest"),
    );

    if consumer == "build" {
        let native = one_line(&stderr, NATIVE_BUILD_PREFIX, label);
        let native_fields = fields(native, NATIVE_BUILD_PREFIX, label);
        assert_eq!(
            native_fields.get("route_id").copied(),
            Some(ROUTE_PROFILE),
            "{label} native compiler must consume the dedicated receipt"
        );
        assert_eq!(
            native_fields.get("profile").copied(),
            Some(ROUTE_PROFILE),
            "{label} native receipt profile"
        );
        assert_eq!(
            native_fields.get("consumer").copied(),
            Some(if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            }),
            "{label} native consumer mode"
        );
        assert_eq!(
            native_fields.get("mir_digest").copied(),
            Some(mir_digest),
            "{label} native compiler MIR digest must match route admission"
        );
    }

    if consumer == "verify" {
        let provenance = one_line(&stderr, VERIFIER_PROVENANCE_PREFIX, label);
        let verifier_fields = fields(provenance, VERIFIER_PROVENANCE_PREFIX, label);
        assert_eq!(
            verifier_fields.get("profile").copied(),
            Some(ROUTE_PROFILE),
            "{label} verifier must consume the dedicated receipt"
        );
        assert_eq!(
            verifier_fields.get("mir_digest").copied(),
            Some(mir_digest),
            "{label} verifier MIR digest must match route admission"
        );
        for digest in [
            "type_desc_digest",
            "abi_digest",
            "ffi_digest",
            "ownership_digest",
            "flow_transition_digest",
            "source_hash",
        ] {
            let value = verifier_fields
                .get(digest)
                .unwrap_or_else(|| panic!("{label} verifier provenance omitted {digest}"));
            assert_digest(value, &format!("{label} verifier {digest}"));
        }
    }

    RouteIdentity {
        mir_digest: mir_digest.to_owned(),
        receipt_identity_digest: receipt_identity_digest.to_owned(),
    }
}

#[test]
// The caller observes only `len()` and then drops the whole snapshot; the
// route does not claim payload access or cross-call indexed unpack.
fn values_result_producer_caller_cli_routes_fail_closed_and_share_receipt() {
    let source = fixture();
    assert_eq!(
        sha256_file(&source),
        "79a48cbba5e154452f3f6bfb1ca5dde41b86b89689df350f2dad17169753519e",
        "the production CLI route is bound to the complete Result producer/caller fixture"
    );
    for (case, source, expected_stdout) in [("result-one-len", source, EXPECTED_RUN_STDOUT)] {
        let temporary = TemporaryDirectory::new();
        let mut identities = BTreeMap::<&str, RouteIdentity>::new();

        for (consumer, explicit_mir) in [
            ("build", false),
            ("build", true),
            ("run", false),
            ("run", true),
            ("verify", false),
            ("verify", true),
        ] {
            let label = format!(
                "{case} {consumer} {}",
                if explicit_mir {
                    "explicit --mir"
                } else {
                    "default"
                }
            );
            let native_output = (consumer == "build").then(|| {
                temporary.path().join(format!(
                    "native-{case}-{consumer}-{}",
                    if explicit_mir { "mir" } else { "default" }
                ))
            });
            let output = cli_for_fixture(consumer, explicit_mir, native_output.as_deref(), &source);
            assert_success(&output, &label);
            let identity = route_identity(&output, consumer, explicit_mir, &label);
            identities.insert(
                match (consumer, explicit_mir) {
                    ("build", false) => "build-default",
                    ("build", true) => "build-mir",
                    ("run", false) => "run-default",
                    ("run", true) => "run-mir",
                    ("verify", false) => "verify-default",
                    ("verify", true) => "verify-mir",
                    _ => unreachable!(),
                },
                identity,
            );

            match consumer {
                "run" => assert_eq!(
                    output.stdout, expected_stdout,
                    "{label} must execute the returned snapshot with the expected scalar result"
                ),
                "verify" => assert_eq!(
                    text(&output.stdout),
                    format!("No contracts to verify in {}\n", source.display()),
                    "{label} must retain the fixture's empty contract request"
                ),
                "build" => {
                    let native = native_output.expect("build output path");
                    assert!(native.is_file(), "{label} must produce a native executable");
                    let native_run = Command::new(&native)
                        .output()
                        .unwrap_or_else(|error| panic!("execute {label} artifact: {error}"));
                    assert_eq!(
                        native_run.status.code(),
                        Some(0),
                        "{label} native artifact must execute:\nstdout:\n{}\nstderr:\n{}",
                        text(&native_run.stdout),
                        text(&native_run.stderr)
                    );
                    assert_eq!(
                        native_run.stdout, expected_stdout,
                        "{label} native execution must match the bytecode result"
                    );
                }
                _ => unreachable!(),
            }
        }

        let first = identities
            .values()
            .next()
            .expect("six route identities were captured");
        assert_eq!(identities.len(), 6);
        assert!(
            identities.values().all(|identity| identity == first),
            "{case} build/run/verify default and explicit --mir must share one MIR and receipt identity: {identities:#?}"
        );
    }
}

#[test]
fn result_values_producer_without_len_is_rejected_for_default_and_explicit_consumers() {
    let temporary = TemporaryDirectory::new();
    let source = no_len_fixture();
    assert_eq!(
        sha256_file(&source),
        "9a5adccef9e6757509355eae7fb161a0291c05f61e4efe03afc26654cad4670c",
        "the one-length omission remains a source-bound route negative"
    );

    for (consumer, explicit_mir) in [
        ("build", false),
        ("build", true),
        ("run", false),
        ("run", true),
        ("verify", false),
        ("verify", true),
    ] {
        let label = format!(
            "Result producer without len: {consumer} {}",
            if explicit_mir {
                "explicit --mir"
            } else {
                "default"
            }
        );
        let native_output = (consumer == "build").then(|| {
            temporary.path().join(format!(
                "rejected-no-len-{consumer}-{}",
                if explicit_mir { "mir" } else { "default" }
            ))
        });
        let output = cli_for_fixture(consumer, explicit_mir, native_output.as_deref(), &source);
        assert_ne!(
            output.status.code(),
            Some(0),
            "{label} must reject the missing exact length observation:\nstdout:\n{}\nstderr:\n{}",
            text(&output.stdout),
            text(&output.stderr)
        );
        let stderr = text(&output.stderr);
        assert!(
            stderr.contains("MIR-COVERAGE-001"),
            "{label} must reject during canonical MIR coverage:\n{stderr}"
        );
        assert!(
            stderr.contains("incomplete ordinary Map/Any owner-v2 attempt")
                && stderr.contains("unreceipted Values producer/caller"),
            "{label} must identify the absent exact producer/caller receipt:\n{stderr}"
        );
        for forbidden in [
            "canonical route disposition",
            "MIMI_ROUTE_CENSUS route-admission",
            NATIVE_BUILD_PREFIX,
            VERIFIER_PROVENANCE_PREFIX,
        ] {
            assert!(
                !stderr.contains(forbidden),
                "{label} must not choose legacy/generic/native/verifier behavior ({forbidden:?}):\n{stderr}"
            );
        }
        assert!(
            output.stdout.is_empty(),
            "{label} must reject before user-visible output:\n{}",
            text(&output.stdout)
        );
        if let Some(native) = native_output {
            assert!(
                !native.exists(),
                "{label} must reject before writing native artifact {}",
                native.display()
            );
        }
    }
}

#[test]
fn incomplete_values_producer_caller_intent_never_uses_legacy_or_generic_route() {
    let temporary = TemporaryDirectory::new();
    let source = temporary
        .path()
        .join("incomplete_values_producer_caller.mimi");
    fs::write(&source, INCOMPLETE_VALUES_PRODUCER_CALLER_SOURCE).unwrap_or_else(|error| {
        panic!(
            "write incomplete Values producer/caller fixture {}: {error}",
            source.display()
        )
    });

    for (consumer, explicit_mir) in [
        ("build", false),
        ("build", true),
        ("run", false),
        ("run", true),
        ("verify", false),
        ("verify", true),
    ] {
        let label = format!(
            "{} {}",
            consumer,
            if explicit_mir {
                "explicit --mir"
            } else {
                "default"
            }
        );
        let native_output = (consumer == "build").then(|| {
            temporary.path().join(format!(
                "rejected-native-{}-{}",
                consumer,
                if explicit_mir { "mir" } else { "default" }
            ))
        });
        let output = cli_for_fixture(consumer, explicit_mir, native_output.as_deref(), &source);
        assert_ne!(
            output.status.code(),
            Some(0),
            "{label} must reject incomplete Values producer/caller intent:\nstdout:\n{}\nstderr:\n{}",
            text(&output.stdout),
            text(&output.stderr)
        );
        let stderr = text(&output.stderr);
        assert!(
            stderr.contains("MIR-COVERAGE-001"),
            "{label} must report the canonical route coverage rejection:\n{stderr}"
        );
        assert!(
            stderr.contains("incomplete ordinary Map/Any owner-v2 attempt"),
            "{label} must reject before legacy or generic collection dispatch:\n{stderr}"
        );
        assert!(
            stderr.contains("unreceipted Values producer/caller"),
            "{label} must identify the missing exact producer/caller receipt:\n{stderr}"
        );
        for forbidden in [
            "canonical route disposition",
            "MIMI_ROUTE_CENSUS route-admission",
            NATIVE_BUILD_PREFIX,
            VERIFIER_PROVENANCE_PREFIX,
        ] {
            assert!(
                !stderr.contains(forbidden),
                "{label} must not select or report a legacy, generic, native, or verifier route ({forbidden:?}):\n{stderr}"
            );
        }
        assert!(
            output.stdout.is_empty(),
            "{label} must fail before user-visible execution or verification output:\n{}",
            text(&output.stdout)
        );
        if let Some(native) = native_output {
            assert!(
                !native.exists(),
                "{label} must reject before writing a native artifact at {}",
                native.display()
            );
        }
    }
}
