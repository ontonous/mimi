use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROFILE: &str = "ordinary-map-owned-get-i32-string-v1";
const SHARED_RECEIPT_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];

struct Case {
    fixture: &'static str,
    expected_stdout: &'static str,
    expected_exit: i32,
    trap_code: Option<&'static str>,
}

const CASES: &[Case] = &[
    Case {
        fixture: "mir_ordinary_map_any_get_escape.mimi",
        expected_stdout: "true\ntrue\ntrue\n0\n",
        expected_exit: 0,
        trap_code: None,
    },
    Case {
        fixture: "mir_ordinary_map_any_get_kind_mismatch.mimi",
        expected_stdout: "77\n",
        expected_exit: 1,
        trap_code: Some("E0800"),
    },
    Case {
        fixture: "mir_ordinary_map_any_get_cross_callee_division.mimi",
        expected_stdout: "77\n",
        expected_exit: 1,
        trap_code: Some("E0801"),
    },
    Case {
        fixture: "mir_ordinary_map_any_get_cross_callee_overflow.mimi",
        expected_stdout: "77\n",
        expected_exit: 1,
        trap_code: Some("E0802"),
    },
];

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
        .expect("Cargo must provide mimi for ordinary Map Any native CLI evidence")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn temporary_directory() -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "mimi-ordinary-map-any-get-cli-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("integration")
    ));
    std::fs::create_dir_all(&directory).expect("create isolated ordinary Map Any directory");
    directory
}

struct TemporaryArtifactDirectory {
    path: PathBuf,
    keep: bool,
}

impl Drop for TemporaryArtifactDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn normalize_run_output(bytes: &[u8]) -> Vec<u8> {
    let stdout = text(bytes);
    let mut lines = stdout.lines().collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.starts_with("-> ")) {
        lines.pop();
    }
    lines.join("\n").into_bytes()
}

fn assert_run_case_output(output: &Output, case: &Case, mode: &str) {
    assert_eq!(
        output.status.code(),
        Some(case.expected_exit),
        "{mode} had the wrong exit status; stdout={} stderr={}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        normalize_run_output(&output.stdout),
        case.expected_stdout.trim_end_matches('\n').as_bytes(),
        "{mode} program output changed"
    );
    if let Some(code) = case.trap_code {
        assert!(
            text(&output.stderr).contains(code),
            "{mode} did not preserve {code}: {}",
            text(&output.stderr)
        );
    }
}

fn assert_case_output(output: &Output, case: &Case, mode: &str) {
    assert_eq!(
        output.status.code(),
        Some(case.expected_exit),
        "{mode} had the wrong exit status; stdout={} stderr={}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(
        output.stdout.as_slice(),
        case.expected_stdout.as_bytes(),
        "{mode} output changed"
    );
    if let Some(code) = case.trap_code {
        assert!(
            text(&output.stderr).contains(code),
            "{mode} did not preserve {code}: {}",
            text(&output.stderr)
        );
    }
}

fn assert_no_legacy_fallback(stderr: &str, mode: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} silently selected the legacy route: {stderr}"
    );
}

fn assert_default_route(stderr: &str, expected_digest: &str, mode: &str) {
    let marker = format!("canonical route disposition: canonical ({PROFILE}) mir_digest=");
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must produce one canonical route-census record: {stderr}"
    );
    let digest = matching[0]
        .split_once("mir_digest=")
        .expect("route record has MIR digest")
        .1
        .trim();
    assert_eq!(digest, expected_digest, "{mode} route digest differs");
    assert_no_legacy_fallback(stderr, mode);
}

fn parse_manifest(output: &Output, mode: &str) -> BTreeMap<String, String> {
    assert!(
        output.status.success(),
        "{mode} did not materialize a canonical MIR receipt:\n{}\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let fields = text(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some("cli-mir-v1")
    );
    for field in SHARED_RECEIPT_FIELDS {
        let digest = fields
            .get(*field)
            .unwrap_or_else(|| panic!("{mode} receipt omitted {field}"));
        assert_eq!(digest.len(), 64, "{mode} has malformed {field}");
    }
    assert!(
        fields.contains_key("root_owners"),
        "{mode} receipt omitted its ownership-root identity"
    );
    fields
}

fn assert_verifier_receipt(stderr: &str, manifest: &BTreeMap<String, String>, mode: &str) {
    let line = stderr
        .lines()
        .find(|line| line.starts_with("canonical MIR verifier provenance: "))
        .unwrap_or_else(|| panic!("{mode} omitted canonical verifier provenance: {stderr}"));
    let fields = line
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect::<BTreeMap<_, _>>();
    for field in SHARED_RECEIPT_FIELDS {
        assert_eq!(
            fields.get(field).copied(),
            manifest.get(*field).map(String::as_str),
            "{mode} verifier receipt differs in {field}: {line}"
        );
    }
}

fn assert_memcheck(binary: &Path, case: &Case, mode: &str, available: bool) {
    if !available {
        return;
    }
    let output = Command::new("valgrind")
        .args([
            "--leak-check=full",
            "--show-leak-kinds=all",
            "--errors-for-leak-kinds=definite,indirect,possible",
            "--error-exitcode=97",
        ])
        .arg(binary)
        .output()
        .expect("run native ordinary Map Any executable under strict Memcheck");
    let report = text(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(case.expected_exit),
        "{mode} native process changed status or Memcheck found an error:\n{report}"
    );
    assert_eq!(
        output.stdout.as_slice(),
        case.expected_stdout.as_bytes(),
        "{mode} stdout"
    );
    if let Some(code) = case.trap_code {
        assert!(report.contains(code), "{mode} lost {code}: {report}");
    }
    let all_freed = report.contains("All heap blocks were freed -- no leaks are possible");
    for kind in ["definitely lost", "indirectly lost", "possibly lost"] {
        assert!(
            all_freed || report.contains(&format!("{kind}: 0 bytes in 0 blocks")),
            "{mode} reported a {kind} owner leak:\n{report}"
        );
    }
    assert!(
        report.contains("ERROR SUMMARY: 0 errors"),
        "{mode} native process had Memcheck errors:\n{report}"
    );
    eprintln!("[mimi-test] strict Memcheck {mode} report:\n{report}");
}

fn artifact_hash(path: &Path) -> String {
    let contents = std::fs::read(path).expect("read retained MapGetAny native executable");
    blake3::hash(&contents).to_hex().to_string()
}

fn hash_native_artifact_if_requested(path: &Path, keep_tmp: bool) -> Option<String> {
    keep_tmp.then(|| artifact_hash(path))
}

struct NativeArtifactTrace {
    path: PathBuf,
    keep: bool,
    hash_before_run: Option<String>,
    finished: bool,
}

impl NativeArtifactTrace {
    fn finish(mut self) {
        if self.keep {
            let hash_after_run = artifact_hash(&self.path);
            let stable = self
                .hash_before_run
                .as_ref()
                .is_some_and(|before| before == &hash_after_run);
            eprintln!(
                "[mimi-test] kept MapGetAny native binary: path={} hash_before_run={:?} hash_after_run={} stable_during_execution={stable}",
                self.path.display(),
                self.hash_before_run,
                hash_after_run
            );
            self.finished = true;
            assert!(stable, "retained native binary changed during execution");
        } else {
            std::fs::remove_file(&self.path).expect("remove native ordinary Map Any executable");
            self.finished = true;
        }
    }
}

impl Drop for NativeArtifactTrace {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if self.keep {
            match std::fs::read(&self.path) {
                Ok(contents) => {
                    let hash_after_run = blake3::hash(&contents).to_hex().to_string();
                    let stable = self
                        .hash_before_run
                        .as_ref()
                        .is_some_and(|before| before == &hash_after_run);
                    eprintln!(
                        "[mimi-test] retained MapGetAny native binary after early exit: path={} hash_before_run={:?} hash_after_run={} stable_during_execution={stable}",
                        self.path.display(),
                        self.hash_before_run,
                        hash_after_run
                    );
                }
                Err(error) => eprintln!(
                    "[mimi-test] retained MapGetAny artifact path={} but could not hash it after early exit: {error}",
                    self.path.display()
                ),
            }
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[test]
fn ordinary_map_any_get_cli_shares_receipt_across_default_direct_and_native_paths() {
    let mimi = mimi_bin();
    let valgrind_available = Command::new("valgrind")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let valgrind_required = std::env::var_os("MIMI_REQUIRE_VALGRIND").is_some();
    if !valgrind_available {
        if valgrind_required {
            panic!("MIMI_REQUIRE_VALGRIND is set but Valgrind is unavailable; L3 cannot pass");
        }
        eprintln!(
            "Valgrind unavailable; native ordinary Map Any L3 evidence is unverified (set MIMI_REQUIRE_VALGRIND=1 to fail closed)"
        );
    }
    let keep_tmp = std::env::var_os("MIMI_KEEP_TMP").is_some();
    let directory = temporary_directory();
    let _artifact_cleanup = TemporaryArtifactDirectory {
        path: directory.clone(),
        keep: keep_tmp,
    };
    if keep_tmp {
        eprintln!(
            "[mimi-test] retaining MapGetAny native artifacts in {}",
            directory.display()
        );
    }

    for case in CASES {
        let source = fixture(case.fixture);
        let receipt_output = Command::new(&mimi)
            .arg("mir")
            .arg("--receipt")
            .arg(&source)
            .output()
            .expect("materialize ordinary Map Any route receipt");
        let manifest = parse_manifest(&receipt_output, "mimi mir --receipt");
        let digest = manifest
            .get("mir_digest")
            .expect("validated MIR receipt has its graph digest")
            .clone();

        let mut run_outputs = Vec::new();
        for direct in [false, true] {
            let mut command = Command::new(&mimi);
            command.env("MIMI_ROUTE_CENSUS", "1").arg("run");
            if direct {
                command.arg("--mir");
            }
            let run = command
                .arg(&source)
                .output()
                .expect("run ordinary Map Any fixture");
            assert_run_case_output(
                &run,
                case,
                if direct {
                    "direct MIR run"
                } else {
                    "default run"
                },
            );
            let stderr = text(&run.stderr);
            assert_no_legacy_fallback(&stderr, "run");
            if direct {
                assert!(
                    !stderr.contains("canonical route disposition: legacy"),
                    "explicit MIR run fell back: {stderr}"
                );
            } else {
                assert_default_route(&stderr, &digest, "default run");
            }
            run_outputs.push(normalize_run_output(&run.stdout));
        }
        assert_eq!(run_outputs[0], run_outputs[1], "default/direct run output");

        let mut native_outputs = Vec::new();
        for direct in [false, true] {
            let binary = directory.join(format!(
                "{}-{}",
                case.fixture.trim_end_matches(".mimi"),
                if direct { "direct" } else { "default" }
            ));
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
            let build = command
                .output()
                .expect("build native ordinary Map Any fixture");
            assert!(
                build.status.success(),
                "{} build failed:\nstdout={}\nstderr={}",
                if direct { "direct MIR" } else { "default" },
                text(&build.stdout),
                text(&build.stderr)
            );
            let stderr = text(&build.stderr);
            assert_no_legacy_fallback(&stderr, "build");
            if direct {
                assert!(
                    binary.is_file(),
                    "explicit MIR did not produce a native executable"
                );
            } else {
                assert_default_route(&stderr, &digest, "default build");
            }

            let hash_before_run = hash_native_artifact_if_requested(&binary, keep_tmp);
            let artifact_trace = NativeArtifactTrace {
                path: binary.clone(),
                keep: keep_tmp,
                hash_before_run,
                finished: false,
            };
            let native = Command::new(&binary)
                .output()
                .expect("execute the native ordinary Map Any binary");
            assert_case_output(
                &native,
                case,
                if direct {
                    "direct MIR native binary"
                } else {
                    "default native binary"
                },
            );
            assert_memcheck(
                &binary,
                case,
                if direct {
                    "direct MIR native binary"
                } else {
                    "default native binary"
                },
                valgrind_available,
            );
            native_outputs.push(native.stdout);
            artifact_trace.finish();
        }
        assert_eq!(
            native_outputs[0], native_outputs[1],
            "default/direct native output"
        );
        assert_eq!(
            native_outputs[0].as_slice(),
            case.expected_stdout.as_bytes()
        );

        let mut verify_outputs = Vec::new();
        for direct in [false, true] {
            let mut command = Command::new(&mimi);
            command
                .env("MIMI_ROUTE_CENSUS", "1")
                .env("MIMI_VERBOSE", "1")
                .arg("verify");
            if direct {
                command.arg("--mir");
            }
            let verify = command
                .arg(&source)
                .output()
                .expect("verify ordinary Map Any fixture");
            assert!(
                verify.status.success(),
                "{} verify failed:\nstdout={}\nstderr={}",
                if direct { "direct MIR" } else { "default" },
                text(&verify.stdout),
                text(&verify.stderr)
            );
            assert!(
                text(&verify.stdout).contains("No contracts to verify"),
                "no-contract verification result changed: {}",
                text(&verify.stdout)
            );
            let stderr = text(&verify.stderr);
            assert_no_legacy_fallback(&stderr, "verify");
            assert_verifier_receipt(&stderr, &manifest, "verify");
            if !direct {
                assert_default_route(&stderr, &digest, "default verify");
            }
            verify_outputs.push(verify.stdout);
        }
        assert_eq!(
            verify_outputs[0], verify_outputs[1],
            "default/direct verify output"
        );
    }

    if keep_tmp {
        eprintln!(
            "[mimi-test] kept MapGetAny native directory: {}",
            directory.display()
        );
    }
}
