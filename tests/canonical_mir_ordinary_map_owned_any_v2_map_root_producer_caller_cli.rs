#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ROUTE_PROFILE: &str = "ordinary-map-owned-any-v2-map-root-producer-caller-v1";
const ROUTE_PREFIX: &str = "MIMI_ROUTE_CENSUS route-admission ";
const NATIVE_PREFIX: &str = "MIMI_ROUTE_CENSUS canonical-native-build ";
const ARTIFACT_DIR_ENV: &str = "MIMI_MAP_ROOT_PRODUCER_CALLER_L3_ARTIFACT_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteIdentity {
    mir_digest: String,
    receipt_identity_digest: String,
}

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mir_ordinary_map_owned_any_v2_map_root_producer_caller.mimi")
}

fn evidence_dir(label: &str) -> PathBuf {
    let root = std::env::var_os(ARTIFACT_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time follows UNIX epoch")
                .as_nanos();
            std::env::temp_dir().join(format!("mimi-map-root-v1-{}-{nonce}", std::process::id()))
        });
    fs::create_dir_all(&root).expect("create evidence root");
    let path = root.join(label);
    fs::create_dir(&path).expect("create unique test evidence directory");
    path
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn record(directory: &Path, label: &str, output: &Output) {
    let stem = directory.join(label);
    fs::write(stem.with_extension("stdout.bin"), &output.stdout).expect("record command stdout");
    fs::write(stem.with_extension("stderr.bin"), &output.stderr).expect("record command stderr");
    let status = match output.status.code() {
        Some(code) => format!("exit_code={code}\nsuccess={}\n", output.status.success()),
        None => format!(
            "exit_code=signal\nsuccess={}\nstatus={:?}\n",
            output.status.success(),
            output.status
        ),
    };
    fs::write(stem.with_extension("status.txt"), status).expect("record command status");
}

fn parse_fields(line: &str) -> BTreeMap<String, String> {
    line.split_whitespace()
        .filter_map(|item| item.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
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

fn assert_success(output: &Output, label: &str, evidence: &Path) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} failed; stdout={} stderr={}; evidence={}",
        text(&output.stdout),
        text(&output.stderr),
        evidence.display()
    );
}

fn route_identity(output: &Output, consumer: &str, explicit: bool, label: &str) -> RouteIdentity {
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback: {stderr}"
    );
    let rows = stderr
        .lines()
        .filter(|line| line.starts_with(ROUTE_PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{label} needs one route admission: {stderr}");
    let fields = parse_fields(rows[0]);
    assert_eq!(fields.get("consumer").map(String::as_str), Some(consumer));
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(if explicit { "explicit-mir" } else { "default" })
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{label} selected a different profile: {stderr}"
    );
    let mir_digest = fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{label} omitted MIR digest: {stderr}"))
        .clone();
    let receipt_identity_digest = fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{label} omitted route identity: {stderr}"))
        .clone();
    assert_digest(&mir_digest, label);
    assert_digest(&receipt_identity_digest, label);
    RouteIdentity {
        mir_digest,
        receipt_identity_digest,
    }
}

fn assert_native_receipt(output: &Output, identity: &RouteIdentity, explicit: bool, label: &str) {
    let stderr = text(&output.stderr);
    let rows = stderr
        .lines()
        .filter(|line| line.starts_with(NATIVE_PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{label} needs one native receipt: {stderr}");
    let fields = parse_fields(rows[0]);
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some(if explicit { "explicit-mir" } else { "default" })
    );
    assert_eq!(
        fields.get("route_id").map(String::as_str),
        Some(ROUTE_PROFILE)
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE)
    );
    assert_eq!(fields.get("mir_digest"), Some(&identity.mir_digest));
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&identity.receipt_identity_digest)
    );
}

fn run_cli(
    directory: &Path,
    consumer: &str,
    explicit: bool,
    source: &Path,
    output: Option<&Path>,
) -> Output {
    let mut command = Command::new(mimi_bin());
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg(consumer);
    if explicit {
        command.arg("--mir");
    }
    command.arg(source);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    let result = command
        .output()
        .unwrap_or_else(|error| panic!("start {consumer} {}: {error}", source.display()));
    record(
        directory,
        &format!(
            "{consumer}-{}",
            if explicit { "explicit-mir" } else { "default" }
        ),
        &result,
    );
    result
}

#[test]
fn map_root_producer_caller_default_and_explicit_routes_share_receipt() {
    let evidence = evidence_dir("cli-route");
    let source = fixture();
    let mut identities = Vec::new();
    for explicit in [false, true] {
        let mode = if explicit { "explicit-mir" } else { "default" };
        let run = run_cli(&evidence, "run", explicit, &source, None);
        assert_success(&run, &format!("{mode} run"), &evidence);
        assert_eq!(run.stdout, b"1\n", "{mode} VM output");
        let identity = route_identity(&run, "run", explicit, &format!("{mode} run"));

        let binary = evidence.join(format!("native-{mode}"));
        let build = run_cli(&evidence, "build", explicit, &source, Some(&binary));
        assert_success(&build, &format!("{mode} build"), &evidence);
        let build_identity = route_identity(&build, "build", explicit, &format!("{mode} build"));
        assert_eq!(build_identity, identity, "{mode} build/run identity");
        assert_native_receipt(&build, &identity, explicit, &format!("{mode} build"));
        let native = Command::new(&binary)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("run native binary");
        record(&evidence, &format!("native-{mode}"), &native);
        assert_success(&native, &format!("{mode} native"), &evidence);
        assert_eq!(native.stdout, b"1\n", "{mode} native output");
        assert!(
            native.stderr.is_empty(),
            "{mode} native stderr: {}",
            text(&native.stderr)
        );

        let verify = run_cli(&evidence, "verify", explicit, &source, None);
        assert_success(&verify, &format!("{mode} verify"), &evidence);
        let verify_identity =
            route_identity(&verify, "verify", explicit, &format!("{mode} verify"));
        assert_eq!(verify_identity, identity, "{mode} verify/run identity");
        identities.push(identity);
    }
    assert_eq!(
        identities[0], identities[1],
        "default/explicit MIR identity"
    );
    fs::write(
        evidence.join("route-summary.txt"),
        format!(
            "profile={ROUTE_PROFILE}\nmir_digest={}\nreceipt_identity_digest={}\ndefault_explicit_identity_equal=true\n",
            identities[0].mir_digest, identities[0].receipt_identity_digest
        ),
    )
    .expect("retain route summary");
}

fn assert_rejected_without_fallback(output: &Output, label: &str) {
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        !output.status.success(),
        "{label} unexpectedly succeeded: {diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: canonical ("),
        "{label} reported a production canonical route: {diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: legacy"),
        "{label} retried the legacy route: {diagnostic}"
    );
}

#[test]
fn map_root_producer_caller_cli_rejects_incomplete_and_escaping_shapes() {
    let evidence = evidence_dir("negative-routes");
    let fixture_source = fs::read_to_string(fixture()).expect("read exact root fixture");
    let missing_drop = fixture_source.replace("    drop(received_root)\n", "");
    assert_ne!(missing_drop, fixture_source);
    let second_caller = format!(
        "{fixture_source}\nfunc second_caller() -> i32 {{\n    let root = make_root()\n    let count = map_size(root)\n    drop(root)\n    count\n}}\n"
    );
    let old_main = "func main() -> i32 {\n    println(direct_caller())\n    0\n}";
    assert_eq!(fixture_source.matches(old_main).count(), 1);
    let escape = format!("const hidden_root = make_root()\n{fixture_source}");
    let escape = escape.replace(
        old_main,
        "func hidden_caller() -> i32 {\n    let root = hidden_root\n    let count = map_size(root)\n    drop(root)\n    count\n}\nfunc main() -> i32 {\n    println(direct_caller() + hidden_caller())\n    0\n}",
    );
    for (label, source) in [
        ("missing-drop", missing_drop),
        ("second-caller", second_caller),
        ("declaration-escape", escape),
    ] {
        let path = evidence.join(format!("{label}.mimi"));
        fs::write(&path, source).expect("write negative source fixture");
        for explicit in [false, true] {
            let mode = if explicit { "explicit-mir" } else { "default" };
            let output = run_cli(&evidence, "run", explicit, &path, None);
            assert_rejected_without_fallback(&output, &format!("{label} {mode}"));
        }
    }
}

#[test]
#[ignore = "strict serial native Map-root producer/caller Memcheck L3; run scripts/verify-ordinary-map-owned-any-v2-map-root-producer-caller-l3.sh"]
fn map_root_producer_caller_default_and_explicit_native_bins_pass_strict_memcheck() {
    let evidence = evidence_dir("strict-memcheck");
    let source = fixture();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut identities = Vec::new();
    for explicit in [false, true] {
        let mode = if explicit { "explicit-mir" } else { "default" };
        let binary = evidence.join(format!("native-{mode}"));
        let build = run_cli(&evidence, "build", explicit, &source, Some(&binary));
        assert_success(&build, &format!("{mode} build"), &evidence);
        let identity = route_identity(&build, "build", explicit, &format!("{mode} build"));
        assert_native_receipt(&build, &identity, explicit, &format!("{mode} build"));
        let report = evidence.join(format!("memcheck-{mode}.log"));
        let isolated_home = evidence.join(format!("valgrind-home-{mode}"));
        fs::create_dir(&isolated_home).expect("create clean Valgrind home");
        let mut valgrind = Command::new("valgrind");
        valgrind
            .current_dir(root)
            .env("HOME", &isolated_home)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env_remove("VALGRIND_OPTS")
            .env_remove("VALGRIND_LIB")
            .env_remove("LD_PRELOAD")
            .args([
                "--command-line-only=yes",
                "--default-suppressions=no",
                "--tool=memcheck",
                "--leak-check=full",
                "--show-leak-kinds=all",
                "--errors-for-leak-kinds=definite,indirect,possible",
                "--error-exitcode=97",
            ])
            .arg(format!("--log-file={}", report.display()))
            .arg(&binary);
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key.starts_with("MIMI_") {
                valgrind.env_remove(OsString::from(key.as_ref()));
            }
        }
        let output = valgrind.output().expect("run native binary under Memcheck");
        record(&evidence, &format!("memcheck-{mode}"), &output);
        assert_success(&output, &format!("{mode} strict Memcheck"), &evidence);
        assert_eq!(output.stdout, b"1\n", "{mode} Memcheck application output");
        assert!(
            output.stderr.is_empty(),
            "{mode} app stderr: {}",
            text(&output.stderr)
        );
        let raw_report = fs::read_to_string(&report).expect("read raw Valgrind report");
        let summary = memcheck_summary::parse(&raw_report);
        for expected in [
            "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        ] {
            assert!(
                summary.rendered.contains(expected),
                "{mode} Memcheck omitted {expected}: {}",
                summary.rendered
            );
        }
        fs::write(
            evidence.join(format!("memcheck-summary-{mode}.txt")),
            &summary.rendered,
        )
        .expect("write strict Memcheck summary");
        identities.push(identity);
    }
    assert_eq!(
        identities[0], identities[1],
        "default and explicit native route identities"
    );
    fs::write(
        evidence.join("l3-summary.txt"),
        format!(
            "profile={ROUTE_PROFILE}\nmir_digest={}\nreceipt_identity_digest={}\ndefault_explicit_identity_equal=true\nmemcheck_runs=2\n",
            identities[0].mir_digest, identities[0].receipt_identity_digest
        ),
    )
    .expect("write strict L3 summary");
}
