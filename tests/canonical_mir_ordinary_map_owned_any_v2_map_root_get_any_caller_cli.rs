use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ROUTE_PROFILE: &str = "ordinary-map-owned-any-v2-map-root-get-any-caller-v1";
const ROUTE_ADMISSION_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const NATIVE_BUILD_PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";
const VERIFIER_PROVENANCE_PREFIX: &str = "canonical MIR verifier provenance: ";
const EXPECTED_RUN_STDOUT: &[u8] = b"0\n";

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteIdentity {
    mir_digest: String,
    receipt_identity_digest: String,
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time follows UNIX epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-map-root-get-any-caller-cli-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create isolated Map-root get-any caller CLI directory {}: {error}",
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
        .join("mir_ordinary_map_owned_any_v2_map_root_get_any_caller.mimi")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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

fn cli(consumer: &str, explicit_mir: bool, source: &Path, output: Option<&Path>) -> Output {
    let mut command = Command::new(mimi_bin());
    remove_mimi_environment(&mut command);
    command
        .arg(consumer)
        .env("MIMI_ROUTE_CENSUS", "1")
        .env("MIMI_VERBOSE", "1")
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if explicit_mir {
        command.arg("--mir");
    }
    command.arg(source);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("invoke mimi {consumer}: {error}"))
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

    let admission = one_line(&stderr, ROUTE_ADMISSION_PREFIX, label);
    let values = fields(admission, ROUTE_ADMISSION_PREFIX, label);
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
        "{label} must use the dedicated Map-root get-any caller profile"
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
            "{label} native compiler must consume the dedicated profile"
        );
        assert_eq!(
            native_fields.get("profile").copied(),
            Some(ROUTE_PROFILE),
            "{label} native route profile"
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
        assert_eq!(
            native_fields.get("route_identity_digest").copied(),
            Some(receipt_identity_digest),
            "{label} native compiler receipt identity must match route admission"
        );
    }

    if consumer == "verify" {
        let provenance = one_line(&stderr, VERIFIER_PROVENANCE_PREFIX, label);
        let verifier_fields = fields(provenance, VERIFIER_PROVENANCE_PREFIX, label);
        assert_eq!(
            verifier_fields.get("profile").copied(),
            Some(ROUTE_PROFILE),
            "{label} verifier must consume the dedicated profile"
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
fn map_root_get_any_caller_default_and_explicit_routes_share_receipt() {
    let temporary = TemporaryDirectory::new();
    let source = fixture();
    let mut identities = BTreeMap::<&str, RouteIdentity>::new();

    for (consumer, explicit_mir) in [
        ("build", false),
        ("build", true),
        ("run", false),
        ("run", true),
        ("verify", false),
        ("verify", true),
    ] {
        let mode = if explicit_mir {
            "explicit --mir"
        } else {
            "default"
        };
        let label = format!("{consumer} {mode}");
        let native = (consumer == "build").then(|| {
            temporary.path().join(format!(
                "native-{consumer}-{}",
                if explicit_mir { "mir" } else { "default" }
            ))
        });
        let output = cli(consumer, explicit_mir, &source, native.as_deref());
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
                output.stdout, EXPECTED_RUN_STDOUT,
                "{label} must return the expected owned Any payload result"
            ),
            "build" => {
                let binary = native.expect("build output path");
                assert!(binary.is_file(), "{label} must produce a native executable");
                let native_run = Command::new(&binary)
                    .current_dir(env!("CARGO_MANIFEST_DIR"))
                    .output()
                    .unwrap_or_else(|error| panic!("execute {label} native artifact: {error}"));
                assert_eq!(
                    native_run.status.code(),
                    Some(0),
                    "{label} native artifact must execute:\nstdout:\n{}\nstderr:\n{}",
                    text(&native_run.stdout),
                    text(&native_run.stderr)
                );
                assert_eq!(
                    native_run.stdout, EXPECTED_RUN_STDOUT,
                    "{label} native output must match the bytecode result"
                );
                assert!(
                    native_run.stderr.is_empty(),
                    "{label} native stderr must be empty: {}",
                    text(&native_run.stderr)
                );
            }
            "verify" => {}
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
        "default and explicit --mir build/run/verify must share one MIR and receipt identity: {identities:#?}"
    );
}
