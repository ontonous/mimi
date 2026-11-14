use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ROUTE_PROFILE: &str = "ordinary-map-owner-lifecycle-observer-v1";
const EXPECTED_STDOUT: &[u8] =
    b"{\"a-number\":42,\"z-label\":\"persistent observer\"}\n{\"a-number\":42,\"z-label\":\"persistent observer\"}\n";
const PRODUCT_SOURCE: &str = r#"
        func main() -> i32 {
            let m0 = map_new()
            let m1 = map_set(m0, "name", "Alice")
            let m2 = map_set(m1, "age", 30)
            println(to_json(m2))
            println(m2)
            0
        }
        "#;
const PRODUCT_EXPECTED_STDOUT: &[u8] =
    b"{\"age\":30,\"name\":\"Alice\"}\n{\"age\":30,\"name\":\"Alice\"}\n";

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|directory| directory.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide the mimi binary for Map observer CLI evidence")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owner_observer_v1.mimi")
}

fn product_source(evidence: &EvidenceDirectory) -> PathBuf {
    let path = evidence
        .path()
        .join("dual-prod-untyped-map-to-json-and-println.mimi");
    std::fs::write(&path, PRODUCT_SOURCE).expect("retain exact product source in evidence");
    path
}

fn control_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owner_observer_v1_memcheck_control.mimi")
}

struct EvidenceDirectory(PathBuf);

impl EvidenceDirectory {
    fn new(label: &str) -> Self {
        let path = if let Some(path) = std::env::var_os("MIMI_OBSERVER_V1_EVIDENCE_DIR") {
            PathBuf::from(path)
        } else {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after UNIX epoch")
                .as_nanos();
            std::env::temp_dir().join(format!(
                "mimi-map-observer-v1-{label}-{}-{nonce}",
                std::process::id()
            ))
        };
        std::fs::create_dir(&path).expect("create unique Map observer evidence directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn record(&self, label: &str, output: &Output) {
        let stem = self.0.join(label);
        std::fs::write(stem.with_extension("stdout.bin"), &output.stdout)
            .expect("retain command stdout evidence");
        std::fs::write(stem.with_extension("stderr.bin"), &output.stderr)
            .expect("retain command stderr evidence");
        let status = match output.status.code() {
            Some(code) => format!("exit_code={code}\nsuccess={}\n", output.status.success()),
            None => format!(
                "exit_code=signal\nsuccess={}\nstatus={:?}\n",
                output.status.success(),
                output.status
            ),
        };
        std::fs::write(stem.with_extension("status.txt"), status)
            .expect("retain command status evidence");
    }

    fn cleanup(self) {
        std::fs::remove_dir_all(&self.0).expect("remove successful Map observer evidence");
    }

    fn preserve(self) {
        eprintln!(
            "MIMI_OBSERVER_V1_EVIDENCE_PRESERVED path={}",
            self.0.display()
        );
    }
}

impl Drop for EvidenceDirectory {
    fn drop(&mut self) {
        if std::thread::panicking() && self.0.exists() {
            eprintln!(
                "MIMI_OBSERVER_V1_EVIDENCE_PRESERVED path={}",
                self.0.display()
            );
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_success(output: &Output, mode: &str, evidence: &EvidenceDirectory) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{mode} must exit 0; stdout={} stderr={}; evidence={}",
        text(&output.stdout),
        text(&output.stderr),
        evidence.path().display()
    );
}

fn assert_exact_stdout(output: &Output, expected: &[u8], mode: &str, evidence: &EvidenceDirectory) {
    assert_eq!(
        output.stdout,
        expected,
        "{mode} stdout changed; stderr={}; evidence={}",
        text(&output.stderr),
        evidence.path().display()
    );
}

fn assert_no_legacy_fallback(stderr: &str, mode: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} selected a legacy fallback: {stderr}"
    );
}

fn assert_v2_readiness_stays_closed(output: &Output, mode: &str) {
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    let census = format!("{stdout}\n{stderr}");
    let readiness = census
        .split_whitespace()
        .filter_map(|token| token.strip_prefix("global_map_any_v2_production_ready="))
        .collect::<Vec<_>>();
    assert_eq!(
        readiness.len(),
        1,
        "{mode} must emit exactly one Map/Any owner-v2 readiness field: {census}"
    );
    assert_eq!(
        readiness[0], "false",
        "{mode} must keep Map/Any owner-v2 production readiness closed: {census}"
    );
}

fn fields(line: &str, mode: &str) -> BTreeMap<String, String> {
    line.split_whitespace()
        .filter(|token| token.contains('='))
        .map(|token| {
            let (key, value) = token
                .split_once('=')
                .unwrap_or_else(|| panic!("{mode} has malformed census field {token:?}"));
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

fn assert_digest(digest: &str, mode: &str) {
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{mode} has malformed canonical MIR digest {digest:?}"
    );
}

fn assert_route_admission(
    output: &Output,
    consumer: &str,
    mode_name: &str,
    label: &str,
    evidence: &EvidenceDirectory,
) -> RouteIdentity {
    let stderr = text(&output.stderr);
    assert_no_legacy_fallback(&stderr, label);
    assert!(
        !stderr.contains("map-any-owner-v2-direct-print-v1"),
        "{label} selected the separate DirectPrint V2 profile: {stderr}"
    );
    assert_v2_readiness_stays_closed(output, label);
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{label} must emit one route-admission record; evidence={}: {stderr}",
        evidence.path().display()
    );
    let observed = fields(matching[0], label);
    assert_eq!(
        observed.get("consumer").map(String::as_str),
        Some(consumer),
        "{label} consumer"
    );
    assert_eq!(
        observed.get("mode").map(String::as_str),
        Some(mode_name),
        "{label} route mode"
    );
    assert_eq!(
        observed.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{label} must use the observer-v1 checker profile"
    );
    let digest = observed
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{label} omitted mir_digest: {stderr}"));
    assert_digest(digest, label);
    let receipt_identity_digest = observed
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{label} omitted receipt_identity_digest: {stderr}"));
    assert_digest(receipt_identity_digest, label);
    RouteIdentity {
        mir_digest: digest.clone(),
        receipt_identity_digest: receipt_identity_digest.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteIdentity {
    mir_digest: String,
    receipt_identity_digest: String,
}

fn assert_route_disposition(
    stderr: &str,
    explicit_mir: bool,
    expected: &RouteIdentity,
    mode: &str,
) {
    let disposition = if explicit_mir {
        format!("canonical route disposition: explicit canonical ({ROUTE_PROFILE}) mir_digest=")
    } else {
        format!("canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest=")
    };
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with(&disposition))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must expose one exact observer-v1 route disposition: {stderr}"
    );
    let digest = matching[0]
        .strip_prefix(&disposition)
        .expect("route disposition prefix was matched");
    assert_eq!(
        digest, expected.mir_digest,
        "{mode} disposition MIR digest differs"
    );
}

fn assert_native_build_receipt(
    output: &Output,
    expected_consumer: &str,
    expected: &RouteIdentity,
    mode: &str,
) {
    let stderr = text(&output.stderr);
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one canonical native build receipt: {stderr}"
    );
    let observed = fields(matching[0], mode);
    assert_eq!(
        observed.get("consumer").map(String::as_str),
        Some(expected_consumer),
        "{mode} native build consumer"
    );
    assert_eq!(
        observed.get("route_id").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} native route id"
    );
    assert_eq!(
        observed.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} native receipt profile"
    );
    assert_eq!(
        observed.get("mir_digest").map(String::as_str),
        Some(expected.mir_digest.as_str()),
        "{mode} native build MIR digest"
    );
    assert_eq!(
        observed.get("route_identity_digest").map(String::as_str),
        Some(expected.receipt_identity_digest.as_str()),
        "{mode} native build route identity differs from route admission"
    );
}

fn assert_verifier_receipt(output: &Output, expected: &RouteIdentity, mode: &str) {
    let stderr = text(&output.stderr);
    let prefix = "canonical MIR verifier provenance: ";
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one MIR verifier provenance receipt: {stderr}"
    );
    let observed = fields(
        matching[0].strip_prefix(prefix).expect("provenance prefix"),
        mode,
    );
    assert_eq!(
        observed.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} verifier profile"
    );
    assert_eq!(
        observed.get("mir_digest").map(String::as_str),
        Some(expected.mir_digest.as_str()),
        "{mode} verifier MIR digest"
    );
    assert_eq!(
        observed.get("route_identity_digest").map(String::as_str),
        Some(expected.receipt_identity_digest.as_str()),
        "{mode} verifier route identity differs from route admission"
    );
}

fn assert_verifier_reports_reachable_capacity_failure(
    output: &Output,
    mode: &str,
    evidence: &EvidenceDirectory,
) {
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{mode} must fail closed on the reachable E0800 allocation path; stdout={stdout} stderr={stderr}; evidence={}",
        evidence.path().display()
    );
    assert!(
        stdout.contains("0/1 verified"),
        "{mode} must expose the single disproven owner result: {stdout}"
    );
    assert!(
        stderr.contains("canonical MIR body can reach trap 'E0800' under requires"),
        "{mode} must retain the reachable capacity counterexample: {stderr}"
    );
    assert!(
        stderr.contains("verification failed or was inconclusive"),
        "{mode} must keep the verifier failure visible to automation: {stderr}"
    );
}

fn assert_build_stdout(output: &Output, source: &Path, binary: &Path, mode: &str) {
    let expected = format!(
        "✓ Compiled {} → {} (executable)\n",
        source.display(),
        binary.display()
    );
    assert_eq!(
        text(&output.stdout),
        expected,
        "{mode} build stdout changed"
    );
}

#[test]
fn observer_v1_run_build_verify_use_the_same_default_and_explicit_mir_receipt() {
    let evidence = EvidenceDirectory::new("cli");
    exercise_run_build_verify_routes(&fixture(), EXPECTED_STDOUT, "fixture", &evidence);
    evidence.cleanup();
}

#[test]
fn product_observer_v1_run_build_verify_use_same_default_and_explicit_mir_receipt() {
    let evidence = EvidenceDirectory::new("product-cli");
    let source = product_source(&evidence);
    exercise_run_build_verify_routes(&source, PRODUCT_EXPECTED_STDOUT, "product", &evidence);
    evidence.cleanup();
}

fn exercise_run_build_verify_routes(
    source: &Path,
    expected_stdout: &[u8],
    scenario: &str,
    evidence: &EvidenceDirectory,
) {
    let mimi = mimi_bin();
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut identities = Vec::new();

    for explicit_mir in [false, true] {
        let mode_name = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let route_label = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let label = format!("{scenario}-{route_label}");

        let mut run_command = Command::new(&mimi);
        run_command
            .current_dir(source_root)
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("run");
        if explicit_mir {
            run_command.arg("--mir");
        }
        let run = run_command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("start {label} observer-v1 run: {error}"));
        evidence.record(&format!("{label}-run"), &run);
        assert_success(&run, &format!("{label} run"), evidence);
        assert_exact_stdout(&run, expected_stdout, &format!("{label} run"), evidence);
        let run_identity =
            assert_route_admission(&run, "run", mode_name, &format!("{label} run"), evidence);
        assert_route_disposition(
            &text(&run.stderr),
            explicit_mir,
            &run_identity,
            &format!("{label} run"),
        );

        let binary = evidence.path().join(format!("{label}-observer-native"));
        let mut build_command = Command::new(&mimi);
        build_command
            .current_dir(source_root)
            .env("MIMI_ROUTE_CENSUS", "1")
            .arg("build");
        if explicit_mir {
            build_command.arg("--mir");
        }
        let build = build_command
            .arg(source)
            .arg("--output")
            .arg(&binary)
            .output()
            .unwrap_or_else(|error| panic!("start {label} observer-v1 build: {error}"));
        evidence.record(&format!("{label}-build"), &build);
        assert_success(&build, &format!("{label} build"), evidence);
        assert_build_stdout(&build, source, &binary, &format!("{label} build"));
        let build_identity = assert_route_admission(
            &build,
            "build",
            mode_name,
            &format!("{label} build"),
            evidence,
        );
        assert_eq!(
            build_identity, run_identity,
            "{label} build/run route identity differs"
        );
        assert_native_build_receipt(&build, mode_name, &run_identity, &format!("{label} build"));
        assert!(binary.is_file(), "{label} build did not produce {binary:?}");

        let native = Command::new(&binary)
            .current_dir(source_root)
            .output()
            .unwrap_or_else(|error| panic!("run {label} native binary: {error}"));
        evidence.record(&format!("{label}-native"), &native);
        assert_success(&native, &format!("{label} native binary"), evidence);
        assert_exact_stdout(
            &native,
            expected_stdout,
            &format!("{label} native binary"),
            evidence,
        );
        assert!(
            native.stderr.is_empty(),
            "{label} native binary wrote unexpected stderr: {}; evidence={}",
            text(&native.stderr),
            evidence.path().display()
        );

        let mut verify_command = Command::new(&mimi);
        verify_command
            .current_dir(source_root)
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .arg("verify");
        if explicit_mir {
            verify_command.arg("--mir");
        }
        let verify = verify_command
            .arg(source)
            .output()
            .unwrap_or_else(|error| panic!("start {label} observer-v1 verify: {error}"));
        evidence.record(&format!("{label}-verify"), &verify);
        assert_verifier_reports_reachable_capacity_failure(
            &verify,
            &format!("{label} verify"),
            evidence,
        );
        let verify_identity = assert_route_admission(
            &verify,
            "verify",
            mode_name,
            &format!("{label} verify"),
            evidence,
        );
        assert_eq!(
            verify_identity, run_identity,
            "{label} verify/run route identity differs"
        );
        assert_verifier_receipt(&verify, &run_identity, &format!("{label} verify"));
        identities.push(run_identity);
    }

    assert_eq!(identities.len(), 2);
    assert_eq!(
        identities[0], identities[1],
        "default and explicit MIR route identities differ"
    );
}

#[derive(Debug)]
struct MemcheckSummary {
    still_reachable_line: String,
    still_reachable_bytes: u64,
    still_reachable_blocks: u64,
    registry_metadata: BTreeMap<&'static str, (u64, u64, usize)>,
}

fn assert_clean_memcheck_report(report: &str, mode: &str) -> MemcheckSummary {
    assert!(
        report.contains("Memcheck, a memory error detector"),
        "{mode} report is not a Valgrind Memcheck report: {report}"
    );
    let all_freed_with_zero_in_use = report
        .lines()
        .any(|line| line.contains("in use at exit: 0 bytes in 0 blocks"))
        && report
            .lines()
            .any(|line| line.contains("All heap blocks were freed -- no leaks are possible"));
    for category in ["definitely lost", "indirectly lost", "possibly lost"] {
        let rows = report
            .lines()
            .filter(|line| line.contains(&format!("{category}:")))
            .collect::<Vec<_>>();
        if rows.is_empty() {
            assert!(
                all_freed_with_zero_in_use,
                "{mode} Memcheck report omits {category} without proving that all heap blocks were freed: {report}"
            );
        } else {
            assert!(
                rows.iter()
                    .all(|line| line.contains(&format!("{category}: 0 bytes in 0 blocks"))),
                "{mode} Memcheck report has nonzero {category}: {report}"
            );
        }
    }
    let errors = report
        .lines()
        .filter(|line| line.contains("ERROR SUMMARY:"))
        .collect::<Vec<_>>();
    assert_eq!(
        errors.len(),
        1,
        "{mode} Memcheck report must contain one error summary: {report}"
    );
    assert!(
        errors[0].contains("ERROR SUMMARY: 0 errors"),
        "{mode} Memcheck report contains errors: {report}"
    );
    let reachable = report
        .lines()
        .filter(|line| line.contains("still reachable:"))
        .collect::<Vec<_>>();
    let still_reachable_line = match reachable.as_slice() {
        [line] => line.trim().to_owned(),
        [] if all_freed_with_zero_in_use => {
            "still reachable: 0 bytes in 0 blocks (inferred from zero bytes in use at exit and Valgrind's all-blocks-freed summary)".to_owned()
        }
        _ => panic!(
            "{mode} Memcheck report must retain one still-reachable summary row unless it proves zero bytes in use and all blocks freed: {report}"
        ),
    };
    let still_reachable_line = still_reachable_line.as_str();
    assert!(
        still_reachable_line.contains("bytes in") && still_reachable_line.contains("blocks"),
        "{mode} still-reachable summary is malformed: {report}"
    );
    let count_summary = still_reachable_line
        .split_once(" (")
        .map_or(still_reachable_line, |(summary, _)| summary);
    let counts = count_summary
        .split_once("still reachable:")
        .expect("summary line contains the still-reachable label")
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(
        counts.len(),
        5,
        "{mode} still-reachable summary has an unexpected shape: {still_reachable_line}"
    );
    assert_eq!(counts[1], "bytes", "{mode} still-reachable byte unit");
    assert_eq!(counts[2], "in", "{mode} still-reachable count separator");
    assert_eq!(counts[4], "blocks", "{mode} still-reachable block unit");
    let still_reachable_bytes = counts[0]
        .replace(',', "")
        .parse::<u64>()
        .unwrap_or_else(|error| {
            panic!("{mode} has invalid still-reachable byte count: {error}: {still_reachable_line}")
        });
    let still_reachable_blocks =
        counts[3]
            .replace(',', "")
            .parse::<u64>()
            .unwrap_or_else(|error| {
                panic!(
                "{mode} has invalid still-reachable block count: {error}: {still_reachable_line}"
            )
            });
    MemcheckSummary {
        still_reachable_line: still_reachable_line.to_owned(),
        still_reachable_bytes,
        still_reachable_blocks,
        registry_metadata: BTreeMap::new(),
    }
}

fn assert_only_runtime_registry_metadata_is_reachable(
    report: &str,
    mode: &str,
    summary: &MemcheckSummary,
) -> BTreeMap<&'static str, (u64, u64, usize)> {
    let lines = report.lines().collect::<Vec<_>>();
    let records = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.contains("bytes in ")
                .then_some(())
                .filter(|_| line.contains("are still reachable in loss record"))
                .map(|()| index)
        })
        .collect::<Vec<_>>();
    if records.is_empty() {
        assert_eq!(
            (
                summary.still_reachable_bytes,
                summary.still_reachable_blocks
            ),
            (0, 0),
            "{mode} has a still-reachable summary without auditable loss records: {report}"
        );
        return BTreeMap::new();
    }

    let mut metadata = BTreeMap::<&'static str, (u64, u64, usize)>::new();
    let mut recorded_bytes = 0u64;
    let mut recorded_blocks = 0u64;
    for (record_index, start) in records.iter().copied().enumerate() {
        let end = records
            .get(record_index + 1)
            .copied()
            .or_else(|| lines.iter().position(|line| line.contains("LEAK SUMMARY:")))
            .unwrap_or(lines.len());
        let record = lines[start..end].join("\n");
        let contexts = [
            (
                "any-owner-descriptor-defaults",
                "mimi_runtime::runtime::any_owner::Registry>::with_slot_limit",
            ),
            (
                "any-owner-generation-slots",
                "Vec<mimi_runtime::runtime::any_owner::Slot>",
            ),
            (
                "ordinary-map-root-receipts",
                "Vec<mimi_runtime::runtime::ordinary_map_domain::RegisteredRoot>",
            ),
            (
                "ordinary-map-owned-string-receipts",
                "Vec<mimi_runtime::runtime::ordinary_map_domain::RegisteredString>",
            ),
        ];
        let matching_contexts = contexts
            .iter()
            .filter(|(_, marker)| record.contains(marker))
            .collect::<Vec<_>>();
        assert_eq!(
            matching_contexts.len(),
            1,
            "{mode} still-reachable allocation is not one exact runtime registry metadata class: {record}"
        );
        let (class, _) = matching_contexts[0];

        let summary_text = lines[start]
            .splitn(3, "==")
            .nth(2)
            .expect("Valgrind loss record has a pid delimiter")
            .trim();
        let counts = summary_text.split_whitespace().collect::<Vec<_>>();
        assert!(
            counts.len() >= 5 && counts[1] == "bytes" && counts[2] == "in" && counts[4] == "blocks",
            "{mode} loss record has a malformed byte/block count: {}",
            lines[start]
        );
        let bytes = counts[0]
            .replace(',', "")
            .parse::<u64>()
            .expect("Valgrind loss record byte count is an integer");
        let blocks = counts[3]
            .replace(',', "")
            .parse::<u64>()
            .expect("Valgrind loss record block count is an integer");
        recorded_bytes = recorded_bytes
            .checked_add(bytes)
            .expect("Valgrind reachable byte total does not overflow");
        recorded_blocks = recorded_blocks
            .checked_add(blocks)
            .expect("Valgrind reachable block total does not overflow");
        let entry = metadata.entry(class).or_insert((0, 0, 0));
        entry.0 = entry
            .0
            .checked_add(bytes)
            .expect("registry metadata byte total does not overflow");
        entry.1 = entry
            .1
            .checked_add(blocks)
            .expect("registry metadata block total does not overflow");
        entry.2 += 1;
    }

    assert_eq!(
        (recorded_bytes, recorded_blocks),
        (
            summary.still_reachable_bytes,
            summary.still_reachable_blocks
        ),
        "{mode} reachable loss records do not account for the complete Memcheck summary: {report}"
    );
    assert_eq!(
        metadata.keys().copied().collect::<Vec<_>>(),
        vec![
            "any-owner-descriptor-defaults",
            "any-owner-generation-slots",
            "ordinary-map-owned-string-receipts",
            "ordinary-map-root-receipts",
        ],
        "{mode} retained an unexpected owner, payload, or runtime allocation class: {metadata:?}; report={report}"
    );
    metadata
}

fn assert_control_build_receipt(output: &Output, evidence: &EvidenceDirectory) {
    let stderr = text(&output.stderr);
    assert_no_legacy_fallback(&stderr, "Memcheck control build");
    let admissions = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        admissions.len(),
        1,
        "Memcheck control build must emit one canonical route-admission record; evidence={}: {stderr}",
        evidence.path().display()
    );
    let admission = fields(admissions[0], "Memcheck control build");
    assert_eq!(
        admission.get("consumer").map(String::as_str),
        Some("build"),
        "Memcheck control build consumer"
    );
    assert_eq!(
        admission.get("mode").map(String::as_str),
        Some("explicit-mir"),
        "Memcheck control must use the explicit canonical MIR native backend"
    );
    let digest = admission
        .get("mir_digest")
        .expect("Memcheck control route admission carries a MIR digest");
    assert_digest(digest, "Memcheck control build");
    let route_identity_digest = admission
        .get("receipt_identity_digest")
        .expect("Memcheck control route admission carries a receipt identity digest");
    assert_digest(
        route_identity_digest,
        "Memcheck control build route identity",
    );

    let receipts = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        receipts.len(),
        1,
        "Memcheck control build must emit one native backend receipt: {stderr}"
    );
    let receipt = fields(receipts[0], "Memcheck control build");
    assert_eq!(
        receipt.get("consumer").map(String::as_str),
        Some("explicit-mir"),
        "Memcheck control native build consumer"
    );
    let route_id = receipt
        .get("route_id")
        .expect("Memcheck control build has a native route id");
    assert_eq!(
        receipt.get("profile"),
        Some(route_id),
        "Memcheck control native route id/profile"
    );
    assert_eq!(
        receipt.get("mir_digest"),
        Some(digest),
        "Memcheck control native build digest"
    );
    assert_eq!(
        receipt.get("route_identity_digest"),
        Some(route_identity_digest),
        "Memcheck control native build route identity"
    );
}

fn run_memcheck(
    binary: &Path,
    label: &str,
    expected_stdout: &[u8],
    evidence: &EvidenceDirectory,
) -> MemcheckSummary {
    let report_path = evidence.path().join(format!("{label}.memcheck.log"));
    assert!(
        !report_path.exists(),
        "{label} raw Memcheck report path must be fresh: {report_path:?}"
    );
    let valgrind = Command::new("valgrind")
        .args([
            "--tool=memcheck",
            "--leak-check=full",
            "--show-leak-kinds=all",
            "--errors-for-leak-kinds=definite,indirect,possible",
            "--error-exitcode=97",
        ])
        .arg(format!("--log-file={}", report_path.display()))
        .arg(binary)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .unwrap_or_else(|error| panic!("run {label} native binary under Memcheck: {error}"));
    evidence.record(&format!("{label}-memcheck-process"), &valgrind);
    assert_success(
        &valgrind,
        &format!("{label} native Memcheck process"),
        evidence,
    );
    assert_exact_stdout(
        &valgrind,
        expected_stdout,
        &format!("{label} native Memcheck stdout"),
        evidence,
    );
    let report = std::fs::read_to_string(&report_path).unwrap_or_else(|error| {
        panic!(
            "read {label} raw Memcheck report {}: {error}; evidence={}",
            report_path.display(),
            evidence.path().display()
        )
    });
    let mut summary = assert_clean_memcheck_report(&report, label);
    summary.registry_metadata =
        assert_only_runtime_registry_metadata_is_reachable(&report, label, &summary);
    std::fs::write(
        evidence.path().join(format!("{label}.still-reachable.txt")),
        format!(
            "bytes={}\nblocks={}\n{}\nregistry_metadata={:?}\n",
            summary.still_reachable_bytes,
            summary.still_reachable_blocks,
            summary.still_reachable_line,
            summary.registry_metadata
        ),
    )
    .expect("retain the raw still-reachable summary");
    eprintln!(
        "MIMI_OBSERVER_V1_MEMCHECK mode={label} bytes={} blocks={} summary={}",
        summary.still_reachable_bytes, summary.still_reachable_blocks, summary.still_reachable_line
    );
    summary
}

#[test]
#[ignore = "strict Map observer-v1 native Memcheck gate; requires Valgrind and explicit --ignored"]
fn observer_v1_default_and_explicit_native_bins_pass_strict_memcheck_l3() {
    let evidence = EvidenceDirectory::new("memcheck");
    let version = Command::new("valgrind").arg("--version").output();
    match version {
        Ok(output) if output.status.success() => evidence.record("valgrind-version", &output),
        Ok(output) => {
            evidence.record("valgrind-version", &output);
            panic!(
                "strict observer-v1 Memcheck requires a working Valgrind; skip is forbidden; status={:?}; evidence={}",
                output.status,
                evidence.path().display()
            );
        }
        Err(error) => {
            std::fs::write(
                evidence.path().join("valgrind-unavailable.txt"),
                format!(
                    "strict observer-v1 Memcheck requires Valgrind; skip is forbidden: {error}\n"
                ),
            )
            .expect("retain missing Valgrind failure detail");
            panic!(
                "strict observer-v1 Memcheck requires Valgrind; skip is forbidden: {error}; evidence={}",
                evidence.path().display()
            );
        }
    }

    let mimi = mimi_bin();
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let control_source = control_fixture();
    let control_binary = evidence.path().join("no-map-baseline-native");
    let mut control_build_command = Command::new(&mimi);
    control_build_command
        .current_dir(source_root)
        .env("MIMI_ROUTE_CENSUS", "1")
        .arg("build")
        .arg("--mir")
        .arg(&control_source)
        .arg("--output")
        .arg(&control_binary);
    let control_build = control_build_command
        .output()
        .unwrap_or_else(|error| panic!("start explicit MIR Memcheck control build: {error}"));
    evidence.record("control-build", &control_build);
    assert_success(&control_build, "Memcheck control build", &evidence);
    assert_build_stdout(
        &control_build,
        &control_source,
        &control_binary,
        "Memcheck control build",
    );
    assert_control_build_receipt(&control_build, &evidence);
    assert!(
        control_binary.is_file(),
        "Memcheck control build did not produce {control_binary:?}"
    );
    // This control uses the same explicit MIR native adapter and the same two
    // String println calls/output bytes. It establishes the non-Map process
    // baseline. The Map route intentionally initializes process-wide,
    // generation-safe owner registries; their retained capacities are checked
    // below against exact Valgrind allocation stacks, while live owner payloads
    // and every unclassified reachable allocation remain forbidden.
    let control_summary = run_memcheck(&control_binary, "control", EXPECTED_STDOUT, &evidence);
    assert_eq!(
        (
            control_summary.still_reachable_bytes,
            control_summary.still_reachable_blocks
        ),
        (0, 0),
        "non-Map Memcheck control unexpectedly retains heap blocks: {control_summary:?}"
    );

    let product_source = product_source(&evidence);
    let scenarios = [
        ("fixture", fixture(), EXPECTED_STDOUT),
        ("product", product_source, PRODUCT_EXPECTED_STDOUT),
    ];
    let mut comparison = format!(
        "control bytes={} blocks={} summary={} registry_metadata={:?}\n",
        control_summary.still_reachable_bytes,
        control_summary.still_reachable_blocks,
        control_summary.still_reachable_line,
        control_summary.registry_metadata
    );
    for (scenario, source, expected_stdout) in scenarios {
        let mut build_identities = Vec::new();
        let mut map_summaries = Vec::new();
        for explicit_mir in [false, true] {
            let mode_name = if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            };
            let route_label = if explicit_mir {
                "explicit-mir"
            } else {
                "default"
            };
            let label = format!("{scenario}-{route_label}");
            let binary = evidence.path().join(format!("{label}-observer-native"));
            let mut build_command = Command::new(&mimi);
            build_command
                .current_dir(source_root)
                .env("MIMI_ROUTE_CENSUS", "1")
                .arg("build");
            if explicit_mir {
                build_command.arg("--mir");
            }
            let build = build_command
                .arg(&source)
                .arg("--output")
                .arg(&binary)
                .output()
                .unwrap_or_else(|error| {
                    panic!("start {label} observer-v1 Memcheck build: {error}")
                });
            evidence.record(&format!("{label}-build"), &build);
            assert_success(&build, &format!("{label} Memcheck build"), &evidence);
            assert_build_stdout(&build, &source, &binary, &format!("{label} Memcheck build"));
            let identity = assert_route_admission(
                &build,
                "build",
                mode_name,
                &format!("{label} Memcheck build"),
                &evidence,
            );
            assert_native_build_receipt(
                &build,
                mode_name,
                &identity,
                &format!("{label} Memcheck build"),
            );
            build_identities.push(identity);

            let summary = run_memcheck(&binary, &label, expected_stdout, &evidence);
            map_summaries.push((label, summary));
        }

        assert_eq!(build_identities.len(), 2);
        assert_eq!(
            build_identities[0], build_identities[1],
            "{scenario} default and explicit MIR Memcheck builds produced different route identities"
        );
        assert_eq!(
            map_summaries[0].1.still_reachable_bytes, map_summaries[1].1.still_reachable_bytes,
            "{scenario} default and explicit MIR routes retain different registry bytes"
        );
        assert_eq!(
            map_summaries[0].1.still_reachable_blocks, map_summaries[1].1.still_reachable_blocks,
            "{scenario} default and explicit MIR routes retain different registry blocks"
        );
        assert_eq!(
            map_summaries[0].1.registry_metadata, map_summaries[1].1.registry_metadata,
            "{scenario} default and explicit MIR routes retain different registry metadata classes"
        );
        for (label, summary) in &map_summaries {
            comparison.push_str(&format!(
                "{label} bytes={} blocks={} summary={} registry_metadata={:?}\n",
                summary.still_reachable_bytes,
                summary.still_reachable_blocks,
                summary.still_reachable_line,
                summary.registry_metadata
            ));
        }
    }
    std::fs::write(
        evidence.path().join("still-reachable-comparison.txt"),
        comparison,
    )
    .expect("retain all raw still-reachable summaries and baseline comparison");
    // Preserve raw Valgrind reports and exact commands after a successful L3
    // run. The evidence runner binds this directory into the immutable gate
    // artifact and verifies the required report set before it records PASS.
    evidence.preserve();
}
