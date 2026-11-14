use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const PROFILE: &str = "ordinary-map-owned-get-i32-string-v1";
const EXPECTED_STDOUT: &[u8] = b"snapshot survives\n";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mir_ordinary_map_owned_get_string_detached_result_match.mimi")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

struct EvidenceDirectory {
    path: PathBuf,
    preserve: bool,
}

impl EvidenceDirectory {
    fn new() -> Self {
        let (path, preserve) =
            if let Some(path) = std::env::var_os("MIMI_MAP_STRING_MATCH_L3_EVIDENCE_DIR") {
                (PathBuf::from(path), true)
            } else {
                let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
                (
                    std::env::temp_dir().join(format!(
                        "mimi-owned-get-string-match-l3-{}-{id}",
                        std::process::id()
                    )),
                    false,
                )
            };
        fs::create_dir(&path).unwrap_or_else(|error| {
            panic!(
                "create fresh strict L3 evidence directory {}: {error}",
                path.display()
            )
        });
        Self { path, preserve }
    }

    fn record(&self, label: &str, output: &Output) {
        fs::write(
            self.path.join(format!("{label}.stdout.bin")),
            &output.stdout,
        )
        .expect("retain process stdout");
        fs::write(
            self.path.join(format!("{label}.stderr.bin")),
            &output.stderr,
        )
        .expect("retain process stderr");
        fs::write(
            self.path.join(format!("{label}.status.txt")),
            format!(
                "success={} status={:?}\n",
                output.status.success(),
                output.status
            ),
        )
        .expect("retain process status");
    }
}

impl Drop for EvidenceDirectory {
    fn drop(&mut self) {
        if self.preserve || std::thread::panicking() {
            eprintln!("MIMI_MAP_STRING_MATCH_L3_EVIDENCE={}", self.path.display());
        } else {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn route_identity(
    output: &Output,
    expected_consumer: &str,
    expected_mode: &str,
    label: &str,
) -> (String, String, String) {
    let stderr = text(&output.stderr);
    assert!(
        output.status.success(),
        "{label} failed:\nstdout:\n{}\nstderr:\n{stderr}",
        text(&output.stdout)
    );
    if expected_mode == "default" {
        assert!(
            stderr.contains(&format!(
                "canonical route disposition: canonical ({PROFILE})"
            )),
            "{label} did not select the exact default owned-get V1 route:\n{stderr}"
        );
    } else if stderr.contains("canonical route disposition:") {
        assert!(
            stderr.contains(&format!("canonical ({PROFILE})"))
                || stderr.contains(&format!("explicit canonical ({PROFILE})")),
            "{label} reported a different canonical route:\n{stderr}"
        );
    }
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} fell back to Legacy:\n{stderr}"
    );
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{label} route-admission receipt count");
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route-admission prefix")
        .split_whitespace()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{label} has malformed route field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&expected_consumer));
    assert_eq!(fields.get("mode"), Some(&expected_mode));
    assert_eq!(fields.get("checker_profile"), Some(&PROFILE));
    let mir_digest = *fields.get("mir_digest").expect("route MIR digest");
    let route_digest = *fields
        .get("receipt_identity_digest")
        .expect("route receipt digest");
    for (name, digest) in [("MIR", mir_digest), ("route receipt", route_digest)] {
        assert!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{label} has malformed {name} digest {digest:?}"
        );
    }
    (PROFILE.into(), mir_digest.into(), route_digest.into())
}

fn assert_native_receipt(
    output: &Output,
    identity: &(String, String, String),
    mode: &str,
    label: &str,
) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{label} native receipt count: {stderr}");
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS canonical-native-build ")
        .expect("native receipt prefix")
        .split_whitespace()
        .map(|token| {
            token
                .split_once('=')
                .unwrap_or_else(|| panic!("{label} has malformed native receipt field {token:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&mode));
    assert_eq!(fields.get("route_id"), Some(&PROFILE));
    assert_eq!(fields.get("profile"), Some(&PROFILE));
    assert_eq!(fields.get("mir_digest"), Some(&identity.1.as_str()));
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&identity.2.as_str())
    );
}

#[test]
#[ignore = "strict default and explicit MIR native Map/String Memcheck L3; run scripts/verify-ordinary-map-owned-get-string-match-l3.sh"]
fn ordinary_map_owned_get_string_match_default_and_explicit_native_builds_are_strict_memcheck_clean(
) {
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict L3 execution requires MIMI_REQUIRE_VALGRIND=1"
    );
    const {
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };

    let evidence = EvidenceDirectory::new();
    let mimi = mimi_bin();
    let source = fixture();
    let version = Command::new("valgrind")
        .arg("--version")
        .output()
        .expect("strict L3 requires a working Valgrind executable");
    evidence.record("valgrind-version", &version);
    assert!(
        version.status.success(),
        "Valgrind version probe failed: {}",
        text(&version.stderr)
    );

    let mut identities = Vec::new();
    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let build_directory = evidence
            .path
            .join("native")
            .join(format!("case-00-map-owned-get-string-match-{mode}"))
            .join(if explicit_mir {
                "explicit-build"
            } else {
                "default-build"
            });
        fs::create_dir_all(&build_directory)
            .expect("create isolated native build evidence directory");
        let binary = build_directory.join("program");
        let mut build_command = Command::new(&mimi);
        build_command.env("MIMI_ROUTE_CENSUS", "1").arg("build");
        if explicit_mir {
            build_command.arg("--mir");
        }
        let build = build_command
            .arg(&source)
            .arg("--output")
            .arg(&binary)
            .output()
            .unwrap_or_else(|error| panic!("start {mode} native build: {error}"));
        evidence.record(&format!("{mode}-build"), &build);
        let identity = route_identity(&build, "build", mode, &format!("{mode} build"));
        assert_native_receipt(&build, &identity, mode, &format!("{mode} build"));
        assert!(
            binary.is_file(),
            "{mode} native build produced no executable"
        );

        let memcheck_log = evidence.path.join(format!("{mode}.memcheck.log"));
        let memcheck = Command::new("valgrind")
            .arg("--tool=memcheck")
            .arg("--leak-check=full")
            .arg("--show-leak-kinds=all")
            .arg("--errors-for-leak-kinds=definite,indirect,possible")
            .arg("--error-exitcode=97")
            .arg(format!("--log-file={}", memcheck_log.display()))
            .arg(&binary)
            .output()
            .unwrap_or_else(|error| panic!("start {mode} strict Memcheck: {error}"));
        evidence.record(&format!("{mode}-memcheck"), &memcheck);
        assert!(
            memcheck.status.success(),
            "{mode} Memcheck failed:\nstdout:\n{}\nstderr:\n{}",
            text(&memcheck.stdout),
            text(&memcheck.stderr)
        );
        assert_eq!(
            memcheck.stdout, EXPECTED_STDOUT,
            "{mode} Memcheck native output"
        );
        let report = fs::read_to_string(&memcheck_log).expect("read raw Memcheck log");
        fs::write(
            evidence.path.join(format!("{mode}.memcheck-raw.txt")),
            &report,
        )
        .expect("retain raw Memcheck report");
        let summary = memcheck_summary::parse(&report);
        assert_eq!(
            summary.still_reachable_bytes, 0,
            "{mode} retained reachable bytes: {}",
            summary.rendered
        );
        assert_eq!(
            summary.still_reachable_blocks, 0,
            "{mode} retained reachable blocks: {}",
            summary.rendered
        );
        fs::write(
            evidence.path.join(format!("{mode}.memcheck-summary.txt")),
            summary.rendered,
        )
        .expect("retain parsed Memcheck counters");
        identities.push(identity);
    }
    assert_eq!(
        identities[0], identities[1],
        "default and explicit MIR native builds differ"
    );
    fs::write(
        evidence.path.join("run-summary.txt"),
        format!(
            "profile={PROFILE}\nglobal_map_any_v2_production_ready=false\nroute_identity_digest={}\nmir_digest={}\nnative_build_count=2\nmemcheck_run_count=2\ndefinitely_lost=0\nindirectly_lost=0\npossibly_lost=0\nstill_reachable=0\nerrors=0\n",
            identities[0].2, identities[0].1
        ),
    )
    .expect("retain strict L3 summary");
}
