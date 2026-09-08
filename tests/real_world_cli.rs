// ============================================================
// Real-world Mimi programs — CLI-driven MCDD regression suite
// ============================================================
//
// This integration test discovers every `.mimi` program under
// `tests/real_world/` (plus `projects/consumer/main.mimi`) and runs it
// through the actual `mimi run` and `mimi build` CLI paths. It is the
// Cargo-facing counterpart to `tests/real_world/run_suite.py`.
//
// Programs whose `main()` returns 0 are considered passing. Known gaps
// are listed in `KNOWN_GAPS`; failures there are reported but do not
// fail the test, so the suite can be used as a CI gate while still
// documenting real-world limitations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mimi_bin() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_mimi") {
        return PathBuf::from(path);
    }

    // Cargo does not expose CARGO_BIN_EXE_mimi for every custom-target-dir
    // invocation.  The integration test itself still lives beside the
    // matching target directory, so prefer that binary over a stale
    // workspace target/debug/mimi.
    if let Ok(test_exe) = std::env::current_exe() {
        if let Some(target_debug) = test_exe.parent().and_then(Path::parent) {
            let candidate = target_debug.join("mimi");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    project_root().join("target").join("debug").join("mimi")
}

fn can_link() -> bool {
    static CAN_LINK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN_LINK.get_or_init(|| Command::new("cc").arg("--version").output().is_ok())
}

/// Files that are expected to fail because they exercise known
/// language or codegen gaps. Keep this list minimal and aligned with
/// `tests/real_world/RESULTS.md`.
/// Generic List construction with managed or nested elements is intentionally
/// fail-closed while the Canonical MIR construction island only proves the
/// single Copy-scalar shape (S105). Keep this fixture visible as a known gap so
/// the suite records the boundary instead of allowing a legacy fallback.
const KNOWN_GAPS: &[&str] = &["core_generics_return_abi.mimi"];

/// Programs whose feature contract is intentionally interpreter-only. Keep in
/// lockstep with `tests/real_world/run_suite.py`.
const INTERPRETER_ONLY: &[&str] = &["flow_test_macros.mimi"];

fn is_known_gap(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    KNOWN_GAPS.contains(&name)
}

fn normalize_run_output(s: &str) -> String {
    let mut lines: Vec<&str> = s.lines().collect();
    if lines.last().is_some_and(|l| l.starts_with("-> ")) {
        lines.pop();
    }
    lines.join("\n")
}

fn parse_route_receipt_manifest(stdout: &[u8]) -> BTreeMap<String, String> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("mimi-mir-route-manifest-v1"),
        "unexpected route receipt manifest header"
    );
    lines
        .map(|line| {
            line.split_once('=')
                .unwrap_or_else(|| panic!("malformed route receipt manifest line: {line}"))
        })
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn checked_route_receipt(path: &Path) -> mimi::core::mir::CanonicalMirRouteReceipt {
    let source = fs::read_to_string(path).expect("read checked route fixture");
    let tokens = mimi::lexer::Lexer::new(&source)
        .tokenize()
        .expect("route matrix fixture must tokenize");
    let file = mimi::loader::parser_for_path(tokens, path)
        .expect("route matrix fixture parser setup")
        .parse_file()
        .expect("route matrix fixture must parse");
    let mut file = if !file.imports.is_empty() {
        let base_dir = path.parent().expect("route fixture parent").to_path_buf();
        let mut loader = mimi::loader::ModuleLoader::new(base_dir);
        loader
            .load_main_with_file(path, file)
            .expect("route matrix imports must load");
        loader.merge_all().expect("route matrix imports must merge")
    } else {
        file
    };
    mimi::loader::merge_prelude_into(&mut file);
    let checked = mimi::core::check_program(&file).expect("route matrix fixture must typecheck");
    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<std::collections::HashSet<_>>();
    let route = mimi::core::mir::materialize_canonical_mir_route(&checked, Some(&excluded_sources))
        .expect("route matrix fixture must materialize canonical MIR");
    route.program.route_receipt("cli-mir-v1")
}

fn run_mimi_run_out(src: &Path) -> Result<String, String> {
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(src)
        .output()
        .map_err(|e| format!("failed to spawn mimi run: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(format!("mimi run failed\n{stderr}\n{stdout}"));
    }
    Ok(normalize_run_output(&stdout))
}

fn run_mimi_build_and_exec(src: &Path) -> Result<String, String> {
    let dir = std::env::temp_dir();
    let stem = src.file_stem().expect("src has stem").to_string_lossy();
    let binary = dir.join(format!("mimi_rw_{}_{}", std::process::id(), stem));

    let build_output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(src)
        .arg("-o")
        .arg(&binary)
        .output()
        .map_err(|e| format!("failed to spawn mimi build: {e}"))?;
    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        let _ = fs::remove_file(&binary);
        return Err(format!("mimi build failed\n{stderr}"));
    }

    let exec_output = Command::new(&binary)
        .output()
        .map_err(|e| format!("failed to run compiled binary: {e}"))?;
    let _ = fs::remove_file(&binary);
    if exec_output.status.success() {
        Ok(String::from_utf8_lossy(&exec_output.stdout)
            .trim_end()
            .to_string())
    } else {
        Err(format!(
            "compiled binary exited with {}",
            exec_output.status
        ))
    }
}

#[test]
fn canonical_native_scalar_ffi_executes_checker_owned_symbol() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let source = project_root().join("tests/fixtures/mir_scalar_ffi_labs.mimi");
    let dir = std::env::temp_dir().join(format!(
        "mimi_scalar_ffi_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI output directory");
    let binary = dir.join("labs");
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("spawn canonical native FFI build");
    assert!(
        build.status.success(),
        "canonical native FFI build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&binary)
        .output()
        .expect("run canonical native FFI binary");
    assert!(
        run.status.success(),
        "canonical native FFI binary failed\nstderr:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "42");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_runtime_requires_and_skip_flag_are_observable() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_requires_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("requires.mimi");
    let binary = dir.join("requires");
    for explicit_mir in [true, false] {
        let mir_flag: &[&str] = if explicit_mir { &["--mir"] } else { &[] };
        for (argument, helper) in [(42, false), (-1, false), (-1, true)] {
            let wrapper = if helper {
                "func assume_positive(x: i64) -> i64 { requires: x > 0\n labs(x) }"
            } else {
                ""
            };
            let call = if helper { "assume_positive" } else { "labs" };
            fs::write(&source, format!(
            "extern \"C\" {{ func labs(x: i64) -> i64 requires: x > 0; }}\n{wrapper}\nfunc main() -> i64 {{ println({call}({argument} as i64)); 0 }}"
        )).unwrap();
            let verification = Command::new(mimi_bin())
                .current_dir(project_root())
                .arg("verify")
                .args(mir_flag)
                .arg(&source)
                .env("MIMI_VERBOSE", "1")
                .output()
                .unwrap();
            // A helper's requires is a conditional proof assumption. Runtime
            // checking must still reject the invalid actual input below.
            assert_eq!(
                verification.status.success(),
                argument > 0 || helper,
                "{}",
                String::from_utf8_lossy(&verification.stderr)
            );
            let verified_stdout = String::from_utf8_lossy(&verification.stdout);
            assert!(
                verified_stdout.contains("canonical MIR extern requires contract")
                    || String::from_utf8_lossy(&verification.stderr)
                        .contains("canonical MIR extern requires contract"),
                "{verified_stdout}"
            );
            assert!(!String::from_utf8_lossy(&verification.stderr)
                .contains("canonical route disposition: legacy"));
            let static_build = Command::new(mimi_bin())
                .current_dir(project_root())
                .args(["build", "--verify-ffi", "--emit-ir"])
                .args(mir_flag)
                .arg(&source)
                .output()
                .unwrap();
            assert_eq!(
                static_build.status.success(),
                argument > 0 || helper,
                "{}",
                String::from_utf8_lossy(&static_build.stderr)
            );
            let run = Command::new(mimi_bin())
                .current_dir(project_root())
                .arg("run")
                .args(mir_flag)
                .env("MIMI_VERBOSE", "1")
                .arg(&source)
                .env_remove("MIMI_FFI_LIB")
                .output()
                .unwrap();
            assert!(!String::from_utf8_lossy(&run.stderr)
                .contains("canonical route disposition: legacy"));
            let expected_stdout = if argument > 0 { "42\n" } else { "" };
            assert_eq!(
                run.status.success(),
                argument > 0,
                "{}",
                String::from_utf8_lossy(&run.stderr)
            );
            assert_eq!(run.stdout, expected_stdout.as_bytes());
            if argument < 0 {
                assert!(String::from_utf8_lossy(&run.stderr).contains("[E0808]"));
                let skipped = Command::new(mimi_bin())
                    .current_dir(project_root())
                    .arg("run")
                    .args(mir_flag)
                    .arg("--skip-verify-ffi")
                    .arg(&source)
                    .env_remove("MIMI_FFI_LIB")
                    .output()
                    .unwrap();
                assert!(
                    skipped.status.success(),
                    "{}",
                    String::from_utf8_lossy(&skipped.stderr)
                );
                assert_eq!(skipped.stdout, b"1\n");
            }
            let build = Command::new(mimi_bin())
                .current_dir(project_root())
                .arg("build")
                .args(mir_flag)
                .env("MIMI_VERBOSE", "1")
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .output()
                .unwrap();
            assert!(
                build.status.success(),
                "{}",
                String::from_utf8_lossy(&build.stderr)
            );
            assert!(!String::from_utf8_lossy(&build.stderr)
                .contains("canonical route disposition: legacy"));
            let native = Command::new(&binary).output().unwrap();
            assert_eq!(
                native.status.success(),
                argument > 0,
                "{}",
                String::from_utf8_lossy(&native.stderr)
            );
            assert_eq!(native.stdout, expected_stdout.as_bytes());
            if argument < 0 {
                assert!(String::from_utf8_lossy(&native.stderr).contains("[E0808]"));
            }
        }
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_default_cli_transports_all_abis_with_and_without_contracts() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_cli_abi_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let library = dir.join("ffi.so");
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(project_root().join("tests/fixtures/mir_scalar_ffi_abi.c"))
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );
    for contracts in [true, false] {
        let mut source = include_str!("fixtures/mir_scalar_ffi_abi.mimi").to_owned();
        if !contracts {
            for requires in [
                " requires: x != 0",
                " requires: x || not x",
                " requires: x > 0",
            ] {
                source = source.replace(requires, "");
            }
        }
        let path = dir.join("abi.mimi");
        fs::write(&path, source).unwrap();
        let run = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("run")
            .arg(&path)
            .env("MIMI_VERBOSE", "1")
            .env("MIMI_FFI_LIB", &library)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(
            run.stdout,
            b"-2147483647\n2147483647\n4294967296\ntrue\nfalse\n1\n42\n"
        );
        let run_stderr = String::from_utf8_lossy(&run.stderr);
        assert!(!run_stderr.contains("canonical route disposition: legacy"));
        assert!(!run_stderr.contains("FFI contract verification is disabled"));
        // The CLI does not accept an extra native library link argument.
        // Exercise its production emitter here; direct native linking against
        // this same C fixture is checked in canonical_scalar_ffi.rs.
        let build = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(["build", "--emit-ir"])
            .arg(&path)
            .env("MIMI_VERBOSE", "1")
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let ir = String::from_utf8_lossy(&build.stdout);
        for symbol in [
            "mir_ffi_i32",
            "mir_ffi_i64",
            "mir_ffi_bool",
            "mir_ffi_f64",
            "mir_ffi_store",
        ] {
            assert!(ir.contains(symbol), "missing emitted ABI {symbol}");
        }
        assert!(
            !String::from_utf8_lossy(&build.stderr).contains("canonical route disposition: legacy")
        );
        let verify = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("verify")
            .arg(&path)
            .env("MIMI_VERBOSE", "1")
            .output()
            .unwrap();
        assert!(
            verify.status.success(),
            "{}",
            String::from_utf8_lossy(&verify.stderr)
        );
        let output = String::from_utf8_lossy(&verify.stdout);
        if contracts {
            assert!(
                output.contains("canonical MIR extern requires contract proven"),
                "{output}"
            );
        } else {
            assert!(output.contains("No contracts to verify"), "{output}");
        }
        assert!(!String::from_utf8_lossy(&verify.stderr)
            .contains("canonical route disposition: legacy"));
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn canonical_scalar_ffi_default_and_explicit_mir_preserve_remainder_status() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_remainder_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create remainder CLI fixture directory");
    let library = dir.join("ffi.so");
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(project_root().join("tests/fixtures/mir_scalar_ffi_abi.c"))
        .arg("-o")
        .arg(&library)
        .output()
        .expect("compile scalar remainder FFI fixture");
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );

    for (label, contract, argument, expected_success, expected_stdout, expected_code) in [
        (
            "negative-sign",
            "result / 3 == -2 and result % 3 == -1",
            "-7 as i64",
            true,
            "-7\n",
            None,
        ),
        (
            "remainder-by-zero",
            "result % x == 0",
            "0 as i64",
            false,
            "",
            Some("E0801"),
        ),
    ] {
        let source = dir.join(format!("{label}.mimi"));
        fs::write(
            &source,
            format!(
                "extern \"C\" {{ func mir_ffi_i64(x: i64) -> i64 ensures: {contract}; }}\nfunc main() -> i64 {{ println(mir_ffi_i64({argument})); 0 }}\n"
            ),
        )
        .expect("write remainder CLI source");
        for explicit_mir in [false, true] {
            let mut command = Command::new(mimi_bin());
            command.current_dir(project_root()).arg("run");
            if explicit_mir {
                command.arg("--mir");
            }
            let output = command
                .arg(&source)
                .env("MIMI_FFI_LIB", &library)
                .output()
                .unwrap_or_else(|error| panic!("{label} {:?}: {error}", explicit_mir));
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.success(),
                expected_success,
                "{label} {:?}: stdout={stdout} stderr={stderr}",
                explicit_mir
            );
            assert_eq!(stdout, expected_stdout, "{label} {:?}", explicit_mir);
            if let Some(code) = expected_code {
                assert!(
                    stderr.contains(code),
                    "{label} {:?}: {stderr}",
                    explicit_mir
                );
                assert!(
                    stderr.contains("postcondition"),
                    "{label} {:?}: {stderr}",
                    explicit_mir
                );
            } else {
                assert!(stderr.is_empty(), "{label} {:?}: {stderr}", explicit_mir);
            }
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{label} {:?}: {stderr}",
                explicit_mir
            );
        }
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_postcondition_phase_stability() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_postcondition_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create postcondition CLI fixture directory");

    for (label, contract, argument, runtime_code, runtime_message) in [
        (
            "violation",
            "result == x",
            "-7 as i64",
            "E0808",
            "FFI postcondition failed",
        ),
        (
            "overflow",
            "result + 1 > result",
            "9223372036854775807 as i64",
            "E0802",
            "integer overflow in FFI postcondition",
        ),
    ] {
        let source = dir.join(format!("{label}.mimi"));
        fs::write(
            &source,
            format!(
                "extern \"C\" {{ func labs(x: i64) -> i64 ensures: {contract}; }}\nfunc main() -> i64 {{ println(labs({argument})); 0 }}\n"
            ),
        )
        .expect("write postcondition CLI source");
        for explicit_mir in [false, true] {
            let mut run = Command::new(mimi_bin());
            run.current_dir(project_root()).arg("run");
            if explicit_mir {
                run.arg("--mir");
            }
            let run = run
                .arg(&source)
                .output()
                .unwrap_or_else(|error| panic!("{label} run {:?}: {error}", explicit_mir));
            let run_stderr = String::from_utf8_lossy(&run.stderr);
            assert!(
                !run.status.success(),
                "{label} run {:?} must fail",
                explicit_mir
            );
            assert_eq!(run.stdout, b"", "{label} run {:?}", explicit_mir);
            assert!(run_stderr.contains(runtime_code), "{label}: {run_stderr}");
            assert!(
                run_stderr.contains(runtime_message),
                "{label}: {run_stderr}"
            );
            assert!(!run_stderr.contains("canonical route disposition: legacy"));

            let mut verify = Command::new(mimi_bin());
            verify.current_dir(project_root()).arg("verify");
            if explicit_mir {
                verify.arg("--mir");
            }
            let verify = verify
                .arg(&source)
                .output()
                .unwrap_or_else(|error| panic!("{label} verify {:?}: {error}", explicit_mir));
            let verify_stdout = String::from_utf8_lossy(&verify.stdout);
            let verify_stderr = String::from_utf8_lossy(&verify.stderr);
            assert!(
                !verify.status.success(),
                "{label} verify {:?} must fail",
                explicit_mir
            );
            assert!(
                verify_stdout.contains("0/1 verified") || verify_stderr.contains("0/1 verified"),
                "{label} verify {:?}: stdout={verify_stdout} stderr={verify_stderr}",
                explicit_mir
            );
            assert!(
                verify_stderr.contains("canonical MIR extern ensures contract disproven"),
                "{label} verify {:?}: {verify_stderr}",
                explicit_mir
            );
            assert!(!verify_stderr.contains("canonical route disposition: legacy"));

            let mut build = Command::new(mimi_bin());
            build
                .current_dir(project_root())
                .arg("build")
                .arg("--verify-ffi");
            if explicit_mir {
                build.arg("--mir");
            }
            let build = build
                .arg("--emit-ir")
                .arg(&source)
                .output()
                .unwrap_or_else(|error| panic!("{label} build {:?}: {error}", explicit_mir));
            let build_stderr = String::from_utf8_lossy(&build.stderr);
            assert!(
                !build.status.success(),
                "{label} build {:?} must fail",
                explicit_mir
            );
            assert!(
                build_stderr.contains("FFI contract verification failed"),
                "{label} build {:?}: {build_stderr}",
                explicit_mir
            );
            assert!(
                build_stderr.contains("canonical MIR extern ensures contract disproven"),
                "{label} build {:?}: {build_stderr}",
                explicit_mir
            );
            assert!(!build_stderr.contains("canonical route disposition: legacy"));
        }
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_multi_argument_remainder_zero_domain_matches() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_multi_remainder_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-argument remainder CLI fixture directory");
    let c_path = dir.join("pair.c");
    let library = dir.join("pair.so");
    fs::write(
        &c_path,
        "#include <stdint.h>\nint64_t pair(int64_t left, int64_t right) { (void)right; return left; }\n",
    )
    .expect("write multi-argument remainder C fixture");
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("compile multi-argument remainder C fixture");
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );

    for (label, contract, expected_success, expected_stdout, expected_code) in [
        (
            "short-circuit",
            "right == 0 or result % right == left % right",
            true,
            "-7\n",
            None,
        ),
        (
            "zero-domain",
            "result % right == 0",
            false,
            "",
            Some("E0801"),
        ),
    ] {
        let source = dir.join(format!("{label}.mimi"));
        fs::write(
            &source,
            format!(
                "extern \"C\" {{ func pair(left: i64, right: i64) -> i64 ensures: {contract}; }}\nfunc main() -> i64 {{ println(pair(-7 as i64, 0 as i64)); 0 }}\n"
            ),
        )
        .expect("write multi-argument remainder CLI source");
        for explicit_mir in [false, true] {
            let mut command = Command::new(mimi_bin());
            command.current_dir(project_root()).arg("run");
            if explicit_mir {
                command.arg("--mir");
            }
            let output = command
                .arg(&source)
                .env("MIMI_FFI_LIB", &library)
                .output()
                .unwrap_or_else(|error| panic!("{label} {:?}: {error}", explicit_mir));
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.success(),
                expected_success,
                "{label} {:?}: {stderr}",
                explicit_mir
            );
            assert_eq!(stdout, expected_stdout, "{label} {:?}", explicit_mir);
            if let Some(code) = expected_code {
                assert!(
                    stderr.contains(code),
                    "{label} {:?}: {stderr}",
                    explicit_mir
                );
                assert!(
                    stderr.contains("postcondition"),
                    "{label} {:?}: {stderr}",
                    explicit_mir
                );
            } else {
                assert!(stderr.is_empty(), "{label} {:?}: {stderr}", explicit_mir);
            }
            assert!(!stderr.contains("canonical route disposition: legacy"));
        }
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_mixed_width_checked_arithmetic_matches_default_and_mir() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_mixed_width_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create mixed-width CLI fixture directory");
    let c_path = dir.join("mixed.c");
    let library = dir.join("mixed.so");
    fs::write(
        &c_path,
        "#include <stdint.h>\nint32_t mixed_max(int32_t left, int64_t right) { (void)left; (void)right; return INT32_MAX; }\nint32_t mixed_zero(int32_t left, int64_t right) { (void)left; (void)right; return INT32_MAX; }\n",
    )
    .expect("write mixed-width C fixture");
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("compile mixed-width C fixture");
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );

    let source = dir.join("mixed.mimi");
    fs::write(
        &source,
        r#"extern "C" {
    func mixed_max(left: i32, right: i64) -> i32
        ensures: result + 1 > result;
    func mixed_zero(left: i32, right: i64) -> i32
        ensures: right == 0 or result + 1 > result;
}
func main() -> i32 {
    let max_value = mixed_max(0, 1 as i64);
    let zero_value = mixed_zero(0, 0 as i64);
    println(max_value);
    println(zero_value);
    0
}
"#,
    )
    .expect("write mixed-width CLI source");

    for explicit_mir in [false, true] {
        let mut run = Command::new(mimi_bin());
        run.current_dir(project_root()).arg("run");
        if explicit_mir {
            run.arg("--mir");
        }
        let run = run
            .arg(&source)
            .env("MIMI_FFI_LIB", &library)
            .output()
            .unwrap_or_else(|error| panic!("mixed-width run {:?}: {error}", explicit_mir));
        let run_stdout = String::from_utf8_lossy(&run.stdout);
        let run_stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "mixed-width run {:?}: stdout={run_stdout} stderr={run_stderr}",
            explicit_mir
        );
        assert_eq!(run_stdout, "2147483647\n2147483647\n");
        assert!(
            run_stderr.is_empty(),
            "mixed-width run {:?}: {run_stderr}",
            explicit_mir
        );
        assert!(!run_stderr.contains("canonical route disposition: legacy"));

        let mut verify = Command::new(mimi_bin());
        verify.current_dir(project_root()).arg("verify");
        if explicit_mir {
            verify.arg("--mir");
        }
        let verify = verify
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("mixed-width verify {:?}: {error}", explicit_mir));
        let verify_stdout = String::from_utf8_lossy(&verify.stdout);
        let verify_stderr = String::from_utf8_lossy(&verify.stderr);
        assert!(
            verify.status.success(),
            "mixed-width verify {:?}: stdout={verify_stdout} stderr={verify_stderr}",
            explicit_mir
        );
        assert!(verify_stdout.contains("2/2 verified"), "{verify_stdout}");
        assert!(
            verify_stdout.contains("canonical MIR extern ensures contract proven"),
            "{verify_stdout}"
        );
        assert!(!verify_stdout.contains("canonical route disposition: legacy"));
        assert!(!verify_stderr.contains("canonical route disposition: legacy"));

        let mut build = Command::new(mimi_bin());
        build
            .current_dir(project_root())
            .arg("build")
            .arg("--verify-ffi")
            .arg("--emit-ir");
        if explicit_mir {
            build.arg("--mir");
        }
        let build = build
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("mixed-width build {:?}: {error}", explicit_mir));
        let build_stdout = String::from_utf8_lossy(&build.stdout);
        let build_stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "mixed-width build {:?}: stdout={build_stdout} stderr={build_stderr}",
            explicit_mir
        );
        assert!(build_stdout.contains("define i32 @main"), "{build_stdout}");
        assert!(!build_stdout.contains("canonical route disposition: legacy"));
        assert!(!build_stderr.contains("canonical route disposition: legacy"));
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_mixed_width_error_phases_match_default_and_mir() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_mixed_width_error_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create mixed-width error CLI fixture directory");
    let c_path = dir.join("mixed_error.c");
    let library = dir.join("mixed_error.so");
    fs::write(
        &c_path,
        "#include <stdint.h>\nint32_t mixed_bad(int32_t left, int64_t right) { (void)right; return left + 1; }\nint32_t mixed_overflow(int32_t left, int64_t right) { (void)left; (void)right; return 1; }\n",
    )
    .expect("write mixed-width error C fixture");
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("compile mixed-width error C fixture");
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );

    for (label, declaration, call, runtime_code, runtime_message) in [
        (
            "violation",
            "func mixed_bad(left: i32, right: i64) -> i32 ensures: result == left;",
            "mixed_bad(6, 1 as i64)",
            "E0808",
            "FFI postcondition failed",
        ),
        (
            "overflow",
            "func mixed_overflow(left: i32, right: i64) -> i32 ensures: result + 9223372036854775807 > result;",
            "mixed_overflow(0, 1 as i64)",
            "E0802",
            "integer overflow in FFI postcondition",
        ),
    ] {
        let source = dir.join(format!("{label}.mimi"));
        fs::write(
            &source,
            format!(
                "extern \"C\" {{ {declaration} }}\nfunc main() -> i32 {{ println({call}); 0 }}\n"
            ),
        )
        .expect("write mixed-width error CLI source");

        for explicit_mir in [false, true] {
            let mut run = Command::new(mimi_bin());
            run.current_dir(project_root()).arg("run");
            if explicit_mir {
                run.arg("--mir");
            }
            let run = run
                .arg(&source)
                .env("MIMI_FFI_LIB", &library)
                .output()
                .unwrap_or_else(|error| panic!("{label} run {:?}: {error}", explicit_mir));
            let run_stderr = String::from_utf8_lossy(&run.stderr);
            assert!(!run.status.success(), "{label} run {:?} must fail", explicit_mir);
            assert_eq!(run.stdout, b"", "{label} run {:?}", explicit_mir);
            assert!(run_stderr.contains(runtime_code), "{label}: {run_stderr}");
            assert!(run_stderr.contains(runtime_message), "{label}: {run_stderr}");
            assert!(run_stderr.contains("postcondition"), "{label}: {run_stderr}");
            assert!(!run_stderr.contains("canonical route disposition: legacy"));

            let mut verify = Command::new(mimi_bin());
            verify.current_dir(project_root()).arg("verify");
            if explicit_mir {
                verify.arg("--mir");
            }
            let verify = verify
                .arg(&source)
                .output()
                .unwrap_or_else(|error| panic!("{label} verify {:?}: {error}", explicit_mir));
            let verify_stdout = String::from_utf8_lossy(&verify.stdout);
            let verify_stderr = String::from_utf8_lossy(&verify.stderr);
            let verify_text = format!("{verify_stdout}{verify_stderr}");
            assert!(!verify.status.success(), "{label} verify {:?} must fail", explicit_mir);
            assert!(verify_stdout.contains("0/1 verified"), "{label}: {verify_stdout}");
            assert!(
                verify_text.contains("canonical MIR extern ensures contract disproven"),
                "{label}: stdout={verify_stdout} stderr={verify_stderr}"
            );
            assert!(!verify_stdout.contains("canonical route disposition: legacy"));
            assert!(!verify_stderr.contains("canonical route disposition: legacy"));

            let mut build = Command::new(mimi_bin());
            build
                .current_dir(project_root())
                .arg("build")
                .arg("--verify-ffi")
                .arg("--emit-ir");
            if explicit_mir {
                build.arg("--mir");
            }
            let build = build
                .arg(&source)
                .output()
                .unwrap_or_else(|error| panic!("{label} build {:?}: {error}", explicit_mir));
            let build_stderr = String::from_utf8_lossy(&build.stderr);
            assert!(!build.status.success(), "{label} build {:?} must fail", explicit_mir);
            assert!(
                build_stderr.contains("FFI contract verification failed"),
                "{label}: {build_stderr}"
            );
            assert!(
                build_stderr.contains("canonical MIR extern ensures contract disproven"),
                "{label}: {build_stderr}"
            );
            assert!(!build_stderr.contains("canonical route disposition: legacy"));
        }
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn std_mimispec_removed() {
    // 0.1.8 Phase E: the in-repo std/mimispec implementation and external
    // `mimispec` crate are removed. This test prevents regrowth of the old
    // sketch-parser surface in the standard library.
    let dir = project_root().join("std").join("mimispec");
    assert!(!dir.exists(), "std/mimispec must be removed in 0.1.8");
}

#[test]
fn canonical_mir_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_scalar.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mimi mir");
    assert!(
        output.status.success(),
        "mimi mir failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mir.type-catalog"));
    assert!(stdout.contains("mir.function function:main"));
    assert!(stdout.contains("binary"));
}

#[test]
fn canonical_mir_cli_receipt_manifest_is_deterministic_and_exposes_ffi_digest() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_scalar_ffi_labs.mimi");
    let run = || {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&fixture)
            .arg("--receipt")
            .output()
            .expect("failed to spawn mimi mir --receipt")
    };
    let first = run();
    let second = run();
    assert!(
        first.status.success(),
        "mimi mir --receipt failed:\n{}\n{}",
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&first.stdout)
    );
    assert!(second.status.success());
    assert_eq!(
        first.stdout, second.stdout,
        "receipt manifest must be deterministic"
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    assert!(stdout.contains("mimi-mir-route-manifest-v1\n"));
    assert!(stdout.contains("schema=mimi-mir-route-receipt-v1\n"));
    assert!(stdout.contains("profile=cli-mir-v1\n"));
    let ffi_digest = stdout
        .lines()
        .find_map(|line| line.strip_prefix("ffi_digest="))
        .expect("receipt manifest must include ffi_digest");
    assert_eq!(ffi_digest.len(), 64);
    assert!(stdout.contains("mir_digest="));
    assert!(stdout.contains("root_owners=function:main"));
}

#[test]
fn canonical_mir_cli_receipt_manifest_matches_checked_api_matrix_and_entry_routes() {
    let fixtures = ["mir_scalar_ffi_labs.mimi", "mir_scalar_ffi_abi.mimi"];
    let mut manifests = Vec::new();
    for fixture_name in fixtures {
        let fixture = project_root()
            .join("tests")
            .join("fixtures")
            .join(fixture_name);
        let checked = checked_route_receipt(&fixture);
        let manifest_output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&fixture)
            .arg("--receipt")
            .output()
            .unwrap_or_else(|error| panic!("spawn receipt manifest for {fixture_name}: {error}"));
        assert!(
            manifest_output.status.success(),
            "receipt manifest failed for {fixture_name}: {}",
            String::from_utf8_lossy(&manifest_output.stderr)
        );
        let manifest = parse_route_receipt_manifest(&manifest_output.stdout);
        let expected = [
            ("schema", checked.schema.to_owned()),
            ("profile", checked.profile.clone()),
            ("mir_digest", checked.mir_digest.clone()),
            ("type_desc_digest", checked.type_desc_digest.clone()),
            ("abi_digest", checked.abi_digest.clone()),
            ("ffi_digest", checked.ffi_digest.clone()),
            ("ownership_digest", checked.ownership_digest.clone()),
            (
                "flow_transition_digest",
                checked.flow_transition_digest.clone(),
            ),
            (
                "root_owners",
                checked
                    .root_owners
                    .iter()
                    .map(|owner| owner.0.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ];
        let expected_len = expected.len();
        for (key, value) in &expected {
            assert_eq!(
                manifest.get(*key).map(String::as_str),
                Some(value.as_str()),
                "{fixture_name}: {key}"
            );
        }
        assert_eq!(
            manifest.len(),
            expected_len,
            "{fixture_name}: manifest schema drift"
        );

        let default_ir = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(["build", "--emit-ir"])
            .arg(&fixture)
            .output()
            .unwrap_or_else(|error| panic!("default build for {fixture_name}: {error}"));
        let explicit_ir = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(["build", "--mir", "--emit-ir"])
            .arg(&fixture)
            .output()
            .unwrap_or_else(|error| panic!("explicit MIR build for {fixture_name}: {error}"));
        assert!(
            default_ir.status.success(),
            "default build failed for {fixture_name}: {}",
            String::from_utf8_lossy(&default_ir.stderr)
        );
        assert!(
            explicit_ir.status.success(),
            "explicit MIR build failed for {fixture_name}: {}",
            String::from_utf8_lossy(&explicit_ir.stderr)
        );
        assert_eq!(
            default_ir.stdout, explicit_ir.stdout,
            "{fixture_name}: default and --mir route IR drift"
        );
        assert!(!String::from_utf8_lossy(&default_ir.stderr)
            .contains("canonical route disposition: legacy"));
        assert!(!String::from_utf8_lossy(&explicit_ir.stderr)
            .contains("canonical route disposition: legacy"));
        manifests.push(manifest);
    }
    assert_ne!(
        manifests[0].get("ffi_digest"),
        manifests[1].get("ffi_digest"),
        "different declaration/call graphs must never collapse to one FFI digest"
    );
}

#[test]
fn canonical_mir_cli_receipt_manifest_import_graph_requires_all_and_is_deterministic() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-import-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create import graph fixture directory");
    let helper = dir.join("helper.mimi");
    fs::write(&helper, "pub func imported_value() -> i32 { 41 }\n").expect("write imported helper");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use helper;\nfunc main() -> i32 { imported_value() }\n",
    )
    .expect("write import graph entry");

    let run = |include_imports: bool| {
        let mut command = Command::new(mimi_bin());
        command
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .arg("--receipt");
        if include_imports {
            command.arg("--all");
        }
        command.output().expect("spawn import graph MIR receipt")
    };

    let source_only = run(false);
    assert!(!source_only.status.success());
    let source_only_stderr = String::from_utf8_lossy(&source_only.stderr);
    assert!(
        source_only_stderr.contains("MIR inspection input rejected"),
        "source-only import graph rejection lost its diagnostic: {source_only_stderr}"
    );
    assert!(!source_only_stderr.contains("canonical route disposition: legacy"));

    let checked = checked_route_receipt(&main);
    let first = run(true);
    let second = run(true);
    assert!(
        first.status.success(),
        "import graph receipt failed:\n{}\n{}",
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&first.stdout)
    );
    assert_eq!(
        first.stdout, second.stdout,
        "import graph receipt must be deterministic"
    );
    let manifest = parse_route_receipt_manifest(&first.stdout);
    assert_eq!(
        manifest.get("schema").map(String::as_str),
        Some(checked.schema)
    );
    assert_eq!(
        manifest.get("profile").map(String::as_str),
        Some("cli-mir-v1")
    );
    assert_eq!(manifest.get("mir_digest"), Some(&checked.mir_digest));
    assert_eq!(
        manifest.get("type_desc_digest"),
        Some(&checked.type_desc_digest)
    );
    assert_eq!(manifest.get("abi_digest"), Some(&checked.abi_digest));
    assert_eq!(manifest.get("ffi_digest"), Some(&checked.ffi_digest));
    assert_eq!(
        manifest.get("ownership_digest"),
        Some(&checked.ownership_digest)
    );
    assert_eq!(
        manifest.get("flow_transition_digest"),
        Some(&checked.flow_transition_digest)
    );
    let root_owners = manifest.get("root_owners").expect("root owners field");
    assert!(root_owners.contains("function:main"));
    assert!(root_owners.contains("function:imported_value"));
    assert_eq!(
        manifest.len(),
        mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_all_uses_the_production_builder_for_imported_instances() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("std_set.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn imported canonical MIR inspection");
    assert!(
        output.status.success(),
        "mimi mir --all failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mir.function function:mir:instance:function:set_insert"),
        "canonical MIR inspection omitted the materialized Set facade instance:\n{stdout}"
    );
    assert!(
        stdout.contains(" list_op "),
        "canonical MIR inspection omitted the List.len operation:\n{stdout}"
    );
}

#[test]
fn canonical_mir_cli_all_rejects_unsupported_shapes_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn rejected canonical MIR inspection");
    assert!(
        !output.status.success(),
        "unsupported MIR inspection shape must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MIR inspection input rejected") && stderr.contains("Copy scalar"),
        "unexpected canonical MIR inspection rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("legacy") && !stderr.contains("bytecode runtime error"),
        "MIR inspection must not fall back to another consumer:\n{stderr}"
    );
}

#[test]
fn canonical_mir_run_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_scalar.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn mimi run --mir");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_native_build_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_scalar.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR reference bytecode run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-{}-{}",
        std::process::id(),
        fixture.file_stem().unwrap().to_string_lossy()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR native build");
    assert!(
        build.status.success(),
        "canonical MIR native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_nested_tuple_variant_matches_reference_and_native() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("mir_nested_tuple_option.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn nested tuple canonical MIR run");
    assert_eq!(
        mir_run.status.code(),
        Some(0),
        "nested tuple canonical MIR run failed:\n{}\n{}",
        String::from_utf8_lossy(&mir_run.stderr),
        String::from_utf8_lossy(&mir_run.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&mir_run.stdout), "7\n");

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-{}-nested-tuple",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn nested tuple canonical MIR native build");
    assert!(
        build.status.success(),
        "nested tuple canonical MIR native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute nested tuple canonical MIR native binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(0),
        "nested tuple canonical MIR native run failed:\n{}\n{}",
        String::from_utf8_lossy(&native_run.stderr),
        String::from_utf8_lossy(&native_run.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&native_run.stdout), "7\n");
}

#[test]
fn default_route_nested_tuple_variant_uses_canonical_mir_for_all_cli_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("mir_nested_tuple_option.mimi");

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default nested tuple run");
    assert!(
        run.status.success(),
        "default nested tuple run failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "7\n");

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default nested tuple verifier");
    assert!(
        verify.status.success(),
        "default nested tuple verifier failed:\n{}\n{}",
        String::from_utf8_lossy(&verify.stderr),
        String::from_utf8_lossy(&verify.stdout)
    );
    let verify_stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(verify_stdout.contains("canonical MIR"), "{verify_stdout}");
    assert!(verify_stdout.contains("1/1 verified"), "{verify_stdout}");
    assert!(
        verify_stdout.contains("2 total constraints"),
        "{verify_stdout}"
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-native-option-nested-tuple-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default nested tuple native build");
    assert!(
        build.status.success(),
        "default nested tuple native build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default nested tuple native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native_run.stdout), "7\n");
}

#[test]
fn canonical_flow_state_match_uses_default_mir_route_and_native_output() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("flow_state_match_fail_result_dual_backend.mimi");

    let mir_dump = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn cross-state canonical MIR inspection");
    assert!(
        mir_dump.status.success(),
        "cross-state MIR inspection failed:\n{}\n{}",
        String::from_utf8_lossy(&mir_dump.stderr),
        String::from_utf8_lossy(&mir_dump.stdout)
    );
    let mir_stdout = String::from_utf8_lossy(&mir_dump.stdout);
    assert!(mir_stdout.contains("RecoverableBoundary"));
    assert!(mir_stdout.contains("flow_transition"));
    assert!(mir_stdout.contains("match.nested.record"));
    assert!(mir_stdout.contains("match.nested.record.project"));

    for args in [vec!["run"], vec!["run", "--mir"]] {
        let run = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(args)
            .arg(&fixture)
            .output()
            .expect("failed to spawn cross-state canonical MIR run");
        assert_eq!(
            run.status.code(),
            Some(0),
            "cross-state run failed:\n{}\n{}",
            String::from_utf8_lossy(&run.stderr),
            String::from_utf8_lossy(&run.stdout)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout), "2\n");
    }

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-flow-state-match-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default cross-state canonical MIR build");
    assert!(
        build.status.success(),
        "default cross-state MIR build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default cross-state canonical MIR binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), "2\n");
    assert!(native.stderr.is_empty());
}

#[test]
fn canonical_flow_failure_match_returns_source_on_default_mir_route() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("flow_state_match_fail_result_failure_dual_backend.mimi");

    let mir_dump = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn cross-state failure MIR inspection");
    assert!(
        mir_dump.status.success(),
        "cross-state failure MIR inspection failed:\n{}\n{}",
        String::from_utf8_lossy(&mir_dump.stderr),
        String::from_utf8_lossy(&mir_dump.stdout)
    );
    let mir_stdout = String::from_utf8_lossy(&mir_dump.stdout);
    assert!(mir_stdout.contains("RecoverableBoundary"));
    assert!(mir_stdout.contains("nested=Some"));
    assert!(mir_stdout.contains("switch_move"));
    assert!(mir_stdout.contains("drop.0"));

    for args in [vec!["verify"], vec!["verify", "--mir"]] {
        let verify = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(args)
            .arg(&fixture)
            .output()
            .expect("failed to spawn recoverable cross-state verifier");
        assert!(
            verify.status.success(),
            "recoverable cross-state verifier failed:\n{}\n{}",
            String::from_utf8_lossy(&verify.stderr),
            String::from_utf8_lossy(&verify.stdout)
        );
        let verify_stdout = String::from_utf8_lossy(&verify.stdout);
        assert!(verify_stdout.contains("canonical MIR ensures contract proven"));
        assert!(verify_stdout.contains("1/1 verified"));
        assert!(verify_stdout.contains("[15 constraints]"));
        assert!(verify.stderr.is_empty());
    }

    for args in [vec!["run"], vec!["run", "--mir"]] {
        let run = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(args)
            .arg(&fixture)
            .output()
            .expect("failed to spawn cross-state failure MIR run");
        assert_eq!(
            run.status.code(),
            Some(0),
            "cross-state failure run failed:\n{}\n{}",
            String::from_utf8_lossy(&run.stderr),
            String::from_utf8_lossy(&run.stdout)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n");
    }

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-flow-failure-match-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default cross-state failure MIR build");
    assert!(
        build.status.success(),
        "default cross-state failure MIR build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default cross-state failure MIR binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), "1\n");
    assert!(native.stderr.is_empty());
}

#[test]
fn canonical_multifield_flow_source_receipt_uses_default_mir_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_m3_flow_multifield_string_source_receipt.mimi");

    let mir_dump = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn multifield Flow MIR inspection");
    assert!(
        mir_dump.status.success(),
        "multifield Flow MIR inspection failed:\n{}\n{}",
        String::from_utf8_lossy(&mir_dump.stderr),
        String::from_utf8_lossy(&mir_dump.stdout)
    );
    let mir_stdout = String::from_utf8_lossy(&mir_dump.stdout);
    assert!(mir_stdout.contains("RecoverableBoundary"), "{mir_stdout}");
    assert!(mir_stdout.contains("move_project_drop"), "{mir_stdout}");

    for args in [vec!["run"], vec!["run", "--mir"]] {
        let run = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(args)
            .arg(&fixture)
            .output()
            .expect("failed to spawn multifield Flow canonical run");
        assert_eq!(
            run.status.code(),
            Some(0),
            "multifield Flow run failed:\n{}\n{}",
            String::from_utf8_lossy(&run.stderr),
            String::from_utf8_lossy(&run.stdout)
        );
        assert_eq!(String::from_utf8_lossy(&run.stdout), "source\n");
        assert!(run.stderr.is_empty());
    }

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-multifield-flow-source-receipt-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn multifield Flow canonical build");
    assert!(
        build.status.success(),
        "multifield Flow native build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute multifield Flow native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), "source\n");
    assert!(native.stderr.is_empty());
}

#[test]
fn canonical_flow_failure_verifier_reports_a_real_counterexample() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("flow_state_match_fail_result_failure_disproven_dual_backend.mimi");
    for args in [vec!["verify"], vec!["verify", "--mir"]] {
        let verify = Command::new(mimi_bin())
            .current_dir(project_root())
            .args(args)
            .arg(&fixture)
            .output()
            .expect("failed to spawn recoverable cross-state counterexample verifier");
        assert!(!verify.status.success());
        let verify_stdout = String::from_utf8_lossy(&verify.stdout);
        assert!(verify_stdout.contains("canonical MIR ensures contract is disproven"));
        assert!(verify_stdout.contains("[15 constraints]"));
        assert!(!verify_stdout.contains("No contracts to verify"));
        assert!(!verify_stdout.contains("flow_ast"));
    }
}

#[test]
fn canonical_mir_scalar_list_len_closes_reference_native_and_verifier() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_len.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical MIR List.len dump");
    assert!(
        mir.status.success(),
        "canonical MIR List.len dump failed:\n{}\n{}",
        String::from_utf8_lossy(&mir.stderr),
        String::from_utf8_lossy(&mir.stdout)
    );
    assert!(String::from_utf8_lossy(&mir.stdout).contains("list_op"));

    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR List.len reference run");
    assert_eq!(
        reference.status.code(),
        Some(42),
        "canonical MIR List.len reference run failed:\n{}",
        String::from_utf8_lossy(&reference.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-len-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR List.len native build");
    assert!(
        build.status.success(),
        "canonical MIR List.len native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR List.len native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "canonical MIR List.len native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR List.len verifier");
    assert!(
        verification.status.success(),
        "canonical MIR List.len verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));

    // The same complete program is now a default production island. The
    // selector must choose one canonical graph for run/build/verify; the
    // presence of the canonical native adapter makes the route observable.
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default canonical List.len run");
    assert_eq!(
        default_run.status.code(),
        Some(42),
        "default canonical List.len run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default canonical List.len build");
    assert!(
        default_build_ir.status.success(),
        "default canonical List.len build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    assert!(
        String::from_utf8_lossy(&default_build_ir.stdout).contains("mimi_mir_list_len_scalar"),
        "default build did not select the canonical List.len island:\n{}",
        String::from_utf8_lossy(&default_build_ir.stdout)
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default canonical List.len verifier");
    assert!(
        default_verification.status.success(),
        "default canonical List.len verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stderr),
        String::from_utf8_lossy(&default_verification.stdout)
    );
    assert!(String::from_utf8_lossy(&default_verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_default_does_not_promote_non_copy_list_len() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_string_len_rejected.mimi");

    // The typed body contains List<string>. The MIR List.len contract is
    // scalar-Copy-only, so explicit MIR must reject it before any backend.
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical List<string>.len build");
    assert!(
        !explicit.status.success(),
        "unsupported List<string>.len unexpectedly entered canonical MIR:\n{}",
        String::from_utf8_lossy(&explicit.stderr)
    );

    // Default routing remains the explicit compatibility route for this
    // unsupported shape. It must not be mistaken for a canonical promotion.
    let default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn compatibility List<string>.len build");
    assert!(
        default.status.success(),
        "compatibility List<string>.len build failed:\n{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&default.stdout).contains("mimi_mir_list_len_scalar"),
        "unsupported List<string>.len was promoted to canonical MIR:\n{}",
        String::from_utf8_lossy(&default.stdout)
    );
}

#[test]
fn canonical_mir_test_uses_ast_free_bytecode_for_scalar_collection() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("test")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical scalar-collection mimi test");
    assert!(
        output.status.success(),
        "canonical scalar-collection mimi test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed, 0 failed"));
}

#[test]
fn canonical_mir_test_rejects_mixed_scalar_collection_without_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection_mixed.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("test")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected mixed scalar-collection mimi test");
    assert!(
        !output.status.success(),
        "mixed scalar collection unexpectedly used a compatibility test compiler:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("default Canonical MIR route rejected"));
    assert!(stderr.contains("S11 scalar collection candidate"));
}

#[test]
fn canonical_mir_disasm_uses_ast_free_bytecode_for_scalar_collection() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("disasm")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical scalar-collection mimi disasm");
    assert!(
        output.status.success(),
        "canonical scalar-collection disasm failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("function:test_list_len"),
        "canonical disasm must expose stable MIR function identity:\n{}",
        stdout
    );
    assert!(
        stdout.contains("MIR_LIST_LEN"),
        "canonical disasm must expose the MIR collection operation:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("__flow_Main_run_Single"),
        "canonical disasm unexpectedly retained compatibility-only Flow helpers:\n{}",
        stdout
    );
}

#[test]
fn canonical_mir_disasm_rejects_mixed_scalar_collection_without_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_test_scalar_collection_mixed.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("disasm")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected mixed scalar-collection mimi disasm");
    assert!(
        !output.status.success(),
        "mixed scalar collection unexpectedly used a compatibility disassembler:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("default Canonical MIR route rejected"));
    assert!(stderr.contains("S11 scalar collection candidate"));
}

#[test]
fn canonical_mir_native_owned_string_glue_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_owned_string.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR owned String reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(41),
        "canonical MIR owned String reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-owned-string-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR owned String native build");
    assert!(
        build.status.success(),
        "canonical MIR owned String native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR owned String native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(41),
        "canonical MIR owned String native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR owned String verifier");
    assert!(
        verification.status.success(),
        "canonical MIR owned String verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_verifier_proves_owned_string_result_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_result_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR owned String result verifier");
    assert!(
        output.status.success(),
        "verifier should prove the canonical owned String return"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_rejects_owned_string_return_branch_before_backend() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_return_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR owned String verifier");
    assert!(
        !output.status.success(),
        "branch-shaped return must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR verifier input rejected"),
        "{stderr}"
    );
    assert!(
        stderr.contains("owned String return contract requires one canonical MIR block"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("flow_ast"),
        "legacy verifier fallback leaked: {stderr}"
    );
}

#[test]
fn canonical_mir_verifier_proves_direct_owned_string_calls_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_call_return.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn direct owned String call verifier");
    assert!(
        output.status.success(),
        "direct owned String calls must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("4/4 verified"), "{stdout}");
    assert!(
        stdout
            .matches("canonical MIR ensures contract proven")
            .count()
            >= 4
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_reports_nested_owned_string_call_boundary() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_owned_string_call_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected nested owned String call verifier");
    assert!(
        output.status.success(),
        "trusted-subset rejection is a verifier result, not a process failure:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.contains(
            "direct owned String call target 'function:nested' rejected: owned String return contract only admits String constants and ownership glue"
        ),
        "{output}"
    );
    assert!(
        !output.contains("flow_ast"),
        "legacy verifier fallback leaked: {output}"
    );
}

#[test]
fn canonical_mir_verifier_proves_non_copy_record_move_projection_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_move_projection.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn record MoveProject verifier");
    assert!(
        output.status.success(),
        "record MoveProject must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_proves_non_copy_result_string_i32_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_i32.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn Result<string, i32> verifier");
    assert!(
        output.status.success(),
        "Result<string, i32> must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_proves_non_copy_result_string_i32_switch_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_i32_switch_move.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn Result SwitchMove verifier");
    assert!(
        output.status.success(),
        "Result SwitchMove must be proven by canonical MIR verifier:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical MIR ensures contract proven"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_verifier_classifies_result_string_string_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_string_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected Result verifier");
    assert!(
        output.status.success(),
        "trusted-subset rejection must remain a verifier classification:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0/1 verified"), "{stdout}");
    assert!(
        stdout.contains("canonical non-Copy Result<string, i32> variant contract"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("flow_ast"),
        "legacy verifier fallback leaked: {stdout}"
    );
}

#[test]
fn canonical_mir_native_recursive_tuple_glue_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_recursive_tuple.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR recursive tuple reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR recursive tuple reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-recursive-tuple-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR recursive tuple native build");
    assert!(
        build.status.success(),
        "canonical MIR recursive tuple native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR recursive tuple native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR recursive tuple native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_native_non_copy_record_glue_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_non_copy_record.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR non-Copy record reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR non-Copy record reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-non-copy-record-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR non-Copy record native build");
    assert!(
        build.status.success(),
        "canonical MIR non-Copy record native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR non-Copy record native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR non-Copy record native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_native_record_move_project_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_move_project.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record MoveProject reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR record MoveProject reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-move-project-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record MoveProject native build");
    assert!(
        build.status.success(),
        "canonical MIR record MoveProject native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record MoveProject native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR record MoveProject native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_generic_list_projection_rejects_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_projection_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-projection-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR generic List build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR build error"));
    assert!(stderr.contains("generic MIR instance argument is outside scalar contract"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_abs_overflow_matches_mir_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_abs_overflow.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR trap oracle");
    assert_eq!(mir_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mir_run.stderr).contains("E0802"));

    let binary =
        std::env::temp_dir().join(format!("mimi-canonical-native-trap-{}", std::process::id()));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR trap build");
    assert!(
        build.status.success(),
        "canonical MIR trap build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR trap binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0802"));
}

#[test]
fn canonical_mir_native_builds_min_max_and_widening_convert() {
    for fixture_name in [
        "mir_builtin_min_max.mimi",
        "mir_convert_i32_to_i64_min_max.mimi",
    ] {
        let fixture = project_root()
            .join("tests")
            .join("fixtures")
            .join(fixture_name);
        let binary = std::env::temp_dir().join(format!(
            "mimi-canonical-native-numeric-{}-{}",
            std::process::id(),
            fixture_name
        ));
        let build = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("build")
            .arg(&fixture)
            .arg("--mir")
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("failed to spawn canonical MIR numeric build");
        assert!(
            build.status.success(),
            "canonical MIR numeric build failed for {fixture_name}:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let native_run = Command::new(&binary)
            .output()
            .expect("failed to execute canonical MIR numeric binary");
        let _ = fs::remove_file(&binary);
        assert_eq!(
            native_run.status.code(),
            Some(42),
            "canonical MIR numeric native run failed for {fixture_name}:\n{}",
            String::from_utf8_lossy(&native_run.stderr)
        );
    }
}

#[test]
fn canonical_mir_native_build_record_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_copy.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record native build");
    assert!(
        build.status.success(),
        "canonical MIR record native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_record_update_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_update.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record update reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-update-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record update native build");
    assert!(
        build.status.success(),
        "canonical MIR record update native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record update native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn default_copy_record_update_selects_canonical_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_update.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default record update run");
    assert_eq!(
        output.status.code(),
        Some(42),
        "default canonical record update run failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default canonical record update build");
    assert!(
        default_build_ir.status.success(),
        "default canonical record update build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    let ir = String::from_utf8_lossy(&default_build_ir.stdout);
    assert!(
        ir.contains("define i32 @main()"),
        "default record update did not select the canonical MIR route:\n{ir}"
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-canonical-record-update-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default canonical record update native build");
    assert!(
        build.status.success(),
        "default canonical record update native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default canonical record update binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "default canonical record update native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default canonical record update verifier");
    assert!(
        verification.status.success(),
        "default canonical record update verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
}

#[test]
fn default_silent_local_flow_transition_selects_one_canonical_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_flow_transition.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Flow transition run");
    assert_eq!(
        default_run.status.code(),
        Some(42),
        "default Flow transition run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default Flow transition LLVM emission");
    assert!(
        default_build_ir.status.success(),
        "default Flow transition build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    let ir = String::from_utf8_lossy(&default_build_ir.stdout);
    assert!(
        ir.contains("@__mimi_transition_Counter__inc__Zero"),
        "default build did not select the canonical Flow transition island:\n{ir}"
    );

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical Flow transition inspection");
    assert!(mir.status.success());
    assert!(String::from_utf8_lossy(&mir.stdout)
        .contains("mir.transition transition:Counter::inc::Zero"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-canonical-flow-transition-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Flow transition native build");
    assert!(
        build.status.success(),
        "default Flow transition native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default Flow transition native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "default Flow transition native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Flow transition verifier");
    assert!(
        verification.status.success(),
        "default Flow transition verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
}

#[test]
fn default_flow_candidate_never_falls_back_to_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_flow_transition_rejected_builtin.mimi");
    let expected = "default Canonical MIR route rejected";

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected Flow candidate run");
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains(expected));

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn rejected Flow candidate build");
    assert!(!build.status.success());
    assert!(String::from_utf8_lossy(&build.stderr).contains(expected));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected Flow candidate verifier");
    assert!(!verify.status.success());
    assert!(String::from_utf8_lossy(&verify.stderr).contains(expected));
}

#[test]
fn canonical_default_does_not_promote_non_copy_record_program() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_non_copy_record.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default non-Copy record build");
    assert!(
        output.status.success(),
        "default non-Copy record compatibility build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ir = String::from_utf8_lossy(&output.stdout);
    assert!(
        ir.contains("define i32 @main(i32 %0, ptr %1)"),
        "non-Copy record was promoted to the flat Copy canonical island:\n{ir}"
    );
}

#[test]
fn canonical_mir_native_borrow_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_borrow_scalar.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR borrow reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(42),
        "canonical MIR borrow reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-borrow-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR borrow native build");
    assert!(
        build.status.success(),
        "canonical MIR borrow native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR borrow native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native_run.status.code(),
        Some(42),
        "canonical MIR borrow native run failed:\n{}",
        String::from_utf8_lossy(&native_run.stderr)
    );
}

#[test]
fn canonical_mir_native_record_update_preserves_checked_trap() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_update_overflow.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record update trap oracle");
    assert_eq!(mir_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mir_run.stderr).contains("E0802"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-update-trap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record update trap build");
    assert!(
        build.status.success(),
        "canonical MIR record update trap build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record update trap binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0802"));
}

#[test]
fn canonical_mir_native_builds_copy_option_and_result_variants() {
    for (fixture_name, expected_status) in [
        ("mir_native_option_bool.mimi", 42),
        ("mir_native_option_copy.mimi", 42),
        ("mir_native_result_copy.mimi", 8),
    ] {
        let fixture = project_root()
            .join("tests")
            .join("fixtures")
            .join(fixture_name);
        let mir_run = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("run")
            .arg(&fixture)
            .arg("--mir")
            .output()
            .expect("failed to spawn canonical MIR variant reference run");
        assert_eq!(mir_run.status.code(), Some(expected_status));

        let binary = std::env::temp_dir().join(format!(
            "mimi-canonical-native-variant-{}-{}",
            std::process::id(),
            fixture_name
        ));
        let build = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("build")
            .arg(&fixture)
            .arg("--mir")
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("failed to spawn canonical MIR variant native build");
        assert!(
            build.status.success(),
            "canonical MIR variant native build failed for {fixture_name}:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let native_run = Command::new(&binary)
            .output()
            .expect("failed to execute canonical MIR variant native binary");
        let _ = fs::remove_file(&binary);
        assert_eq!(native_run.status.code(), Some(expected_status));
    }
}

#[test]
fn canonical_default_copy_option_i32_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i32_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i32> reference run");
    assert_eq!(default_run.status.code(), Some(41));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<i32> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i32> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<i32> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-i32-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<i32> native build");
    assert!(
        build.status.success(),
        "default Copy Option<i32> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<i32> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_option_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option projection run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option projection verifier");
    assert!(
        verify.status.success(),
        "default generic Option projection verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-unwrap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Option projection native build");
    assert!(
        build.status.success(),
        "default generic Option projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Option projection native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_option_f64_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_f64.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option<f64> projection run");
    assert_eq!(default_run.status.code(), Some(42));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR generic Option<f64> projection run");
    assert_eq!(explicit_mir_run.status.code(), Some(42));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option<f64> projection verifier");
    assert!(
        verify.status.success(),
        "default generic Option<f64> projection verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-f64-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Option<f64> projection native build");
    assert!(
        build.status.success(),
        "default generic Option<f64> projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Option<f64> projection native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_default_generic_owned_option_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_owned_string.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default owned generic Option projection run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default owned generic Option projection verifier");
    assert!(
        verify.status.success(),
        "default owned generic Option projection verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-owned-option-unwrap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default owned generic Option projection native build");
    assert!(
        build.status.success(),
        "default owned generic Option projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default owned generic Option projection native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_owned_list_option_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_owned_list.mimi");

    let explicit_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit generic Option<List> MIR run");
    assert_eq!(explicit_run.status.code(), Some(41));

    let explicit_verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit generic Option<List> MIR verifier");
    assert!(
        explicit_verify.status.success(),
        "explicit generic Option<List> MIR verifier failed:\n{}",
        String::from_utf8_lossy(&explicit_verify.stderr)
    );
    assert!(String::from_utf8_lossy(&explicit_verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-explicit-generic-option-owned-list-{}",
        std::process::id()
    ));
    let explicit_build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit generic Option<List> MIR native build");
    assert!(
        explicit_build.status.success(),
        "explicit generic Option<List> MIR native build failed:\n{}",
        String::from_utf8_lossy(&explicit_build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit generic Option<List> MIR native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option<List> run");
    assert_eq!(default_run.status.code(), Some(41));

    let default_verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option<List> verifier");
    assert!(
        default_verify.status.success(),
        "default generic Option<List> verifier failed:\n{}",
        String::from_utf8_lossy(&default_verify.stderr)
    );
    assert!(String::from_utf8_lossy(&default_verify.stdout).contains("canonical MIR"));

    let default_binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-owned-list-{}",
        std::process::id()
    ));
    let default_build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&default_binary)
        .output()
        .expect("failed to spawn default generic Option<List> native build");
    assert!(
        default_build.status.success(),
        "default generic Option<List> native build failed:\n{}",
        String::from_utf8_lossy(&default_build.stderr)
    );
    let default_native_run = Command::new(&default_binary)
        .output()
        .expect("failed to execute default generic Option<List> native binary");
    let _ = fs::remove_file(&default_binary);
    assert_eq!(default_native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_owned_list_scalar_family_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_owned_list_scalars.mimi");

    for extra in [Some("--mir"), None] {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("run").arg(&fixture);
        if let Some(flag) = extra {
            command.arg(flag);
        }
        let run = command
            .output()
            .expect("failed to spawn generic Option<List<i64|bool>> run");
        assert_eq!(run.status.code(), Some(41));
    }

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Option<List<i64|bool>> verifier");
    assert!(
        verify.status.success(),
        "generic Option<List<i64|bool>> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-owned-list-scalars-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn generic Option<List<i64|bool>> native build");
    assert!(
        build.status.success(),
        "generic Option<List<i64|bool>> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute generic Option<List<i64|bool>> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_owned_float_list_option_unwrap_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_owned_float_list_rejected.mimi");

    for command in ["run", "build", "verify"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture)
            .output()
            .expect("failed to spawn unsupported generic Option<List<f64>> command");
        assert!(
            !output.status.success(),
            "default {command} must reject unsupported Option<List<f64>>"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("generic Option projection") && stderr.contains("Canonical MIR"),
            "default {command} lost its stable fail-closed diagnostic:\n{stderr}"
        );
        assert!(
            !stderr.contains("legacy"),
            "default {command} must not fall back to legacy:\n{stderr}"
        );
    }
}

#[test]
fn canonical_default_generic_option_unwrap_or_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default generic Option unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Option unwrap_or native build");
    assert!(
        build.status.success(),
        "default generic Option unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Option unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));

    let none_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_none.mimi");
    let none_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&none_fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or None run");
    assert_eq!(none_run.status.code(), Some(7));

    let bool_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_bool.mimi");
    let bool_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&bool_fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or bool run");
    assert_eq!(bool_run.status.code(), Some(0));

    let i64_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_i64.mimi");
    let i64_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&i64_fixture)
        .output()
        .expect("failed to spawn default generic Option unwrap_or i64 run");
    assert_eq!(i64_run.status.code(), Some(7));

    let i64_binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-unwrap-or-i64-{}",
        std::process::id()
    ));
    let i64_build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&i64_fixture)
        .arg("-o")
        .arg(&i64_binary)
        .output()
        .expect("failed to spawn default generic Option unwrap_or i64 native build");
    assert!(
        i64_build.status.success(),
        "default generic Option unwrap_or i64 native build failed:\n{}",
        String::from_utf8_lossy(&i64_build.stderr)
    );
    let i64_native_run = Command::new(&i64_binary)
        .output()
        .expect("failed to execute default generic Option unwrap_or i64 native binary");
    let _ = fs::remove_file(&i64_binary);
    assert_eq!(i64_native_run.status.code(), Some(7));
}

#[test]
fn canonical_default_generic_option_unwrap_or_f64_matches_mir_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_f64.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option<f64> unwrap_or run");
    assert_eq!(run.status.code(), Some(42));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Option<f64> unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default generic Option<f64> unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("No contracts to verify"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-option-unwrap-or-f64-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Option<f64> unwrap_or native build");
    assert!(
        build.status.success(),
        "default generic Option<f64> unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Option<f64> unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));

    let none_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_f64_none.mimi");
    let none_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&none_fixture)
        .output()
        .expect("failed to spawn default generic Option<f64> unwrap_or None run");
    assert_eq!(none_run.status.code(), Some(42));
}

#[test]
fn canonical_default_generic_option_projection_rejects_unmigrated_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_option_unwrap_or_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn unsupported generic Option projection run");
    assert!(!run.status.success());
    let run_stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run_stderr.contains("generic Option projection") && !run_stderr.contains("legacy"),
        "unsupported generic Option fallback must fail closed:\n{run_stderr}"
    );

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn unsupported generic Option projection verifier");
    assert!(!verify.status.success());
    let verify_stderr = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify_stderr.contains("generic Option projection"),
        "verifier must reject before AST compatibility fallback:\n{verify_stderr}"
    );

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .output()
        .expect("failed to spawn unsupported generic Option projection build");
    assert!(!build.status.success());
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build_stderr.contains("generic Option projection"),
        "native build must reject the unmigrated fallback shape:\n{build_stderr}"
    );
}

#[test]
fn canonical_default_generic_result_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap.mimi");
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result projection run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result projection verifier");
    assert!(
        verify.status.success(),
        "default generic Result projection verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-result-unwrap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Result projection native build");
    assert!(
        build.status.success(),
        "default generic Result projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Result projection native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_result_owned_list_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_owned_list.mimi");
    for explicit in [true, false] {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("run").arg(&fixture);
        if explicit {
            command.arg("--mir");
        }
        let run = command
            .output()
            .expect("failed to spawn generic Result<List> projection run");
        assert_eq!(run.status.code(), Some(41));
    }

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Result<List> projection verifier");
    assert!(
        verify.status.success(),
        "generic Result<List> projection verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-result-owned-list-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn generic Result<List> projection native build");
    assert!(
        build.status.success(),
        "generic Result<List> projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute generic Result<List> projection native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_result_owned_float_list_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests/fixtures/mir_native_generic_result_unwrap_owned_float_list_rejected.mimi");
    for command in ["run", "build", "verify"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture)
            .output()
            .expect("failed to spawn unsupported generic Result<List<f64>> command");
        assert!(
            !output.status.success(),
            "default {command} must reject unsupported Result<List<f64>>"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("generic Result projection") && stderr.contains("Canonical MIR"),
            "default {command} lost its stable fail-closed diagnostic:\n{stderr}"
        );
    }
}

#[test]
fn canonical_default_generic_result_unwrap_or_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or.mimi");
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or run");
    assert_eq!(default_run.status.code(), Some(48));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default generic Result unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-result-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default generic Result unwrap_or native build");
    assert!(
        build.status.success(),
        "default generic Result unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default generic Result unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));

    let i64_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or_i64.mimi");
    let i64_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&i64_fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or i64 run");
    assert_eq!(i64_run.status.code(), Some(7));

    let bool_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or_bool.mimi");
    let bool_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&bool_fixture)
        .output()
        .expect("failed to spawn default generic Result unwrap_or bool run");
    assert_eq!(bool_run.status.code(), Some(0));
}

#[test]
fn canonical_default_generic_result_f64_unwrap_or_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or_f64.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Result f64 unwrap_or run");
    assert_eq!(run.status.code(), Some(42));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Result f64 unwrap_or verifier");
    assert!(
        verify.status.success(),
        "generic Result f64 unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));
}

#[test]
fn canonical_default_generic_result_f64_unwrap_or_rejects_unsupported_shapes() {
    for name in [
        "mir_native_generic_result_unwrap_or_homogeneous_f64_rejected.mimi",
        "mir_native_result_f64_unwrap_or_rejected.mimi",
    ] {
        let fixture = project_root().join("tests").join("fixtures").join(name);
        for command in ["run", "build", "verify"] {
            let output = Command::new(mimi_bin())
                .current_dir(project_root())
                .arg(command)
                .arg(&fixture)
                .output()
                .expect("failed to spawn unsupported generic Result f64 command");
            assert!(
                !output.status.success(),
                "default {command} must reject unsupported Result f64 fallback shape {name}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("generic Result")
                    || stderr.contains("Copy Result")
                    || stderr.contains("variant projection"),
                "default {command} lost its stable fail-closed diagnostic for {name}:\n{stderr}"
            );
        }
    }
}

#[test]
fn canonical_default_generic_result_bool_f64_unwrap_or_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_bool_unwrap_or_f64.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Result<bool,f64> unwrap_or run");
    assert_eq!(run.status.code(), Some(42));
    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Result<bool,f64> unwrap_or verifier");
    assert!(
        verify.status.success(),
        "generic Result<bool,f64> unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));
}

#[test]
fn canonical_default_direct_result_bool_f64_unwrap_or_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_bool_f64_unwrap_or_rejected.mimi");
    for command in ["run", "build", "verify"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture)
            .output()
            .expect("failed to spawn direct Result<bool,f64> fallback command");
        assert!(
            !output.status.success(),
            "default {command} must reject direct Result<bool,f64> fallback"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Result") || stderr.contains("variant projection"),
            "default {command} lost its fail-closed diagnostic:\n{stderr}"
        );
    }
}

#[test]
fn canonical_default_generic_distinct_result_unwrap_or_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_distinct_unwrap_or.mimi");
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default heterogeneous Result unwrap_or run");
    assert_eq!(default_run.status.code(), Some(50));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default heterogeneous Result unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default heterogeneous Result unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-distinct-result-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default heterogeneous Result unwrap_or native build");
    assert!(
        build.status.success(),
        "default heterogeneous Result unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default heterogeneous Result unwrap_or native binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(50));
}

#[test]
fn canonical_default_generic_result_unwrap_or_rejects_unmigrated_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_or_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture)
            .output()
            .expect("failed to spawn unsupported generic Result unwrap_or command");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("generic Result") && !stderr.contains("legacy"),
            "unsupported generic Result unwrap_or must fail closed for {command}:\n{stderr}"
        );
    }
}

#[test]
fn canonical_default_generic_result_projection_trap_and_rejection_are_fail_closed() {
    let trap_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_none.mimi");
    let trap_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&trap_fixture)
        .output()
        .expect("failed to spawn generic Result Err projection run");
    assert!(!trap_run.status.success());
    assert!(String::from_utf8_lossy(&trap_run.stderr).contains("E0800"));

    let rejected_fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&rejected_fixture)
            .output()
            .expect("failed to spawn unsupported generic Result projection command");
        assert!(!output.status.success());
        assert!(
            (String::from_utf8_lossy(&output.stderr).contains("generic Result projection")
                || String::from_utf8_lossy(&output.stderr)
                    .contains("generic-result-projection-v1")),
            "unsupported generic Result projection must fail closed for {command}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn canonical_default_generic_result_distinct_projection_matches_reference_and_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_distinct_unwrap.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic distinct Result projection run");
    assert_eq!(
        run.status.code(),
        Some(41),
        "reference/bytecode run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let binary = std::env::temp_dir().join(format!(
        "mimi-generic-result-distinct-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn generic distinct Result projection native build");
    assert!(
        build.status.success(),
        "native MIR build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute generic distinct Result projection native binary");
    assert_eq!(native.status.code(), Some(41));
}

#[test]
fn canonical_default_generic_result_f64_projection_matches_reference_and_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_f64.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Result f64 projection run");
    assert_eq!(
        run.status.code(),
        Some(42),
        "reference/bytecode run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let binary =
        std::env::temp_dir().join(format!("mimi-generic-result-f64-{}", std::process::id()));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn generic Result f64 projection native build");
    assert!(
        build.status.success(),
        "native MIR build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute generic Result f64 projection native binary");
    assert_eq!(native.status.code(), Some(42));
}

#[test]
fn canonical_default_generic_result_homogeneous_f64_projection_fails_closed() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_result_unwrap_homogeneous_f64_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture)
            .output()
            .expect("failed to spawn homogeneous generic Result f64 command");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("generic Result projection")
                || stderr.contains("generic-result-projection-v1"),
            "homogeneous generic Result f64 must fail closed for {command}:\n{stderr}"
        );
    }
}

#[test]
fn canonical_default_copy_option_bool_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_bool_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<bool> reference run");
    assert_eq!(default_run.status.code(), Some(42));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<bool> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(42));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<bool> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<bool> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-bool-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<bool> native build");
    assert!(
        build.status.success(),
        "default Copy Option<bool> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<bool> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_default_copy_option_i64_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i64_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i64> reference run");
    assert_eq!(default_run.status.code(), Some(41));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<i64> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<i64> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<i64> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-i64-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<i64> native build");
    assert!(
        build.status.success(),
        "default Copy Option<i64> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<i64> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_copy_option_f64_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_f64_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<f64> reference run");
    assert_eq!(default_run.status.code(), Some(42));

    let explicit_mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Copy Option<f64> reference run");
    assert_eq!(explicit_mir_run.status.code(), Some(42));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option<f64> verifier");
    assert!(
        verify.status.success(),
        "default Copy Option<f64> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("No contracts to verify"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-f64-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option<f64> native build");
    assert!(
        build.status.success(),
        "default Copy Option<f64> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option<f64> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_f64_unary_negate_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_negate.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR f64 negate run");
    assert_eq!(
        run.status.code(),
        Some(42),
        "explicit MIR run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-explicit-mir-f64-negate-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR f64 negate build");
    assert!(
        build.status.success(),
        "explicit MIR f64 negate native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR f64 negate native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_f64_add_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_add.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR f64 add run");
    assert_eq!(
        run.status.code(),
        Some(42),
        "explicit MIR f64 add run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary =
        std::env::temp_dir().join(format!("mimi-explicit-mir-f64-add-{}", std::process::id()));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR f64 add build");
    assert!(
        build.status.success(),
        "explicit MIR f64 add native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR f64 add native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_f64_subtract_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_subtract.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR f64 subtract run");
    assert_eq!(
        run.status.code(),
        Some(42),
        "explicit MIR f64 subtract run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-explicit-mir-f64-subtract-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR f64 subtract build");
    assert!(
        build.status.success(),
        "explicit MIR f64 subtract native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR f64 subtract native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_explicit_mir_result_i32_i32_unwrap_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn explicit MIR Result<i32, i32>.unwrap run");
    assert_eq!(
        run.status.code(),
        Some(41),
        "explicit MIR Result unwrap run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-explicit-mir-result-i32-i32-unwrap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn explicit MIR Result unwrap build");
    assert!(
        build.status.success(),
        "explicit MIR Result unwrap native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute explicit MIR Result unwrap native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_explicit_mir_rejects_result_i64_i32_unwrap_before_backend() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i64_i32_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn unsupported explicit MIR Result unwrap run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains(
            "Option/Result unwrap shape is outside the canonical variant projection contract"
        ),
        "unsupported Result unwrap must fail closed before a backend:\n{stderr}"
    );
}

#[test]
fn canonical_default_result_i32_i32_unwrap_matches_all_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result<i32, i32> reference run");
    assert_eq!(default_run.status.code(), Some(41));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result<i32, i32> verifier");
    assert!(
        verify.status.success(),
        "default Copy Result<i32, i32> verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i32-i32-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Result<i32, i32> native build");
    assert!(
        build.status.success(),
        "default Copy Result<i32, i32> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Result<i32, i32> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(41));
}

#[test]
fn canonical_default_rejects_result_i64_i32_unwrap_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i64_i32_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default unsupported Copy Result run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("Copy Result<i32, i32> projection candidate is outside complete coverage"),
        "unsupported Result projection must fail closed before legacy:\n{stderr}"
    );

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default unsupported Copy Result verifier");
    assert!(!verify.status.success());
    let verify_stderr = String::from_utf8_lossy(&verify.stderr);
    assert!(
        verify_stderr
            .contains("Copy Result<i32, i32> projection candidate is outside complete coverage"),
        "unsupported Result projection verifier must fail closed before legacy:\n{verify_stderr}"
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i64-i32-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default unsupported Copy Result native build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        build_stderr.contains("Copy Result<i32, i32> variant MIR island")
            || build_stderr.contains("Copy Result<i32, i32> projection candidate"),
        "unsupported Result projection native build must fail closed before LLVM:\n{build_stderr}"
    );
}

#[test]
fn canonical_default_result_i32_i32_err_unwrap_preserves_active_tag_trap() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap_err.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result Err unwrap run");
    assert_eq!(default_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&default_run.stderr).contains("E0800"));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result Err unwrap verifier");
    assert!(!verify.status.success());
    let verify_stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(verify_stdout.contains("canonical MIR"), "{verify_stdout}");
    assert!(verify_stdout.contains("E0800"), "{verify_stdout}");

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i32-i32-err-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Result Err unwrap native build");
    assert!(
        build.status.success(),
        "default Copy Result Err unwrap native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Result Err unwrap native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0800"));
}

#[test]
fn canonical_default_result_i32_i32_unwrap_or_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i32_unwrap_or.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result unwrap_or reference run");
    assert_eq!(default_run.status.code(), Some(14));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Result unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default Copy Result unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-result-i32-i32-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Result unwrap_or native build");
    assert!(
        build.status.success(),
        "default Copy Result unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Result unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(14));
}

#[test]
fn canonical_default_option_i32_unwrap_or_matches_reference_bytecode_native() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i32_unwrap_or.mimi");

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option unwrap_or reference run");
    assert_eq!(default_run.status.code(), Some(14));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Copy Option unwrap_or verifier");
    assert!(
        verify.status.success(),
        "default Copy Option unwrap_or verifier failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("canonical MIR"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-copy-option-i32-unwrap-or-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default Copy Option unwrap_or native build");
    assert!(
        build.status.success(),
        "default Copy Option unwrap_or native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute default Copy Option unwrap_or native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(14));
}

#[test]
fn canonical_default_rejects_option_i64_unwrap_or_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i64_unwrap_or_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let binary = std::env::temp_dir().join(format!(
            "mimi-default-copy-option-i64-unwrap-or-rejected-{}",
            std::process::id()
        ));
        let mut invocation = Command::new(mimi_bin());
        invocation
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture);
        if command == "build" {
            invocation.arg("-o").arg(&binary);
        }
        let output = invocation
            .output()
            .expect("failed to spawn unsupported Copy Option unwrap_or command");
        let _ = fs::remove_file(&binary);
        assert!(!output.status.success(), "{command} must fail closed");
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains("Copy Option<i64> variant candidate is not eligible")
                || diagnostics.contains("Copy Option<i64>"),
            "{command} must reject before legacy/backend:\n{diagnostics}"
        );
    }
}

#[test]
fn canonical_default_rejects_result_i64_i32_unwrap_or_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_i64_i32_unwrap_or_rejected.mimi");
    for command in ["run", "verify", "build"] {
        let binary = std::env::temp_dir().join(format!(
            "mimi-default-copy-result-i64-i32-unwrap-or-rejected-{}",
            std::process::id()
        ));
        let mut invocation = Command::new(mimi_bin());
        invocation
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture);
        if command == "build" {
            invocation.arg("-o").arg(&binary);
        }
        let output = invocation
            .output()
            .expect("failed to spawn unsupported Copy Result unwrap_or command");
        let _ = fs::remove_file(&binary);
        assert!(!output.status.success(), "{command} must fail closed");
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains(
                "Copy Result<i32, i32> projection candidate is outside complete coverage"
            ),
            "{command} must reject before legacy/backend:\n{diagnostics}"
        );
    }
}

#[test]
fn canonical_default_rejects_mixed_copy_option_bool_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_bool_mixed_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mixed Copy Option<bool> default run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("Copy Option<bool>"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_default_rejects_mixed_copy_option_i64_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_i64_mixed_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mixed Copy Option<i64> default run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("S116 Copy Option<i64>"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_default_rejects_mixed_copy_option_f64_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_f64_mixed_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn mixed Copy Option<f64> default run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("S117 Copy Option<f64>"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_mir_native_builds_non_copy_option_string_glue() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Option<string> reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-option-string-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Option<string> native build");
    assert!(
        build.status.success(),
        "canonical MIR Option<string> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Option<string> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_option_string_switch_move_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string_switch_move.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Option<string> SwitchMove reference run");
    assert_eq!(mir_run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-option-string-switch-move-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Option<string> SwitchMove native build");
    assert!(
        build.status.success(),
        "canonical MIR Option<string> SwitchMove native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Option<string> SwitchMove binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));

    // The same exact island must be selected by the production defaults after
    // all-consumer preflight; this is the switch-over proof, not merely an
    // explicit `--mir` smoke test.
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Canonical MIR Option<string> run");
    assert_eq!(default_run.status.code(), Some(48));

    let default_binary = std::env::temp_dir().join(format!(
        "mimi-default-option-string-switch-move-{}",
        std::process::id()
    ));
    let default_build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&default_binary)
        .output()
        .expect("failed to spawn default Canonical MIR Option<string> build");
    assert!(
        default_build.status.success(),
        "default Canonical MIR Option<string> build failed:\n{}",
        String::from_utf8_lossy(&default_build.stderr)
    );
    let default_native_run = Command::new(&default_binary)
        .output()
        .expect("failed to execute default Canonical MIR Option<string> binary");
    let _ = fs::remove_file(&default_binary);
    assert_eq!(default_native_run.status.code(), Some(48));

    let default_verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Canonical MIR Option<string> verifier");
    assert!(
        default_verify.status.success(),
        "default Canonical MIR Option<string> verify failed:\n{}",
        String::from_utf8_lossy(&default_verify.stderr)
    );
    let verify_output = format!(
        "{}{}",
        String::from_utf8_lossy(&default_verify.stdout),
        String::from_utf8_lossy(&default_verify.stderr)
    );
    assert!(verify_output.contains("consume"), "{verify_output}");
    assert!(verify_output.contains("contract proven"), "{verify_output}");
}

#[test]
fn canonical_mir_native_result_string_i32_switch_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_result_string_i32_switch_move.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-switch-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> native build");
    assert!(
        build.status.success(),
        "canonical MIR Result<string, i32> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result<string, i32> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_result_string_i32_clone_drop_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_result_string_i32_glue.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> glue reference run");
    assert_eq!(mir_run.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-glue-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result<string, i32> glue native build");
    assert!(
        build.status.success(),
        "canonical MIR Result<string, i32> glue native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result<string, i32> glue native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_result_string_i32_call_return_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_string_i32_call_return.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result call/return reference run");
    assert_eq!(mir_run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-call-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result call/return native build");
    assert!(
        build.status.success(),
        "canonical MIR Result call/return native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result call/return native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));
}

#[test]
fn canonical_mir_native_result_list_i32_call_return_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_list_i32_call_return.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result<List<i32>, i32> reference run");
    assert_eq!(mir_run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-list-i32-call-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result<List<i32>, i32> native build");
    assert!(
        build.status.success(),
        "canonical MIR Result<List<i32>, i32> native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result<List<i32>, i32> native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result<List<i32>, i32> verifier");
    assert!(
        verify.status.success(),
        "canonical MIR Result<List<i32>, i32> verification failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let verify_output = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(verify_output.contains("use_ok"), "{verify_output}");
    assert!(verify_output.contains("use_err"), "{verify_output}");
    assert!(verify_output.contains("2/2 verified"), "{verify_output}");
}

#[test]
fn default_route_result_list_i32_call_return_matches_canonical_backends() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_list_i32_call_return.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default managed Result<List<i32>, i32> run");
    assert_eq!(run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-result-list-i32-call-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default managed Result<List<i32>, i32> build");
    assert!(
        build.status.success(),
        "default managed Result<List<i32>, i32> build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default managed Result<List<i32>, i32> binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(48));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default managed Result<List<i32>, i32> verifier");
    assert!(
        verify.status.success(),
        "default managed Result<List<i32>, i32> verification failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(output.contains("2/2 verified"), "{output}");
}

#[test]
fn default_route_result_string_i32_call_return_matches_canonical_backends() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_string_i32_call_return.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default managed Result<string, i32> run");
    assert_eq!(run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-result-string-i32-call-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default managed Result<string, i32> build");
    assert!(
        build.status.success(),
        "default managed Result<string, i32> build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default managed Result<string, i32> binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(48));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default managed Result<string, i32> verifier");
    assert!(
        verify.status.success(),
        "default managed Result<string, i32> verification failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(output.contains("2/2 verified"), "{output}");
}

#[test]
fn default_route_result_i64_bool_list_calls_match_canonical_backends() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_list_i64_bool_call_return.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default managed Result<List<i64|bool>, i32> run");
    assert_eq!(run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-result-list-i64-bool-call-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default managed Result<List<i64|bool>, i32> build");
    assert!(
        build.status.success(),
        "default managed Result<List<i64|bool>, i32> build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default managed Result<List<i64|bool>, i32> binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(48));

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default managed Result<List<i64|bool>, i32> verifier");
    assert!(
        verify.status.success(),
        "default managed Result<List<i64|bool>, i32> verification failed:\n{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(output.contains("2/2 verified"), "{output}");
}

#[test]
fn canonical_mir_result_list_f64_call_fails_closed_before_backends() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_list_f64_call_rejected.mimi");
    for command in ["run", "build", "verify"] {
        let mut invocation = Command::new(mimi_bin());
        invocation
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture)
            .arg("--mir");
        if command == "build" {
            let output_path = std::env::temp_dir().join(format!(
                "mimi-rejected-result-list-f64-{}",
                std::process::id()
            ));
            invocation.arg("-o").arg(&output_path);
        }
        let output = invocation
            .output()
            .expect("failed to spawn rejected Result<List<f64>, i32> command");
        assert!(
            !output.status.success(),
            "canonical MIR {command} unexpectedly accepted Result<List<f64>, i32>"
        );
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains("Copy scalar") || diagnostics.contains("canonical"),
            "{command} lost the stable fail-closed diagnostic:\n{diagnostics}"
        );
    }
}

#[test]
fn default_route_result_list_f64_call_fails_closed_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_list_f64_call_rejected.mimi");
    for command in ["run", "build", "verify"] {
        let mut invocation = Command::new(mimi_bin());
        invocation
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture);
        if command == "build" {
            let output_path = std::env::temp_dir().join(format!(
                "mimi-default-rejected-result-list-f64-{}",
                std::process::id()
            ));
            invocation.arg("-o").arg(&output_path);
        }
        let output = invocation
            .output()
            .expect("failed to spawn default rejected Result<List<f64>, i32> command");
        assert!(
            !output.status.success(),
            "default route unexpectedly accepted Result<List<f64>, i32> for {command}"
        );
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains("managed Result direct-call")
                || diagnostics.contains("canonical")
                || diagnostics.contains("Copy scalar"),
            "{command} lost the stable fail-closed diagnostic:\n{diagnostics}"
        );
    }
}

#[test]
fn canonical_mir_native_result_string_i32_call_return_multipath_matches_mir_run() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_result_string_i32_call_return_multipath.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR Result multi-path reference run");
    assert_eq!(mir_run.status.code(), Some(48));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-result-string-i32-call-return-multipath-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR Result multi-path native build");
    assert!(
        build.status.success(),
        "canonical MIR Result multi-path native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR Result multi-path native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(48));
}

#[test]
fn canonical_mir_native_option_overflow_matches_mir_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_overflow.mimi");
    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR variant trap oracle");
    assert_eq!(mir_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mir_run.stderr).contains("E0802"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-variant-trap-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR variant trap build");
    assert!(
        build.status.success(),
        "canonical MIR variant trap build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native_run = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR variant trap binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native_run.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native_run.stderr).contains("E0802"));
}

#[test]
fn canonical_mir_native_rejects_mixed_variant_payload_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_variant_mixed_payload_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-variant-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR variant build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR native backend rejected"));
    assert!(stderr.contains("flat Copy variant contract"));
    assert!(stderr.contains("mixed payload ABI"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_builds_nested_tuple_variant_construct_and_drop() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-option-nested-tuple-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR nested tuple Option build");
    let _ = fs::remove_file(&binary);
    assert!(
        build.status.success(),
        "canonical MIR nested tuple Option build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
}

#[test]
fn default_route_rejects_non_exhaustive_option_string_switch_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_option_string_default_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-default-option-string-switch-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected default Option<string> build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(
        stderr.contains("S30 non-Copy Option<string> variant candidate"),
        "{stderr}"
    );
    assert!(!stderr.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_mir_native_rejects_record_with_unsupported_child_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_record_noncopy_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-record-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR record build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR native backend rejected"));
    assert!(
        stderr.contains("outside the scalar/String/List<Copy scalar>/Set<Copy scalar>/tuple ABI")
    );
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_rejects_recursive_tuple_with_list_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_recursive_tuple_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-recursive-tuple-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR recursive tuple build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR native backend rejected"));
    assert!(stderr.contains("outside the scalar/String/tuple ABI"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_native_build_rejects_unsupported_shape_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_f64_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical MIR native build");
    let _ = fs::remove_file(&binary);
    assert!(
        !build.status.success(),
        "unsupported native MIR must fail closed"
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("canonical MIR native backend rejected")
            && stderr.contains("binary operator")
            && stderr.contains("finite-only Copy f64 contract"),
        "unexpected canonical native rejection:\n{stderr}"
    );
    assert!(
        stderr.contains("canonical MIR native backend capability check failed"),
        "rejection must identify the canonical MIR gate:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "native MIR rejection must not fall back to another backend:\n{stderr}"
    );
}

#[test]
fn canonical_mir_builtin_abs_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_abs.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR abs fixture");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR abs fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_builtin_abs_rejects_unsupported_width_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_abs_i32_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR abs fixture");
    assert!(
        !output.status.success(),
        "unsupported abs width must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error")
            && stderr.contains("builtin 'abs'")
            && stderr.contains("canonical contract accepts signed i64 or f64"),
        "unexpected canonical abs rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical abs rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_builtin_min_max_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_min_max.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR min/max fixture");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR min/max fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_builtin_min_rejects_unsupported_abi_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_builtin_min_f64_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR min fixture");
    assert!(
        !output.status.success(),
        "unsupported min ABI must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error")
            && stderr.contains("builtin 'min'")
            && stderr.contains("canonical contract accepts signed i64"),
        "unexpected canonical min rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical min rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_convert_i32_to_i64_cli_smoke() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_convert_i32_to_i64_min_max.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR conversion fixture");
    assert_eq!(
        output.status.code(),
        Some(42),
        "canonical MIR conversion fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_convert_i32_to_f64_rejects_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_convert_i32_to_f64_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical conversion fixture");
    assert!(
        !output.status.success(),
        "i32 to f64 conversion must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error")
            && stderr.contains("conversion")
            && stderr.contains("accepted: same Copy scalar type"),
        "unexpected canonical conversion rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical conversion rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_run_cli_executes_imported_user_call_graph() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("projects")
        .join("consumer")
        .join("main.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn imported canonical MIR program");
    assert_eq!(
        output.status.code(),
        Some(0),
        "imported canonical MIR run failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_run_cli_executes_list_index_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("core_list_index.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List index program");
    assert!(
        output.status.success(),
        "canonical List index program failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn canonical_mir_native_build_list_index_matches_reference_and_bytecode() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_index.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-index-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List native build");
    assert!(
        build.status.success(),
        "canonical List native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_set_to_list_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_to_list.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical Set.to_list reference run");
    assert_eq!(
        reference.status.code(),
        Some(42),
        "canonical Set.to_list reference run failed:\n{}",
        String::from_utf8_lossy(&reference.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-set-to-list-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical Set.to_list native build");
    assert!(
        build.status.success(),
        "canonical Set.to_list native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical Set.to_list native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(42),
        "canonical Set.to_list native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );
}

#[test]
fn canonical_mir_native_set_function_contains_matches_reference_and_default() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_contains_function.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical Set function-form MIR dump");
    assert!(
        mir.status.success(),
        "canonical Set function-form MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    assert!(
        String::from_utf8_lossy(&mir.stdout).contains("set_op")
            && String::from_utf8_lossy(&mir.stdout).contains("Contains"),
        "bare contains(Set, T) did not materialize as the canonical SetOp:\n{}",
        String::from_utf8_lossy(&mir.stdout)
    );

    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical Set function-form reference run");
    assert_eq!(
        reference.status.code(),
        Some(42),
        "canonical Set function-form reference run failed:\n{}",
        String::from_utf8_lossy(&reference.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-set-contains-function-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical Set function-form native build");
    assert!(
        build.status.success(),
        "canonical Set function-form native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical Set function-form native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set function-form run");
    assert_eq!(
        default_run.status.code(),
        Some(42),
        "default Set function-form run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default Set function-form native build");
    assert!(default_ir.status.success());
    assert!(
        String::from_utf8_lossy(&default_ir.stdout).contains("define i32 @main()"),
        "default build did not select canonical Set contains:\n{}",
        String::from_utf8_lossy(&default_ir.stdout)
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set function-form verifier");
    assert!(
        default_verification.status.success(),
        "default Set function-form verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stdout),
        String::from_utf8_lossy(&default_verification.stderr)
    );
    assert!(
        String::from_utf8_lossy(&default_verification.stdout)
            .contains("canonical MIR ensures contract proven"),
        "default verifier did not consume the canonical Set program:\n{}",
        String::from_utf8_lossy(&default_verification.stdout)
    );
}

#[test]
fn canonical_mir_set_contains_println_bool_matches_all_production_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_contains_println.mimi");
    let expected = "true\nfalse\ntrue\n";

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical Set/println MIR dump");
    assert!(mir.status.success());
    let mir_stdout = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_stdout.contains("SetOp::Contains") || mir_stdout.contains("set_op"));
    assert!(
        mir_stdout.contains("PrintlnBool"),
        "MIR omitted println(bool):\n{mir_stdout}"
    );

    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical Set/println reference run");
    assert_eq!(reference.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&reference.stdout), expected);

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set/println run");
    assert_eq!(default_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), expected);

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-set-contains-println-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical Set/println native build");
    assert!(
        build.status.success(),
        "canonical Set/println native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical Set/println native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default Set/println native IR build");
    assert!(default_ir.status.success());
    let ir = String::from_utf8_lossy(&default_ir.stdout);
    assert!(ir.contains("define i32 @main("));
    assert!(ir.contains("@printf"));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default Set/println verifier");
    assert!(
        verification.status.success(),
        "default Set/println verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_rejects_unsupported_println_before_any_backend() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_println_non_bool_rejected.mimi");
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical println build");
    assert!(!explicit.status.success());
    let stderr = String::from_utf8_lossy(&explicit.stderr);
    assert!(
        stderr.contains("canonical contract accepts signed i32 or i64"),
        "unsupported println lost its stable canonical diagnostic:\n{stderr}"
    );

    let default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn compatibility non-bool println run");
    assert!(default.status.success());
    assert_eq!(String::from_utf8_lossy(&default.stdout), "true\nlegacy\n");
}

#[test]
fn canonical_mir_standalone_bool_println_uses_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_println_bool_standalone.mimi");
    let expected = "true\nfalse\n";

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone println MIR dump");
    assert!(mir.status.success());
    let mir_stdout = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_stdout.contains("PrintlnBool"));
    assert!(!mir_stdout.contains("SetOp::") && !mir_stdout.contains("ListOp::"));

    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn standalone canonical run");
    assert_eq!(explicit.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&explicit.stdout), expected);

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone default run");
    assert_eq!(default_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), expected);

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn standalone default native build");
    assert!(
        default_ir.status.success(),
        "standalone default native build failed:\n{}",
        String::from_utf8_lossy(&default_ir.stderr)
    );
    let ir = String::from_utf8_lossy(&default_ir.stdout);
    assert!(ir.contains("define i32 @main("));
    assert!(ir.contains("@printf"));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone verifier");
    assert!(verification.status.success());
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_standalone_integer_println_matches_all_production_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_println_int.mimi");
    let expected = "-7\n9223372036854775806\n";

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone integer println MIR dump");
    assert!(mir.status.success());
    let mir_stdout = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_stdout.contains("PrintlnInt"));
    assert!(!mir_stdout.contains("SetOp::") && !mir_stdout.contains("ListOp::"));

    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn standalone integer canonical run");
    assert_eq!(explicit.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&explicit.stdout), expected);

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone integer default run");
    assert_eq!(default_run.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&default_run.stdout), expected);

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-println-int-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn standalone integer default native build");
    assert!(
        build.status.success(),
        "standalone integer default native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute standalone integer native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn standalone integer native IR build");
    assert!(default_ir.status.success());
    let ir = String::from_utf8_lossy(&default_ir.stdout);
    assert!(ir.contains("define i32 @main("));
    assert!(ir.contains("@printf") && ir.contains("c\"%ld\\00"));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn standalone integer verifier");
    assert!(
        verification.status.success(),
        "standalone integer verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_default_does_not_promote_list_function_contains() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_contains_rejected.mimi");

    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected List function-form contains build");
    assert!(
        !explicit.status.success(),
        "List contains unexpectedly entered canonical MIR:\n{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(
        String::from_utf8_lossy(&explicit.stderr).contains("not a materialized MIR function")
            || String::from_utf8_lossy(&explicit.stderr).contains("canonical MIR"),
        "List contains rejection lost its stable canonical boundary:\n{}",
        String::from_utf8_lossy(&explicit.stderr)
    );

    let default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn compatibility List function-form contains build");
    assert!(
        default.status.success(),
        "compatibility List contains build failed:\n{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&default.stdout).contains("mimi_mir_set"),
        "List contains was promoted to the canonical Set backend:\n{}",
        String::from_utf8_lossy(&default.stdout)
    );
}

#[test]
fn canonical_mir_std_set_generic_facade_is_atomic_across_consumers() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("std_set.mimi");

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical std::set reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(0),
        "canonical std::set reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-std-set-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical std::set native build");
    assert!(
        build.status.success(),
        "canonical std::set native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical std::set native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(0),
        "canonical std::set native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical std::set verifier");
    assert!(
        verification.status.success(),
        "canonical std::set verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );

    // The typed scalar Set facade is now a complete default-switch island.
    // All three default entry points must select the same canonical program;
    // there is no per-consumer fallback after selection.
    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default std::set run");
    assert_eq!(
        default_run.status.code(),
        Some(0),
        "default std::set run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_build_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default std::set native emit-ir");
    assert!(
        default_build_ir.status.success(),
        "default std::set native build failed:\n{}",
        String::from_utf8_lossy(&default_build_ir.stderr)
    );
    let default_ir = String::from_utf8_lossy(&default_build_ir.stdout);
    assert!(
        default_ir.contains("mimi_mir_set_to_list_scalar"),
        "default build did not select the canonical Set island:\n{default_ir}"
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default std::set verifier");
    assert!(
        default_verification.status.success(),
        "default std::set verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stderr),
        String::from_utf8_lossy(&default_verification.stdout)
    );
}

#[test]
fn canonical_mir_generic_list_concat_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_concat.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List.concat MIR dump");
    assert!(
        mir.status.success(),
        "canonical generic List.concat MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    let mir_text = String::from_utf8_lossy(&mir.stdout);
    assert!(
        mir_text.contains("Concat"),
        "MIR dump omitted ListOp::Concat:\n{mir_text}"
    );
    assert!(
        mir_text.contains("list_contract=MirListOperationContract"),
        "MIR dump omitted the canonical TypeDesc receipt:\n{mir_text}"
    );

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List.concat reference run");
    assert_eq!(
        mir_run.status.code(),
        Some(5),
        "canonical generic List.concat reference run failed:\n{}",
        String::from_utf8_lossy(&mir_run.stderr)
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-concat-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List.concat native build");
    assert!(
        build.status.success(),
        "canonical generic List.concat native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List.concat native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(
        native.status.code(),
        Some(5),
        "canonical generic List.concat native run failed:\n{}",
        String::from_utf8_lossy(&native.stderr)
    );

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List.concat verifier");
    assert!(
        verification.status.success(),
        "canonical generic List.concat verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List.concat run");
    assert_eq!(
        default_run.status.code(),
        Some(5),
        "default generic List.concat run failed:\n{}",
        String::from_utf8_lossy(&default_run.stderr)
    );

    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List.concat native emit-ir");
    assert!(
        default_ir.status.success(),
        "default generic List.concat build failed:\n{}",
        String::from_utf8_lossy(&default_ir.stderr)
    );
    assert!(
        String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_concat_scalar"),
        "default route did not select canonical List.concat native helper:\n{}",
        String::from_utf8_lossy(&default_ir.stdout)
    );

    let default_verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List.concat verifier");
    assert!(
        default_verification.status.success(),
        "default generic List.concat verification failed:\n{}\n{}",
        String::from_utf8_lossy(&default_verification.stderr),
        String::from_utf8_lossy(&default_verification.stdout)
    );
}

#[test]
fn canonical_mir_generic_list_construct_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_construct.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List construction MIR dump");
    assert!(
        mir.status.success(),
        "canonical generic List construction MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    let mir_text = String::from_utf8_lossy(&mir.stdout);
    assert!(
        mir_text.contains("construct_list"),
        "MIR dump omitted ConstructList"
    );
    assert!(
        mir_text.contains("list_construct_contract=MirListConstructContract"),
        "MIR dump omitted the canonical construction TypeDesc receipt:\n{mir_text}"
    );

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List construction reference run");
    assert_eq!(mir_run.status.code(), Some(1));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-construct-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List construction native build");
    assert!(
        build.status.success(),
        "canonical generic List construction native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List construction native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(1));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List construction verifier");
    assert!(verification.status.success());
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List construction run");
    assert_eq!(default_run.status.code(), Some(1));
    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List construction native emit-ir");
    assert!(default_ir.status.success());
    assert!(String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_new_scalar"));
}

#[test]
fn default_route_rejects_non_copy_generic_list_construct_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_construct_rejected.mimi");
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected generic List construction explicit build");
    assert!(!explicit.status.success());
    assert!(String::from_utf8_lossy(&explicit.stderr).contains("canonical MIR"));

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic List construction default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("generic List facade"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn canonical_mir_generic_list_projection_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_projection.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List projection MIR dump");
    assert!(
        mir.status.success(),
        "canonical generic List projection MIR dump failed:\n{}",
        String::from_utf8_lossy(&mir.stderr)
    );
    let mir_text = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_text.contains("project"));
    assert!(mir_text.contains("list_index=MirListIndexProjectionContract"));

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List projection reference run");
    assert_eq!(mir_run.status.code(), Some(41));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-projection-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List projection native build");
    assert!(
        build.status.success(),
        "canonical generic List projection native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List projection native binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(41));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List projection verifier");
    assert!(verification.status.success());
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List projection run");
    assert_eq!(default_run.status.code(), Some(41));
    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List projection native emit-ir");
    assert!(default_ir.status.success());
    assert!(String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_get_scalar"));
}

#[test]
fn canonical_mir_generic_list_index_one_projection_is_atomic_across_consumers_and_default_route() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_projection_index_one.mimi");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List index-one MIR dump");
    assert!(mir.status.success());
    assert!(
        String::from_utf8_lossy(&mir.stdout).contains("list_index=MirListIndexProjectionContract")
    );

    let mir_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical generic List index-one reference run");
    assert_eq!(mir_run.status.code(), Some(41));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-generic-list-index-one-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical generic List index-one native build");
    assert!(build.status.success());
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical generic List index-one native binary");
    let _ = std::fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(41));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn canonical generic List index-one verifier");
    assert!(verification.status.success());
    let verification_text = format!(
        "{}{}",
        String::from_utf8_lossy(&verification.stdout),
        String::from_utf8_lossy(&verification.stderr)
    );
    assert!(verification_text.contains("canonical MIR ensures contract proven"));

    let default_run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default generic List index-one run");
    assert_eq!(default_run.status.code(), Some(41));
    let default_ir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default generic List index-one native emit-ir");
    assert!(default_ir.status.success());
    assert!(String::from_utf8_lossy(&default_ir.stdout).contains("mimi_mir_list_get_scalar"));
}

#[test]
fn default_route_rejects_non_copy_generic_list_projection_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_projection_rejected.mimi");
    let explicit = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected generic List projection explicit build");
    assert!(!explicit.status.success());
    assert!(String::from_utf8_lossy(&explicit.stderr).contains("canonical MIR"));

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic List projection default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("default Canonical MIR route rejected"),
        "{stderr}"
    );
    assert!(stderr.contains("generic List facade"), "{stderr}");
    assert!(!stderr.contains("bytecode runtime error"), "{stderr}");
}

#[test]
fn default_route_rejects_non_copy_generic_list_concat_without_legacy_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_list_concat_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic List.concat default run");
    assert!(
        !run.status.success(),
        "non-Copy generic concat must fail closed"
    );
    let run_stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run_stderr.contains("default Canonical MIR route rejected"),
        "unexpected default generic concat diagnostic:\n{run_stderr}"
    );
    assert!(run_stderr.contains("generic List facade"), "{run_stderr}");
    assert!(
        !run_stderr.contains("bytecode runtime error"),
        "{run_stderr}"
    );

    let binary = std::env::temp_dir().join(format!(
        "mimi-default-generic-list-concat-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected generic List.concat default build");
    let _ = fs::remove_file(&binary);
    assert!(
        !build.status.success(),
        "non-Copy generic concat build must fail closed"
    );
    let build_stderr = String::from_utf8_lossy(&build.stderr);
    assert!(build_stderr.contains("default Canonical MIR route rejected"));
    assert!(build_stderr.contains("generic List facade"));
    assert!(
        !build_stderr.contains("E0700"),
        "legacy compiler leaked into route failure"
    );
}

#[test]
fn canonical_default_does_not_promote_non_facade_set_program() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_set_to_list.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--emit-ir")
        .output()
        .expect("failed to spawn default non-facade Set build");
    assert!(
        output.status.success(),
        "default non-facade Set build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ir = String::from_utf8_lossy(&output.stdout);
    assert!(
        !ir.contains("mimi_mir_set_to_list_scalar"),
        "an unqualified Set program was promoted to the generic facade island:\n{ir}"
    );
}

#[test]
fn canonical_mir_native_build_bool_list_index_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_bool.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical bool List reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-bool-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical bool List native build");
    assert!(
        build.status.success(),
        "canonical bool List native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical bool List native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_list_drop_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_drop.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List drop reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-drop-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List drop native build");
    assert!(
        build.status.success(),
        "canonical List drop native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List drop native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));
}

#[test]
fn canonical_mir_native_list_return_abi_matches_reference() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_return.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List return reference run");
    assert_eq!(reference.status.code(), Some(20));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-return-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List return native build");
    assert!(
        build.status.success(),
        "canonical List return native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List return native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(20));
}

#[test]
fn canonical_mir_native_list_index_oob_matches_mir_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_list_oob.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical List OOB reference run");
    assert_eq!(reference.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&reference.stderr).contains("E0803"));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-oob-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical List OOB native build");
    assert!(
        build.status.success(),
        "canonical List OOB native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical List OOB native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&native.stderr).contains("E0803"));
}

#[test]
fn canonical_mir_native_rejects_string_list_before_llvm_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-native-list-string-rejected-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn rejected canonical string List build");
    let _ = fs::remove_file(&binary);
    assert!(!build.status.success());
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(stderr.contains("canonical MIR build error"));
    assert!(stderr.contains("Copy scalar"));
    assert!(!stderr.contains("bytecode runtime error"));
}

#[test]
fn canonical_mir_run_cli_rejects_unsupported_list_shape_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn unsupported canonical MIR program");
    assert!(
        !output.status.success(),
        "unsupported MIR shape must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical MIR build error") && stderr.contains("Copy scalar"),
        "unexpected canonical rejection:\n{stderr}"
    );
    assert!(
        !stderr.contains("bytecode runtime error"),
        "canonical rejection must not fall back to the legacy runtime:\n{stderr}"
    );
}

#[test]
fn canonical_mir_verifier_proves_branch_contract() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_branch_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(
        output.status.success(),
        "canonical MIR verifier failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_m1_m3_cli_acceptance_has_real_proofs_and_business_observations() {
    let cases = [
        ("mir_m1_record_list_chain.mimi", Some(6), "", "1/1 verified"),
        (
            "mir_m3_flow_retry.mimi",
            Some(0),
            "100\n95\n",
            "1/1 verified",
        ),
        (
            "mir_m3_flow_retry_helper_result.mimi",
            Some(0),
            "100\n100\n95\n",
            "1/1 verified",
        ),
        (
            "mir_m3_flow_retry_string_state.mimi",
            Some(0),
            "invalid amount\ncredit\n",
            "1/1 verified",
        ),
        (
            "mir_m3_flow_multifield_string_source_receipt.mimi",
            Some(0),
            "source\n",
            "1/1 verified",
        ),
    ];

    for (fixture_name, expected_exit, expected_stdout, expected_summary) in cases {
        let fixture = project_root()
            .join("tests")
            .join("fixtures")
            .join(fixture_name);

        let verification = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("verify")
            .arg(&fixture)
            .arg("--mir")
            .output()
            .expect("failed to spawn M1/M3 Canonical MIR verifier");
        assert!(
            verification.status.success(),
            "{fixture_name} verifier failed:\n{}\n{}",
            String::from_utf8_lossy(&verification.stderr),
            String::from_utf8_lossy(&verification.stdout)
        );
        let verify_stdout = String::from_utf8_lossy(&verification.stdout);
        assert!(
            verify_stdout.contains("canonical MIR ensures contract proven"),
            "{fixture_name} did not produce a real proof:\n{verify_stdout}"
        );
        assert!(!verify_stdout.contains("No contracts to verify"));
        assert!(verify_stdout.contains(expected_summary));

        let reference = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("run")
            .arg(&fixture)
            .arg("--mir")
            .output()
            .expect("failed to spawn M1/M3 Canonical MIR reference run");
        assert_eq!(reference.status.code(), expected_exit);
        assert_eq!(
            String::from_utf8_lossy(&reference.stdout),
            expected_stdout,
            "{fixture_name} reference business observation diverged"
        );

        let binary = std::env::temp_dir().join(format!(
            "mimi-m1-m3-cli-{}-{}",
            std::process::id(),
            fixture_name
        ));
        let build = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("build")
            .arg(&fixture)
            .arg("--mir")
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("failed to spawn M1/M3 Canonical MIR native build");
        assert!(
            build.status.success(),
            "{fixture_name} native build failed:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let native = Command::new(&binary)
            .output()
            .expect("failed to execute M1/M3 Canonical MIR native binary");
        let _ = fs::remove_file(&binary);
        assert_eq!(native.status.code(), expected_exit);
        assert_eq!(
            String::from_utf8_lossy(&native.stdout),
            expected_stdout,
            "{fixture_name} native business observation diverged"
        );
        assert_eq!(String::from_utf8_lossy(&native.stderr), "");

        let default_verification = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("verify")
            .arg(&fixture)
            .output()
            .expect("failed to spawn default-route verifier");
        assert!(
            default_verification.status.success(),
            "{fixture_name} default verifier failed:\n{}\n{}",
            String::from_utf8_lossy(&default_verification.stderr),
            String::from_utf8_lossy(&default_verification.stdout)
        );
        let default_verify_stdout = String::from_utf8_lossy(&default_verification.stdout);
        assert!(default_verify_stdout.contains("canonical MIR ensures contract proven"));
        assert!(!default_verify_stdout.contains("No contracts to verify"));
        assert!(default_verify_stdout.contains(expected_summary));

        let default_reference = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("run")
            .arg(&fixture)
            .output()
            .expect("failed to spawn default-route reference run");
        assert_eq!(default_reference.status.code(), expected_exit);
        assert_eq!(
            String::from_utf8_lossy(&default_reference.stdout),
            expected_stdout,
            "{fixture_name} default-route business observation diverged"
        );

        let default_binary = std::env::temp_dir().join(format!(
            "mimi-m1-m3-cli-default-{}-{}",
            std::process::id(),
            fixture_name
        ));
        let default_build = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("build")
            .arg(&fixture)
            .arg("-o")
            .arg(&default_binary)
            .output()
            .expect("failed to spawn default-route native build");
        assert!(
            default_build.status.success(),
            "{fixture_name} default native build failed:\n{}",
            String::from_utf8_lossy(&default_build.stderr)
        );
        let default_native = Command::new(&default_binary)
            .output()
            .expect("failed to execute default-route native binary");
        let _ = fs::remove_file(&default_binary);
        assert_eq!(default_native.status.code(), expected_exit);
        assert_eq!(
            String::from_utf8_lossy(&default_native.stdout),
            expected_stdout,
            "{fixture_name} default-route native observation diverged"
        );
        assert_eq!(String::from_utf8_lossy(&default_native.stderr), "");
    }
}

#[test]
fn canonical_f64_flow_cli_uses_mir_execution_and_reports_float_verifier_boundary() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_r6_flow_f64_cross_state_receipt.mimi");

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default f64 Flow run");
    assert!(
        run.status.success(),
        "default f64 Flow run failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "");
    assert_eq!(String::from_utf8_lossy(&run.stderr), "");

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-f64-flow-default-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn default f64 Flow native build");
    assert!(
        build.status.success(),
        "default f64 Flow native build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute default f64 Flow native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&native.stdout), "");
    assert_eq!(String::from_utf8_lossy(&native.stderr), "");

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn default f64 Flow verifier");
    assert!(
        verification.status.success(),
        "default f64 Flow verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    let verify_stdout = String::from_utf8_lossy(&verification.stdout);
    assert!(verify_stdout.contains("MIR-VERIFIER-FLOAT-001"));
    assert!(verify_stdout.contains("0/1 verified"));
    assert!(!verify_stdout.contains("No contracts to verify"));
    assert!(!verify_stdout.contains("canonical MIR ensures contract proven"));

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .output()
        .expect("failed to spawn f64 Flow MIR inspection");
    assert!(
        mir.status.success(),
        "f64 Flow MIR inspection failed:\n{}\n{}",
        String::from_utf8_lossy(&mir.stderr),
        String::from_utf8_lossy(&mir.stdout)
    );
    let mir_stdout = String::from_utf8_lossy(&mir.stdout);
    assert!(mir_stdout.contains("RecoverableBoundary"));
    assert!(mir_stdout.contains("Float { bits: 64 }"));
}

#[test]
fn canonical_mir_record_contract_matches_reference_native_and_verifier() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_contract.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record reference run");
    assert_eq!(reference.status.code(), Some(42));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-record-contract-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR record native build");
    assert!(
        build.status.success(),
        "canonical MIR record native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR record native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(42));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record verifier");
    assert!(
        verification.status.success(),
        "canonical MIR record verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    assert!(String::from_utf8_lossy(&verification.stdout)
        .contains("canonical MIR ensures contract proven"));
}

#[test]
fn canonical_mir_variant_contract_matches_reference_native_and_verifier() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_variant_contract.mimi");
    let reference = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR variant reference run");
    assert_eq!(reference.status.code(), Some(0));

    let binary = std::env::temp_dir().join(format!(
        "mimi-canonical-variant-contract-{}",
        std::process::id()
    ));
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&fixture)
        .arg("--mir")
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("failed to spawn canonical MIR variant native build");
    assert!(
        build.status.success(),
        "canonical MIR variant native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("failed to execute canonical MIR variant native binary");
    let _ = fs::remove_file(&binary);
    assert_eq!(native.status.code(), Some(0));

    let verification = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR variant verifier");
    assert!(
        verification.status.success(),
        "canonical MIR variant verification failed:\n{}\n{}",
        String::from_utf8_lossy(&verification.stderr),
        String::from_utf8_lossy(&verification.stdout)
    );
    let stdout = String::from_utf8_lossy(&verification.stdout);
    assert_eq!(
        stdout
            .matches("canonical MIR ensures contract proven")
            .count(),
        3
    );
}

#[test]
fn canonical_mir_record_contract_rejects_move_owned_projection() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_noncopy_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn rejected canonical MIR record verifier");
    assert!(
        !output.status.success(),
        "move-owned projection must fail closed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("canonical MIR verifier input rejected"));
    assert!(stderr.contains("outside the canonical Copy aggregate contract"));
    assert!(!stderr.contains("flow_ast"));
}

#[test]
fn canonical_mir_record_projection_preserves_checked_trap_class() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_record_trap_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR record trap verifier");
    assert!(
        !output.status.success(),
        "reachable record-field trap must fail"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("can reach trap 'E0802'"));
}

#[test]
fn canonical_mir_verifier_reports_ensures_counterexample() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_disproven_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(!output.status.success(), "disproven contract must fail CLI");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("canonical MIR ensures contract is disproven"));
    assert!(!stdout.contains("flow_ast"));
}

#[test]
fn canonical_mir_verifier_reports_reachable_checked_arithmetic_trap() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_trap_contract.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(!output.status.success(), "reachable trap must fail CLI");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("can reach trap 'E0802'"));
}

#[test]
fn canonical_mir_verifier_rejects_unsupported_abi_without_fallback() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_verifier_f64_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .arg("--mir")
        .output()
        .expect("failed to spawn canonical MIR verifier");
    assert!(!output.status.success(), "unsupported ABI must fail closed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("canonical MIR verifier input rejected"));
    assert!(stderr.contains("outside the canonical scalar verifier contract"));
    assert!(!stderr.contains("flow_ast"));
}

#[test]
fn canonical_default_generic_record_f64_projection_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_projection_f64.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Record<f64> default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Record<f64> default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_list_projection_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_projection.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Record<List<i32>> default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Record<List<i32>> default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_list_residual_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_projection_residual.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List residual default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List residual default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_list_list_residual_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_projection_list_residual.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List/List residual default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List/List residual default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_list_two_list_residual_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_projection_two_list_residual.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic two-List residual default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic two-List residual default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_list_string_residual_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_string_residual.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List/String residual default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List/String residual default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_list_two_string_residual_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_two_string_residual.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List/two-String residual default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic List/two-String residual default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_bool_list_two_string_residual_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_bool_list_two_string_residual.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Bool List/two-String residual default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Bool List/two-String residual default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_set_residual_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_set_residual.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Set residual default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Set residual default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_owned_set_scalar_family_routes_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_set_scalar_family.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Set scalar-family default run");
    assert_eq!(run.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&run.stderr).is_empty());

    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&fixture)
        .output()
        .expect("failed to spawn generic Set scalar-family default verify");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(!stdout.contains("flow_ast"), "{stdout}");
}

#[test]
fn canonical_default_generic_record_list_projection_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_projection_list_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic nested List default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir") && stderr_lower.contains("generic record"),
        "{stderr}"
    );
    assert!(!stderr.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_default_generic_record_list_residual_projection_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_projection_nested_list_residual_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic nested List residual default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir")
            && (stderr_lower.contains("generic record") || stderr_lower.contains("nested")),
        "{stderr}"
    );
    assert!(!stderr_lower.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_default_generic_record_three_list_residual_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_projection_three_list_residual_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic three-List residual default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir")
            && (stderr_lower.contains("generic record") || stderr_lower.contains("residual")),
        "{stderr}"
    );
    assert!(!stderr_lower.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_default_generic_record_list_string_string_residual_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_list_string_string_residual_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic List/String residual default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir")
            && (stderr_lower.contains("generic record") || stderr_lower.contains("residual")),
        "{stderr}"
    );
    assert!(!stderr_lower.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_default_generic_record_two_list_string_residual_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_two_list_string_residual_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic two-List/String residual default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir")
            && (stderr_lower.contains("generic record") || stderr_lower.contains("residual")),
        "{stderr}"
    );
    assert!(!stderr_lower.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_default_generic_record_owned_bool_nested_residual_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_bool_nested_residual_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic nested Bool List default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir")
            && (stderr_lower.contains("generic record") || stderr_lower.contains("nested")),
        "{stderr}"
    );
    assert!(!stderr_lower.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_default_generic_record_owned_set_string_residual_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_set_string_residual_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic Set/String residual default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir")
            && (stderr_lower.contains("generic record") || stderr_lower.contains("residual")),
        "{stderr}"
    );
    assert!(!stderr_lower.contains("legacy"), "{stderr}");
}

#[test]
fn canonical_default_generic_record_owned_set_float_rejects_before_legacy() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_generic_record_owned_set_float_rejected.mimi");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("failed to spawn rejected generic Set<f64> default run");
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();
    assert!(
        stderr_lower.contains("canonical mir")
            && (stderr_lower.contains("generic record")
                || stderr_lower.contains("copy scalar")
                || stderr_lower.contains("set")),
        "{stderr}"
    );
    assert!(!stderr_lower.contains("legacy"), "{stderr}");
}

#[test]
fn real_world_cli_suite() {
    let root = project_root().join("tests").join("real_world");
    let mut sources: Vec<PathBuf> = fs::read_dir(&root)
        .expect("read tests/real_world")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "mimi"))
        .collect();

    let consumer = root.join("projects").join("consumer").join("main.mimi");
    if consumer.exists() {
        sources.push(consumer);
    }

    let mut failures = Vec::new();
    let mut known_gap_failures = Vec::new();

    for src in &sources {
        let name = src.file_name().unwrap().to_string_lossy();
        eprintln!("real_world_cli: checking {name}");

        // Prefer stdout-aware run for dual-backend match (esp. flow_* MCDD).
        let interp_out = run_mimi_run_out(src);
        let requires_codegen = !INTERPRETER_ONLY.contains(&name.as_ref());
        let codegen = if requires_codegen && can_link() {
            Some(run_mimi_build_and_exec(src))
        } else {
            if requires_codegen {
                eprintln!("SKIP build for {name}: cc not available");
            } else {
                eprintln!("SKIP build for {name}: interpreter-only fixture");
            }
            None
        };

        let mut details = String::new();
        if let Err(e) = &interp_out {
            details.push_str(&format!("[interp] {e}\n"));
        }
        if let Some(Err(e)) = &codegen {
            details.push_str(&format!("[codegen] {e}\n"));
        }
        // TC-C5 / L1: require matching stdout for all dual successes, not only
        // flow_* programs. Known gaps still route to known_gap_failures.
        if let (Ok(i), Some(Ok(c))) = (&interp_out, &codegen) {
            let i_trim = i.trim_end();
            let c_trim = c.trim_end();
            if i_trim != c_trim {
                details.push_str(&format!(
                    "[L1 dual-backend mismatch]\ninterp:\n{i_trim}\ncodegen:\n{c_trim}\n"
                ));
            }
        }
        if !details.is_empty() {
            if is_known_gap(src) {
                known_gap_failures.push((name.to_string(), details));
            } else {
                failures.push((name.to_string(), details));
            }
        }
    }

    for (name, details) in &known_gap_failures {
        eprintln!("KNOWN GAP (not failing the suite): {name}\n{details}");
    }

    if !failures.is_empty() {
        let mut msg = format!("{} real-world CLI test(s) failed:\n", failures.len());
        for (name, details) in &failures {
            msg.push_str(&format!("\n=== {name} ===\n{details}"));
        }
        panic!("{msg}");
    }
}
