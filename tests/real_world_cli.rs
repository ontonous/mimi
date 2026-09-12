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
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

#[cfg(unix)]
use std::ffi::OsString;

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
    mimi::core::mir::CanonicalMirRouteReceipt::parse_manifest(&text)
        .unwrap_or_else(|error| panic!("invalid route receipt manifest: {error}"))
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

fn temp_build_dir_from_linker_stderr(stderr: &[u8]) -> PathBuf {
    let text = String::from_utf8_lossy(stderr);
    let token = text
        .split_whitespace()
        .find(|token| token.contains("mimi-build-"))
        .unwrap_or_else(|| panic!("linker stderr omitted staging path:\n{text}"));
    let marker = token
        .find("mimi-build-")
        .expect("staging path marker must be present");
    let separator = token[marker..]
        .find(|character| character == '/' || character == '\\')
        .expect("staging path must include an artifact separator");
    PathBuf::from(&token[..marker + separator])
}

fn staging_dirs_for_output_stem(stem: &str) -> Vec<PathBuf> {
    let suffix = format!("-{stem}");
    fs::read_dir(std::env::temp_dir())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with("mimi-build-")
                        && name.to_string_lossy().ends_with(&suffix)
                })
        })
        .collect()
}

#[cfg(unix)]
fn native_runtime_cache_path() -> PathBuf {
    let runtime_rs = project_root().join("src/runtime/standalone.rs");
    let runtime_dir = runtime_rs.parent().expect("runtime source parent");
    let mut files = fs::read_dir(runtime_dir)
        .expect("read runtime source directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect::<Vec<_>>();
    files.push(runtime_rs);
    files.sort();

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mimi-native-runtime-v1\0");
    if std::env::var_os("MIMI_ASAN").is_some() {
        hasher.update(b"asan\0");
    }
    for path in files {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(&fs::read(path).expect("read runtime source"));
    }
    let key = hasher.finalize().to_hex();
    std::env::temp_dir()
        .join("mimi_runtime_build_cache")
        .join(format!("libmimi_runtime_{key}.a"))
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
fn canonical_scalar_ffi_cli_generic_runtime_failures_match_default_and_mir() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_runtime_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI runtime failure directory");
    let c_path = dir.join("present.c");
    let library = dir.join("present.so");
    fs::write(
        &c_path,
        "#include <stdint.h>\nint64_t mir_ffi_present_only(int64_t value) { return value + 1; }\n",
    )
    .expect("write scalar FFI runtime failure C fixture");
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(&c_path)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("compile scalar FFI runtime failure C fixture");
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );

    let source = dir.join("missing.mimi");
    fs::write(
        &source,
        "extern \"C\" { func mir_ffi_absent_symbol(value: i64) -> i64; }\nfunc main() -> i64 { mir_ffi_absent_symbol(7 as i64) }\n",
    )
    .expect("write scalar FFI runtime failure source");

    let run = |explicit_mir: bool, library_path: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("run");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .env("MIMI_FFI_LIB", library_path)
            .output()
            .unwrap_or_else(|error| panic!("runtime failure run {explicit_mir}: {error}"))
    };

    let missing_library = dir.join("missing.so");
    let missing_library_runs = [run(false, &missing_library), run(true, &missing_library)];
    assert_eq!(
        missing_library_runs[0].stderr, missing_library_runs[1].stderr,
        "default and --mir missing-library diagnostics must match"
    );
    for output in &missing_library_runs {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("E0800"), "{stderr}");
        assert!(stderr.contains("failed to load"), "{stderr}");
        assert!(!stderr.contains("E0802"), "{stderr}");
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }

    let missing_symbol_runs = [run(false, &library), run(true, &library)];
    assert_eq!(
        missing_symbol_runs[0].stderr, missing_symbol_runs[1].stderr,
        "default and --mir missing-symbol diagnostics must match"
    );
    for output in &missing_symbol_runs {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("E0800"), "{stderr}");
        assert!(
            stderr.contains("failed to find canonical MIR FFI symbol"),
            "{stderr}"
        );
        assert!(!stderr.contains("E0802"), "{stderr}");
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_verify_is_checker_only_before_native_link_failure() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_link_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI link failure directory");
    let source = dir.join("unlinked.mimi");
    fs::write(
        &source,
        "extern \"C\" { func mir_ffi_unlinked_symbol(value: i64) -> i64; }\nfunc main() -> i64 { mir_ffi_unlinked_symbol(7 as i64) }\n",
    )
    .expect("write scalar FFI link failure source");

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("unlinked verify {explicit_mir}: {error}"))
    };
    let verifies = [verify(false), verify(true)];
    for output in &verifies {
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("No contracts to verify"), "{stdout}");
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert_eq!(verifies[0].stdout, verifies[1].stdout);
    assert_eq!(verifies[0].stderr, verifies[1].stderr);

    let emit_ir = |explicit_mir: bool, binary: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg("--verify-ffi")
            .arg("--emit-ir")
            .arg(&source)
            .arg("-o")
            .arg(binary)
            .output()
            .unwrap_or_else(|error| panic!("unlinked emit-ir build {explicit_mir}: {error}"))
    };
    let default_ir_binary = dir.join("unlinked-default.ll");
    let mir_ir_binary = dir.join("unlinked-mir.ll");
    let emitted = [
        emit_ir(false, &default_ir_binary),
        emit_ir(true, &mir_ir_binary),
    ];
    for output in &emitted {
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("declare i64 @mir_ffi_unlinked_symbol(i64)"),
            "{stdout}"
        );
        assert!(!stdout.contains("canonical route disposition: legacy"));
    }
    assert_eq!(emitted[0].stdout, emitted[1].stdout);
    assert!(!default_ir_binary.exists());
    assert!(!mir_ir_binary.exists());

    let build = |explicit_mir: bool, binary: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg("--verify-ffi")
            .arg(&source)
            .arg("-o")
            .arg(binary)
            .output()
            .unwrap_or_else(|error| panic!("unlinked build {explicit_mir}: {error}"))
    };
    let default_binary = dir.join("unlinked-default");
    let mir_binary = dir.join("unlinked-mir");
    let builds = [build(false, &default_binary), build(true, &mir_binary)];
    let normalize_linker_stderr = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter(|line| !line.contains("mimi-build-"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        normalize_linker_stderr(&builds[0].stderr),
        normalize_linker_stderr(&builds[1].stderr),
        "default and --mir linker diagnostics must match after temporary path normalization"
    );
    for output in &builds {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("undefined symbol: mir_ffi_unlinked_symbol"),
            "{stderr}"
        );
        assert!(!stderr.contains("FFI contract verification failed"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert!(!default_binary.exists());
    assert!(!mir_binary.exists());
    for output in &builds {
        let staging_dir = temp_build_dir_from_linker_stderr(&output.stderr);
        assert!(
            !staging_dir.exists(),
            "failed linker build left staging directory {staging_dir:?}"
        );
    }

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_multiple_linker_symbols_preserve_failure_order() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_multi_link_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI multi-link failure directory");
    let source = dir.join("multi-unlinked.mimi");
    fs::write(
        &source,
        "extern \"C\" { func mir_ffi_unlinked_alpha(value: i64) -> i64; func mir_ffi_unlinked_beta(value: i64) -> i64; }\nfunc main() -> i64 { let alpha = mir_ffi_unlinked_alpha(7 as i64); let beta = mir_ffi_unlinked_beta(8 as i64); alpha + beta }\n",
    )
    .expect("write scalar FFI multi-link failure source");

    let build = |explicit_mir: bool, binary: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg("--verify-ffi")
            .arg(&source)
            .arg("-o")
            .arg(binary)
            .output()
            .unwrap_or_else(|error| panic!("multi-symbol build {explicit_mir}: {error}"))
    };
    let default_binary = dir.join("multi-default");
    let mir_binary = dir.join("multi-mir");
    let builds = [build(false, &default_binary), build(true, &mir_binary)];
    let normalize_linker_stderr = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter(|line| !line.contains("mimi-build-"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        normalize_linker_stderr(&builds[0].stderr),
        normalize_linker_stderr(&builds[1].stderr),
        "default and --mir multi-symbol linker diagnostics must match after path normalization"
    );
    for output in &builds {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.matches("undefined symbol:").count(), 2, "{stderr}");
        let alpha = stderr
            .find("undefined symbol: mir_ffi_unlinked_alpha")
            .expect("alpha linker diagnostic");
        let beta = stderr
            .find("undefined symbol: mir_ffi_unlinked_beta")
            .expect("beta linker diagnostic");
        assert!(alpha < beta, "{stderr}");
        assert!(!stderr.contains("FFI contract verification failed"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert!(!default_binary.exists());
    assert!(!mir_binary.exists());

    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn canonical_scalar_ffi_cli_runtime_failure_cleans_staging_directory() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_runtime_compile_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI runtime compile failure directory");
    let source = dir.join("runtime-failure.mimi");
    fs::write(&source, "func main() -> i64 { 0 }\n")
        .expect("write scalar FFI runtime compile failure source");

    let fake_bin = dir.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("create fake rustc directory");
    let args_capture = dir.join("rustc-args.txt");
    let fake_rustc = fake_bin.join("rustc");
    fs::write(
        &fake_rustc,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$MIMI_FAKE_RUSTC_ARGS\"\nexit 23\n",
    )
    .expect("write failing rustc shim");
    let mut permissions = fs::metadata(&fake_rustc)
        .expect("stat failing rustc shim")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_rustc, permissions).expect("make failing rustc shim executable");

    let binary = dir.join("runtime-failure.so");
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg("--shared")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .env("PATH", &fake_bin)
        .env("MIMI_FAKE_RUSTC_ARGS", &args_capture)
        .output()
        .expect("spawn scalar FFI runtime compile failure build");
    assert!(!build.status.success());
    assert!(build.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("Rust runtime compilation failed"),
        "runtime failure lost its stable diagnostic: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(!binary.exists(), "runtime failure left an output binary");

    let rustc_args = fs::read_to_string(&args_capture).expect("failing rustc shim captured args");
    let runtime_output = rustc_args
        .lines()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|pair| (pair[0] == "-o").then_some(pair[1]))
        .expect("runtime rustc invocation must include an output path");
    let staging_dir = Path::new(runtime_output)
        .parent()
        .expect("runtime output path must have a staging parent");
    assert!(
        !staging_dir.exists(),
        "runtime compiler failure left staging directory {staging_dir:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_parallel_link_failures_keep_staging_isolated() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_parallel_link_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI parallel link failure directory");
    let source = dir.join("parallel-unlinked.mimi");
    fs::write(
        &source,
        "extern \"C\" { func mir_ffi_parallel_unlinked(value: i64) -> i64; }\nfunc main() -> i64 { mir_ffi_parallel_unlinked(1 as i64) }\n",
    )
    .expect("write scalar FFI parallel link failure source");
    let default_binary = dir.join("one").join("same-stem");
    let mir_binary = dir.join("two").join("same-stem");
    fs::create_dir_all(default_binary.parent().expect("default output parent"))
        .expect("create default output directory");
    fs::create_dir_all(mir_binary.parent().expect("MIR output parent"))
        .expect("create MIR output directory");

    let mut default_command = Command::new(mimi_bin());
    default_command
        .current_dir(project_root())
        .args(["build", "--verify-ffi"])
        .arg(&source)
        .arg("-o")
        .arg(&default_binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let default_child = default_command
        .spawn()
        .expect("spawn parallel default linker failure build");

    let mut mir_command = Command::new(mimi_bin());
    mir_command
        .current_dir(project_root())
        .args(["build", "--mir", "--verify-ffi"])
        .arg(&source)
        .arg("-o")
        .arg(&mir_binary)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mir_child = mir_command
        .spawn()
        .expect("spawn parallel MIR linker failure build");

    let default_build = default_child
        .wait_with_output()
        .expect("wait for parallel default linker failure build");
    let mir_build = mir_child
        .wait_with_output()
        .expect("wait for parallel MIR linker failure build");
    for output in [&default_build, &mir_build] {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("undefined symbol: mir_ffi_parallel_unlinked"),
            "parallel linker failure lost its symbol diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let default_staging = temp_build_dir_from_linker_stderr(&default_build.stderr);
    let mir_staging = temp_build_dir_from_linker_stderr(&mir_build.stderr);
    assert_ne!(
        default_staging, mir_staging,
        "parallel builds with the same output stem must use isolated staging directories"
    );
    assert!(
        !default_staging.exists(),
        "default staging directory remained"
    );
    assert!(!mir_staging.exists(), "MIR staging directory remained");
    assert!(
        !default_binary.exists(),
        "parallel default build left an output binary"
    );
    assert!(
        !mir_binary.exists(),
        "parallel MIR build left an output binary"
    );

    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn canonical_scalar_ffi_cli_reuses_runtime_cache_identity_across_default_and_mir() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_runtime_cache_reuse_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI runtime cache reuse directory");
    let source = dir.join("cache-reuse.mimi");
    fs::write(&source, "func main() -> i64 { 0 }\n")
        .expect("write scalar FFI runtime cache reuse source");
    let default_binary = dir.join("default");
    let mir_binary = dir.join("mir");
    let build = |explicit_mir: bool, binary: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .arg("-o")
            .arg(binary)
            .output()
            .unwrap_or_else(|error| panic!("runtime cache reuse build {explicit_mir}: {error}"))
    };

    let cache_path = native_runtime_cache_path();
    let default_build = build(false, &default_binary);
    assert!(
        default_build.status.success(),
        "default cache-seeding build failed: {}",
        String::from_utf8_lossy(&default_build.stderr)
    );
    assert!(
        cache_path.is_file(),
        "default build did not publish runtime cache"
    );
    let first_bytes = fs::read(&cache_path).expect("read seeded runtime cache");
    let first_metadata = fs::metadata(&cache_path).expect("stat seeded runtime cache");
    let stale_temp = cache_path
        .parent()
        .expect("runtime cache parent")
        .join(format!(
            "{}.tmp-stale-{}",
            cache_path
                .file_stem()
                .expect("runtime cache stem")
                .to_string_lossy(),
            std::process::id()
        ));
    fs::write(&stale_temp, b"stale runtime cache temporary")
        .expect("write stale runtime cache temporary");

    let mir_build = build(true, &mir_binary);
    assert!(
        mir_build.status.success(),
        "MIR cache-reuse build failed: {}",
        String::from_utf8_lossy(&mir_build.stderr)
    );
    assert!(
        !stale_temp.exists(),
        "cache hit did not prune stale temporary"
    );
    assert_eq!(
        fs::read(&cache_path).expect("read reused runtime cache"),
        first_bytes
    );
    let second_metadata = fs::metadata(&cache_path).expect("stat reused runtime cache");
    assert_eq!(first_metadata.dev(), second_metadata.dev());
    assert_eq!(first_metadata.ino(), second_metadata.ino());
    assert!(default_binary.is_file());
    assert!(mir_binary.is_file());

    fs::remove_file(&default_binary).ok();
    fs::remove_file(&mir_binary).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_successful_builds_clean_staging_directories() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_success_staging_cli_{}_{}",
        std::process::id(),
        nonce
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI success staging directory");
    let source = dir.join("success-staging.mimi");
    fs::write(&source, "func main() -> i64 { 0 }\n")
        .expect("write scalar FFI success staging source");

    let build = |explicit_mir: bool, stem: &str| {
        let binary = dir.join(stem);
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        let output = command
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap_or_else(|error| panic!("successful staging build {explicit_mir}: {error}"));
        (output, binary)
    };

    for (explicit_mir, stem) in [
        (false, format!("success-default-{nonce}")),
        (true, format!("success-mir-{nonce}")),
    ] {
        let (output, binary) = build(explicit_mir, &stem);
        assert!(
            output.status.success(),
            "successful build failed (mir={explicit_mir}): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(binary.is_file(), "successful build did not create output");
        assert!(
            staging_dirs_for_output_stem(&stem).is_empty(),
            "successful build left staging directories for {stem}"
        );
        fs::remove_file(&binary).ok();
    }

    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn canonical_scalar_ffi_cli_non_utf8_output_path_cleans_staging() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_non_utf8_output_cli_{}_{}",
        std::process::id(),
        nonce
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI non-UTF-8 output directory");
    let source = dir.join("non-utf8-output.mimi");
    fs::write(&source, "func main() -> i64 { 0 }\n")
        .expect("write scalar FFI non-UTF-8 output source");
    let non_utf8_parent = dir.join(OsString::from_vec(b"non-utf8-parent-\xff".to_vec()));
    fs::create_dir_all(&non_utf8_parent).expect("create non-UTF-8 output parent");
    let stem = format!("non-utf8-output-{nonce}");
    let binary = non_utf8_parent.join(&stem);

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .args(["build", "--mir"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("spawn scalar FFI non-UTF-8 output build");
    assert!(!build.status.success());
    assert!(build.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("output path is not valid UTF-8"),
        "non-UTF-8 output lost its path diagnostic: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        !binary.exists(),
        "non-UTF-8 output failure created a binary"
    );
    assert!(
        staging_dirs_for_output_stem(&stem).is_empty(),
        "non-UTF-8 output failure left staging directories for {stem}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_missing_output_parent_cleans_staging() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_missing_output_parent_cli_{}_{}",
        std::process::id(),
        nonce
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI missing output parent directory");
    let source = dir.join("missing-parent.mimi");
    fs::write(&source, "func main() -> i64 { 0 }\n")
        .expect("write scalar FFI missing output parent source");
    let stem = format!("missing-parent-output-{nonce}");
    let binary = dir.join("absent-parent").join(&stem);

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .args(["build", "--mir"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("spawn scalar FFI missing output parent build");
    assert!(!build.status.success());
    assert!(build.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("cannot open output file"),
        "missing output parent lost linker path diagnostic: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        !binary.exists(),
        "missing output parent failure created a binary"
    );
    assert!(
        staging_dirs_for_output_stem(&stem).is_empty(),
        "missing output parent failure left staging directories for {stem}"
    );
    assert!(!binary.parent().expect("missing output parent").exists());

    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn canonical_scalar_ffi_cli_staging_creation_failure_preserves_temp_file() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_staging_creation_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI staging creation failure directory");
    let source = dir.join("staging-failure.mimi");
    fs::write(&source, "func main() -> i64 { 0 }\n")
        .expect("write scalar FFI staging creation failure source");
    let temp_file = dir.join("tmp-file");
    fs::write(&temp_file, b"keep this user file").expect("write TMPDIR file");
    let binary = dir.join("staging-failure");

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .args(["build", "--mir"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .env("TMPDIR", &temp_file)
        .output()
        .expect("spawn scalar FFI staging creation failure build");
    assert!(!build.status.success());
    assert!(build.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("failed to create temp build dir"),
        "staging creation failure lost its diagnostic: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        !binary.exists(),
        "staging creation failure created a binary"
    );
    assert_eq!(
        fs::read(&temp_file).expect("read preserved TMPDIR file"),
        b"keep this user file"
    );

    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn canonical_scalar_ffi_cli_fallback_stem_cleanup_preserves_tmp_root() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_fallback_stem_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI fallback stem directory");
    let source = dir.join("fallback-stem.mimi");
    fs::write(&source, "func main() -> i64 { 0 }\n")
        .expect("write scalar FFI fallback stem source");
    let temp_root = dir.join("isolated-tmp");
    fs::create_dir_all(&temp_root).expect("create isolated TMPDIR");
    let sentinel = temp_root.join("keep.txt");
    fs::write(&sentinel, b"preserve this root file").expect("write isolated TMPDIR sentinel");

    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .args(["build", "--mir", "--shared"])
        .arg(&source)
        .arg("-o")
        .arg("/")
        .env("TMPDIR", &temp_root)
        .output()
        .expect("spawn scalar FFI fallback stem build");
    assert!(!build.status.success());
    assert!(build.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("cannot open output file /"),
        "fallback stem failure lost linker path diagnostic: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert_eq!(
        fs::read(&sentinel).expect("read preserved isolated TMPDIR sentinel"),
        b"preserve this root file"
    );
    let staging = fs::read_dir(&temp_root)
        .expect("read isolated TMPDIR")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("mimi-build-")
        })
        .collect::<Vec<_>>();
    assert!(
        staging.is_empty(),
        "fallback stem failure left staging entries: {staging:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_contract_failure_precedes_unresolved_linker_symbol() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_contract_before_link_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI contract/link failure directory");
    let source = dir.join("contract-before-link.mimi");
    fs::write(
        &source,
        "extern \"C\" { func mir_ffi_disproved(value: i64) -> i64 requires: value >= 0; func mir_ffi_unlinked_other(value: i64) -> i64; }\nfunc main() -> i64 { let bad = mir_ffi_disproved(-1 as i64); mir_ffi_unlinked_other(bad) }\n",
    )
    .expect("write scalar FFI contract/link failure source");

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("contract-before-link verify {explicit_mir}: {error}"))
    };
    let verifies = [verify(false), verify(true)];
    let stable_verify_summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&output.stderr).lines())
            .filter(|line| line.contains("verified") || line.contains("canonical MIR extern"))
            .map(|line| line.split(" in ").next().unwrap_or(line).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stable_verify_summary(&verifies[0]),
        stable_verify_summary(&verifies[1])
    );
    for output in &verifies {
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");
        assert!(combined.contains("0/1 verified"), "{combined}");
        assert!(
            combined.contains("canonical MIR extern requires contract disproven"),
            "{combined}"
        );
        assert!(!combined.contains("undefined symbol: mir_ffi_unlinked_other"));
        assert!(!combined.contains("canonical route disposition: legacy"));
    }

    let build = |explicit_mir: bool, binary: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg("--verify-ffi")
            .arg(&source)
            .arg("-o")
            .arg(binary)
            .output()
            .unwrap_or_else(|error| panic!("contract-before-link build {explicit_mir}: {error}"))
    };
    let default_binary = dir.join("contract-before-link-default");
    let mir_binary = dir.join("contract-before-link-mir");
    let builds = [build(false, &default_binary), build(true, &mir_binary)];
    assert_eq!(builds[0].stderr, builds[1].stderr);
    for output in &builds {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("FFI contract verification failed"),
            "{stderr}"
        );
        assert!(
            stderr.contains("canonical MIR extern requires contract disproven"),
            "{stderr}"
        );
        assert!(!stderr.contains("undefined symbol: mir_ffi_unlinked_other"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert!(!default_binary.exists());
    assert!(!mir_binary.exists());

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_multiple_contract_failures_precede_linker_diagnostics() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_multi_contract_link_failure_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scalar FFI multi-contract failure directory");
    let source = dir.join("multi-contract-before-link.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func mir_ffi_first_disproved(value: i64) -> i64 requires: value >= 0;\n    func mir_ffi_second_disproved(value: i64) -> i64 requires: value >= 0;\n    func mir_ffi_unlinked_third(value: i64) -> i64;\n}\nfunc main() -> i64 {\n    mir_ffi_first_disproved(-1 as i64)\n    mir_ffi_second_disproved(-2 as i64)\n    mir_ffi_unlinked_third(0 as i64)\n}\n",
    )
    .expect("write scalar FFI multi-contract failure source");

    let receipt = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn multi-contract receipt inspection");
    assert!(receipt.status.success());
    assert_eq!(
        mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(&String::from_utf8_lossy(
            &receipt.stdout
        ),)
        .expect("multi-contract receipt manifest"),
        checked_route_receipt(&source)
    );
    let receipt_repeat = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn repeated multi-contract receipt inspection");
    assert_eq!(receipt.stdout, receipt_repeat.stdout);
    let receipt_reordered = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--receipt")
        .arg("--all")
        .output()
        .expect("spawn reordered multi-contract receipt inspection");
    assert_eq!(receipt.stdout, receipt_reordered.stdout);
    let mir_text = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .output()
        .expect("spawn multi-contract MIR inspection");
    assert!(mir_text.status.success());
    let mir_text = String::from_utf8_lossy(&mir_text.stdout);
    assert_eq!(mir_text.matches("    inst:call:").count(), 3, "{mir_text}");
    assert!(!mir_text.contains("canonical route disposition: legacy"));

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("multi-contract verify {explicit_mir}: {error}"))
    };
    let verifies = [verify(false), verify(true)];
    let stable_verify_summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&output.stderr).lines())
            .filter(|line| line.contains("verified") || line.contains("canonical MIR extern"))
            .map(|line| line.split(" in ").next().unwrap_or(line).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stable_verify_summary(&verifies[0]),
        stable_verify_summary(&verifies[1])
    );
    let default_verify_repeat = verify(false);
    let mir_verify_repeat = verify(true);
    assert_eq!(
        stable_verify_summary(&verifies[0]),
        stable_verify_summary(&default_verify_repeat)
    );
    assert_eq!(
        stable_verify_summary(&verifies[1]),
        stable_verify_summary(&mir_verify_repeat)
    );
    assert_eq!(verifies[0].stderr, default_verify_repeat.stderr);
    assert_eq!(verifies[1].stderr, mir_verify_repeat.stderr);
    for output in &verifies {
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");
        assert!(combined.contains("0/2 verified"), "{combined}");
        assert_eq!(
            combined
                .matches("canonical MIR extern requires contract disproven")
                .count(),
            2,
            "{combined}"
        );
        let first = combined
            .find("first_disproved(-1 as i64)")
            .expect("first contract diagnostic source");
        let second = combined
            .find("second_disproved(-2 as i64)")
            .expect("second contract diagnostic source");
        assert!(first < second, "{combined}");
        assert!(!combined.contains("mir_ffi_unlinked_third"));
        assert!(!combined.contains("canonical route disposition: legacy"));
    }

    let build = |explicit_mir: bool, binary: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg("--verify-ffi")
            .arg(&source)
            .arg("-o")
            .arg(binary)
            .output()
            .unwrap_or_else(|error| panic!("multi-contract build {explicit_mir}: {error}"))
    };
    let default_binary = dir.join("multi-contract-default");
    let mir_binary = dir.join("multi-contract-mir");
    let builds = [build(false, &default_binary), build(true, &mir_binary)];
    let default_build_repeat = build(false, &default_binary);
    let mir_build_repeat = build(true, &mir_binary);
    assert_eq!(builds[0].stderr, builds[1].stderr);
    assert_eq!(builds[0].stderr, default_build_repeat.stderr);
    assert_eq!(builds[1].stderr, mir_build_repeat.stderr);
    for output in &builds {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.matches("FFI violation").count(), 2, "{stderr}");
        assert!(
            stderr.contains("canonical MIR extern requires contract disproven"),
            "{stderr}"
        );
        assert!(!stderr.contains("undefined symbol: mir_ffi_unlinked_third"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert!(!default_binary.exists());
    assert!(!mir_binary.exists());

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
        for explicit_mir in [false, true] {
            let mut run = Command::new(mimi_bin());
            run.current_dir(project_root()).arg("run");
            if explicit_mir {
                run.arg("--mir");
            }
            let run = run
                .arg(&path)
                .env("MIMI_VERBOSE", "1")
                .env("MIMI_FFI_LIB", &library)
                .output()
                .unwrap();
            assert!(
                run.status.success(),
                "contracts={contracts} explicit_mir={explicit_mir}: {}",
                String::from_utf8_lossy(&run.stderr)
            );
            assert_eq!(
                run.stdout, b"-2147483647\n2147483647\n4294967296\ntrue\nfalse\n1\n42\n",
                "contracts={contracts} explicit_mir={explicit_mir}"
            );
            let run_stderr = String::from_utf8_lossy(&run.stderr);
            assert!(!run_stderr.contains("canonical route disposition: legacy"));
            assert!(!run_stderr.contains("FFI contract verification is disabled"));

            // The CLI does not accept an extra native library link argument.
            // Exercise its production emitter here; direct native linking against
            // this same C fixture is checked in canonical_scalar_ffi.rs.
            let mut build = Command::new(mimi_bin());
            build.current_dir(project_root()).arg("build");
            if explicit_mir {
                build.arg("--mir");
            }
            let build = build
                .arg("--emit-ir")
                .arg(&path)
                .env("MIMI_VERBOSE", "1")
                .output()
                .unwrap();
            assert!(
                build.status.success(),
                "contracts={contracts} explicit_mir={explicit_mir}: {}",
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
            assert!(!String::from_utf8_lossy(&build.stderr)
                .contains("canonical route disposition: legacy"));
            let mut verify = Command::new(mimi_bin());
            verify.current_dir(project_root()).arg("verify");
            if explicit_mir {
                verify.arg("--mir");
            }
            let verify = verify.arg(&path).env("MIMI_VERBOSE", "1").output().unwrap();
            assert!(
                verify.status.success(),
                "contracts={contracts} explicit_mir={explicit_mir}: {}",
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
    }
    fs::remove_dir_all(dir).ok();
}

#[test]
fn canonical_scalar_ffi_transparent_alias_default_cli_matches_explicit_mir() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_cli_alias_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let library = dir.join("ffi.so");
    fs::write(
        dir.join("ffi.c"),
        "#include <stdint.h>\nint64_t mir_ffi_cli_alias(double value) { return value == 7.0 ? 42 : -1; }\n",
    )
    .unwrap();
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(dir.join("ffi.c"))
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );
    let source = dir.join("alias.mimi");
    fs::write(
        &source,
        r#"
type Scalar = f64
type Real = Scalar
extern "C" { func mir_ffi_cli_alias(value: Real) -> i64; }
func main() -> i64 {
    println(mir_ffi_cli_alias(7 as i64))
    0
}
"#,
    )
    .unwrap();

    for explicit_mir in [false, true] {
        let mut run = Command::new(mimi_bin());
        run.current_dir(project_root()).arg("run");
        if explicit_mir {
            run.arg("--mir");
        }
        let run = run
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .env("MIMI_FFI_LIB", &library)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "explicit_mir={explicit_mir}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"42\n", "explicit_mir={explicit_mir}");
        assert!(
            !String::from_utf8_lossy(&run.stderr).contains("canonical route disposition: legacy"),
            "transparent aliases must not fall back to legacy"
        );

        let mut build = Command::new(mimi_bin());
        build.current_dir(project_root()).arg("build");
        if explicit_mir {
            build.arg("--mir");
        }
        let build = build.arg("--emit-ir").arg(&source).output().unwrap();
        assert!(
            build.status.success(),
            "explicit_mir={explicit_mir}: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        assert!(
            String::from_utf8_lossy(&build.stdout).contains("mir_ffi_cli_alias"),
            "emitted IR must retain the checker-owned alias FFI symbol"
        );

        let mut verify = Command::new(mimi_bin());
        verify.current_dir(project_root()).arg("verify");
        if explicit_mir {
            verify.arg("--mir");
        }
        let verify = verify
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .output()
            .unwrap();
        assert!(
            verify.status.success(),
            "explicit_mir={explicit_mir}: {}",
            String::from_utf8_lossy(&verify.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&verify.stderr)
                .contains("canonical route disposition: legacy"),
            "verify must use the canonical alias route"
        );
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
fn canonical_scalar_ffi_cli_combined_requires_ensures_runtime_verify_boundary() {
    if !can_link() {
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_combined_contract_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create combined-contract CLI fixture directory");
    let source = dir.join("combined.mimi");
    fs::write(
        &source,
        "extern \"C\" { func labs(x: i64) -> i64 requires: x >= 0 ensures: result == x; }\nfunc main() -> i64 { println(labs(7 as i64)); 0 }\n",
    )
    .expect("write combined-contract CLI source");

    let mut verify_outputs = Vec::new();
    for explicit_mir in [false, true] {
        let mut run = Command::new(mimi_bin());
        run.current_dir(project_root()).arg("run");
        if explicit_mir {
            run.arg("--mir");
        }
        let run = run
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("combined run {:?}: {error}", explicit_mir));
        assert!(
            run.status.success(),
            "combined run {:?} must succeed:\n{}\n{}",
            explicit_mir,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"7\n");
        assert!(run.stderr.is_empty());

        let binary = dir.join(if explicit_mir {
            "combined-mir"
        } else {
            "combined-default"
        });
        let mut build = Command::new(mimi_bin());
        build.current_dir(project_root()).arg("build");
        if explicit_mir {
            build.arg("--mir");
        }
        let build = build
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap_or_else(|error| panic!("combined build {:?}: {error}", explicit_mir));
        assert!(
            build.status.success(),
            "combined build {:?} must succeed:\n{}\n{}",
            explicit_mir,
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&build.stderr).contains("canonical route disposition: legacy")
        );
        let native = Command::new(&binary)
            .output()
            .unwrap_or_else(|error| panic!("combined binary {:?}: {error}", explicit_mir));
        assert!(
            native.status.success(),
            "combined binary {:?} failed",
            explicit_mir
        );
        assert_eq!(native.stdout, b"7\n");
        assert!(native.stderr.is_empty());

        let mut verify = Command::new(mimi_bin());
        verify.current_dir(project_root()).arg("verify");
        if explicit_mir {
            verify.arg("--mir");
        }
        let verify = verify
            .arg(&source)
            .output()
            .unwrap_or_else(|error| panic!("combined verify {:?}: {error}", explicit_mir));
        assert!(
            !verify.status.success(),
            "combined verify {:?} must reject the unconstrained external result",
            explicit_mir
        );
        let verify_stdout = String::from_utf8_lossy(&verify.stdout);
        let verify_stderr = String::from_utf8_lossy(&verify.stderr);
        assert!(
            verify_stdout.contains("0/1 verified") || verify_stderr.contains("0/1 verified"),
            "combined verify {:?}: stdout={verify_stdout} stderr={verify_stderr}",
            explicit_mir
        );
        assert!(
            verify_stderr.contains("canonical MIR extern ensures contract disproven"),
            "combined verify {:?}: {verify_stderr}",
            explicit_mir
        );
        assert!(!verify_stdout.contains("canonical route disposition: legacy"));
        assert!(!verify_stderr.contains("canonical route disposition: legacy"));
        verify_outputs.push(verify);

        let mut build_verify = Command::new(mimi_bin());
        build_verify
            .current_dir(project_root())
            .arg("build")
            .arg("--verify-ffi");
        if explicit_mir {
            build_verify.arg("--mir");
        }
        let build_verify = build_verify
            .arg("--emit-ir")
            .arg(&source)
            .output()
            .unwrap_or_else(|error| {
                panic!("combined build --verify-ffi {:?}: {error}", explicit_mir)
            });
        assert!(
            !build_verify.status.success(),
            "combined build --verify-ffi {:?} must reject the unconstrained external result",
            explicit_mir
        );
        let build_verify_stderr = String::from_utf8_lossy(&build_verify.stderr);
        assert!(
            build_verify_stderr.contains("FFI contract verification failed")
                && build_verify_stderr.contains("canonical MIR extern ensures contract disproven"),
            "combined build --verify-ffi {:?}: {build_verify_stderr}",
            explicit_mir
        );
        assert!(!build_verify_stderr.contains("canonical route disposition: legacy"));
        fs::remove_file(&binary).ok();
    }
    let semantic_summary = |stdout: &[u8]| {
        String::from_utf8_lossy(stdout)
            .lines()
            .filter_map(|line| {
                let (status, timing) = line.split_once(" in ")?;
                let constraints = timing.find(" (").map(|index| &timing[index..])?;
                Some(format!("{status}{constraints}"))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        semantic_summary(&verify_outputs[0].stdout),
        semantic_summary(&verify_outputs[1].stdout),
        "default and explicit MIR combined verifier semantic summaries must match"
    );
    assert_eq!(
        verify_outputs[0].stderr, verify_outputs[1].stderr,
        "default and explicit MIR combined verifier diagnostics must match"
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_multi_callsite_mixed_verdict_matches_receipt_and_mir() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_multi_callsite_mixed_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-callsite mixed CLI fixture directory");
    let source = dir.join("mixed.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func first(value: i64) -> i64 requires: value >= 0 ensures: true;\n    func second(value: i64) -> i64 requires: value >= 0 ensures: result == value;\n}\nfunc main() -> i64 {\n    first(7 as i64);\n    second(-8 as i64);\n    0\n}\n",
    )
    .expect("write multi-callsite mixed CLI source");

    let checked = checked_route_receipt(&source);
    let inspect = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn multi-callsite mixed receipt inspection");
    assert!(
        inspect.status.success(),
        "mixed receipt inspection failed:\n{}\n{}",
        String::from_utf8_lossy(&inspect.stdout),
        String::from_utf8_lossy(&inspect.stderr)
    );
    let manifest = parse_route_receipt_manifest(&inspect.stdout);
    assert_eq!(
        manifest.get("mir_digest"),
        Some(&checked.mir_digest),
        "CLI receipt MIR digest must match the checked route"
    );
    assert_eq!(
        manifest.get("ffi_digest"),
        Some(&checked.ffi_digest),
        "CLI receipt FFI digest must match the checked route"
    );
    assert!(
        !String::from_utf8_lossy(&inspect.stderr).contains("canonical route disposition: legacy")
    );

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .output()
            .expect("spawn multi-callsite mixed verifier")
    };
    let default_verify = verify(false);
    let mir_verify = verify(true);
    let semantic_summary = |output: &std::process::Output| {
        let mut lines = Vec::new();
        for bytes in [&output.stdout, &output.stderr] {
            lines.extend(String::from_utf8_lossy(bytes).lines().map(str::to_owned));
        }
        lines
            .into_iter()
            .filter(|line| line.contains("canonical MIR extern requires contract"))
            .map(|line| line.split(" (").next().unwrap_or(&line).to_owned())
            .collect::<Vec<_>>()
    };
    for (label, output) in [("default", &default_verify), ("mir", &mir_verify)] {
        assert!(
            !output.status.success(),
            "{label} mixed verifier must report the disproven second receipt"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("canonical MIR extern requires contract proven"),
            "{label} lost the first proven receipt: {stdout}"
        );
        assert!(
            stderr.contains("canonical MIR extern requires contract disproven"),
            "{label} lost the second disproven receipt: {stderr}"
        );
        assert!(
            stdout.contains("1/2 verified") || stderr.contains("1/2 verified"),
            "{label} lost the two-receipt cardinality summary: stdout={stdout} stderr={stderr}"
        );
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert_eq!(
        semantic_summary(&default_verify),
        semantic_summary(&mir_verify),
        "default and explicit MIR must expose the same mixed verdict order and summaries"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_build_verify_reports_only_failed_receipt() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_build_verify_multi_callsite_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create build-verify multi-callsite fixture directory");
    let source = dir.join("mixed.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func first(value: i64) -> i64 requires: value >= 0 ensures: true;\n    func second(value: i64) -> i64 requires: value >= 0 ensures: result == value;\n}\nfunc main() -> i64 {\n    first(7 as i64);\n    second(-8 as i64);\n    0\n}\n",
    )
    .expect("write build-verify multi-callsite source");

    let mut outputs = Vec::new();
    for explicit_mir in [false, true] {
        let binary = dir.join(if explicit_mir {
            "mixed-mir-output"
        } else {
            "mixed-default-output"
        });
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        let output = command
            .arg("--verify-ffi")
            .arg("--emit-ir")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("spawn multi-callsite build verifier");
        assert!(
            !output.status.success(),
            "build --verify-ffi {:?} must reject the second receipt",
            explicit_mir
        );
        assert!(
            output.stdout.is_empty(),
            "failed build --verify-ffi {:?} emitted an artifact on stdout",
            explicit_mir
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("FFI contract verification failed")
                && stderr.contains("canonical MIR extern requires contract disproven")
                && stderr.contains("second(-8 as i64)"),
            "build --verify-ffi {:?} lost the failed receipt diagnostic: {stderr}",
            explicit_mir
        );
        assert!(
            !stderr.contains("first(7 as i64)"),
            "build --verify-ffi {:?} reported the already-proven receipt as failed: {stderr}",
            explicit_mir
        );
        assert!(!stderr.contains("canonical route disposition: legacy"));
        assert!(
            !binary.exists(),
            "failed build --verify-ffi {:?} left an output binary",
            explicit_mir
        );
        outputs.push(output);
    }
    assert_eq!(
        outputs[0].stderr, outputs[1].stderr,
        "default and explicit MIR build verifier diagnostics must match"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_build_verify_counts_all_failed_receipts() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_build_verify_receipt_count_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create receipt-count CLI fixture directory");
    let source = dir.join("receipt-count.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func first(value: i64) -> i64 requires: value >= 0 ensures: true;\n    func second(value: i64) -> i64 requires: value >= 0 ensures: result == value;\n    func third(value: i64) -> i64 requires: value >= 0 ensures: true;\n}\nfunc main() -> i64 {\n    first(7 as i64);\n    second(-8 as i64);\n    third(-9 as i64);\n    0\n}\n",
    )
    .expect("write receipt-count CLI source");

    let checked = checked_route_receipt(&source);
    let inspect = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn receipt-count route inspection");
    assert!(inspect.status.success());
    let manifest = parse_route_receipt_manifest(&inspect.stdout);
    assert_eq!(manifest.get("mir_digest"), Some(&checked.mir_digest));
    assert_eq!(manifest.get("ffi_digest"), Some(&checked.ffi_digest));
    assert!(
        !String::from_utf8_lossy(&inspect.stderr).contains("canonical route disposition: legacy")
    );

    let mut outputs = Vec::new();
    for explicit_mir in [false, true] {
        let binary = dir.join(if explicit_mir {
            "receipt-count-mir-output"
        } else {
            "receipt-count-default-output"
        });
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        let output = command
            .arg("--verify-ffi")
            .arg("--emit-ir")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("spawn receipt-count build verifier");
        assert_eq!(
            output.status.code(),
            Some(1),
            "build --verify-ffi {:?} must use one failure exit code",
            explicit_mir
        );
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.matches("FFI violation").count(),
            2,
            "build --verify-ffi {:?} must report both failed receipts: {stderr}",
            explicit_mir
        );
        assert_eq!(
            stderr.matches("FFI contract verification failed").count(),
            1,
            "build --verify-ffi {:?} must emit one terminal failure: {stderr}",
            explicit_mir
        );
        assert_eq!(stderr.matches("second(-8 as i64)").count(), 1);
        assert_eq!(stderr.matches("third(-9 as i64)").count(), 1);
        assert!(!stderr.contains("first(7 as i64)"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
        assert!(!binary.exists());
        outputs.push(output);
    }
    assert_eq!(
        outputs[0].stderr, outputs[1].stderr,
        "default and explicit MIR must report the same receipt-count diagnostics"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_verify_orders_diagnostics_and_roundtrips_receipt() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_verify_receipt_order_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create verifier-order CLI fixture directory");
    let source = dir.join("verify-order.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func first(value: i64) -> i64 requires: value >= 0 ensures: true;\n    func second(value: i64) -> i64 requires: value >= 0 ensures: result == value;\n    func third(value: i64) -> i64 requires: value >= 0 ensures: true;\n}\nfunc main() -> i64 {\n    first(7 as i64);\n    second(-8 as i64);\n    third(-9 as i64);\n    0\n}\n",
    )
    .expect("write verifier-order CLI source");

    let checked = checked_route_receipt(&source);
    let inspect = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn verifier-order receipt inspection");
    assert!(inspect.status.success());
    let manifest_text = String::from_utf8_lossy(&inspect.stdout);
    let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(&manifest_text)
        .expect("verifier-order receipt manifest round-trip");
    assert_eq!(manifest_receipt, checked);
    assert_eq!(
        manifest_receipt.root_owners,
        vec![mimi::core::NodeId("function:main".into())]
    );
    assert!(
        !String::from_utf8_lossy(&inspect.stderr).contains("canonical route disposition: legacy")
    );

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .output()
            .expect("spawn verifier-order CLI verifier")
    };
    let default_verify = verify(false);
    let mir_verify = verify(true);
    for (label, output) in [("default", &default_verify), ("mir", &mir_verify)] {
        assert_eq!(output.status.code(), Some(1), "{label} verifier exit code");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("canonical MIR extern requires contract proven"));
        assert!(stdout.contains("1/3 verified"));
        assert_eq!(
            stderr
                .matches("canonical MIR extern requires contract disproven")
                .count(),
            2,
            "{label} must retain both disproven diagnostics: {stderr}"
        );
        let second = stderr
            .find("second(-8 as i64)")
            .expect("second diagnostic source");
        let third = stderr
            .find("third(-9 as i64)")
            .expect("third diagnostic source");
        assert!(
            second < third,
            "{label} diagnostics changed receipt source order"
        );
        assert_eq!(stderr.matches("second(-8 as i64)").count(), 1);
        assert_eq!(stderr.matches("third(-9 as i64)").count(), 1);
        assert!(!stderr.contains("first(7 as i64)"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    let stable_summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains("canonical MIR extern requires contract")
                    || line.contains("verified in")
            })
            .map(|line| {
                let semantic = line.split(" (").next().unwrap_or(line);
                semantic.split(" in ").next().unwrap_or(semantic).to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stable_summary(&default_verify),
        stable_summary(&mir_verify),
        "default and explicit MIR verifier summaries must match without duration"
    );
    assert_eq!(
        default_verify.stderr, mir_verify.stderr,
        "default and explicit MIR verifier diagnostics must match byte-for-byte"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_verify_repeats_and_rejects_reordered_manifest() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_verify_repeat_manifest_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create verifier-repeat CLI fixture directory");
    let source = dir.join("verify-repeat.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func first(value: i64) -> i64 requires: value >= 0 ensures: true;\n    func second(value: i64) -> i64 requires: value >= 0 ensures: true;\n}\nfunc main() -> i64 {\n    first(7 as i64);\n    second(-8 as i64);\n    0\n}\n",
    )
    .expect("write verifier-repeat CLI source");

    let inspect = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn verifier-repeat receipt inspection");
    assert!(inspect.status.success());
    let manifest_text = String::from_utf8_lossy(&inspect.stdout);
    let manifest_lines = manifest_text.lines().collect::<Vec<_>>();
    assert_eq!(
        manifest_lines.len(),
        10,
        "receipt manifest must be complete"
    );
    let mut reordered = manifest_lines[1..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect::<Vec<_>>();
    reordered.reverse();
    let reordered = std::iter::once(manifest_lines[0].to_owned())
        .chain(reordered)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(&reordered),
        Err("invalid MIR route manifest: field 'root_owners' at row 0, expected 'schema'".into())
    );

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .output()
            .expect("spawn verifier-repeat CLI verifier")
    };
    let runs = [verify(false), verify(false), verify(true), verify(true)];
    for (index, output) in runs.iter().enumerate() {
        assert_eq!(
            output.status.code(),
            Some(1),
            "verifier run {index} exit code"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("canonical MIR extern requires contract disproven"),
            "verifier run {index} must retain the failed receipt diagnostic"
        );
    }
    assert_eq!(
        runs[0].stderr, runs[1].stderr,
        "repeated default verifier runs must retain byte-identical diagnostics"
    );
    assert_eq!(
        runs[2].stderr, runs[3].stderr,
        "repeated explicit MIR verifier runs must retain byte-identical diagnostics"
    );
    assert_eq!(
        runs[0].stderr, runs[2].stderr,
        "default and explicit MIR verifier diagnostics must remain identical"
    );
    let stable_summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains("canonical MIR extern requires contract")
                    || line.contains("verified in")
            })
            .map(|line| {
                let semantic = line.split(" (").next().unwrap_or(line);
                semantic.split(" in ").next().unwrap_or(semantic).to_owned()
            })
            .collect::<Vec<_>>()
    };
    for (index, output) in runs.iter().enumerate().skip(1) {
        assert_eq!(
            stable_summary(&runs[0]),
            stable_summary(output),
            "verifier run {index} changed the semantic summary"
        );
    }

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_import_graph_mixed_verdict_preserves_manifest_and_source_order() {
    let dir = project_root().join("target").join(format!(
        "mimi-ffi-import-verify-order-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create imported verifier-order directory");
    fs::write(
        dir.join("leaf.mimi"),
        "pub type ScalarInt = i64\nextern \"C\" {\n    func leaf_ok(x: i64) -> ScalarInt requires: x >= 0 ensures: true;\n    func leaf_bad(x: i64) -> ScalarInt requires: x >= 0 ensures: true;\n}\npub func imported_ok(x: i64) -> ScalarInt {\n    requires: x >= 0\n    leaf_ok(x)\n}\npub func imported_bad(x: i64) -> ScalarInt {\n    leaf_bad(x)\n}\n",
    )
    .expect("write imported verifier-order leaf");
    fs::write(
        dir.join("mid.mimi"),
        "use leaf;\npub func mid_ok(x: i64) -> i64 {\n    requires: x >= 0\n    imported_ok(x)\n}\npub func mid_bad(x: i64) -> i64 {\n    imported_bad(x)\n}\n",
    )
    .expect("write imported verifier-order middle");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use mid;\nfunc main() -> i32 {\n    mid_ok(7 as i64);\n    mid_bad(-8 as i64);\n    0\n}\n",
    )
    .expect("write imported verifier-order entry");

    let checked = checked_route_receipt(&main);
    assert_eq!(
        checked.root_owners,
        vec![
            mimi::core::NodeId("function:imported_bad".into()),
            mimi::core::NodeId("function:imported_ok".into()),
            mimi::core::NodeId("function:main".into()),
            mimi::core::NodeId("function:mid_bad".into()),
            mimi::core::NodeId("function:mid_ok".into()),
        ]
    );
    let inspect = |receipt_first: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("mir").arg(&main);
        if receipt_first {
            command.arg("--receipt").arg("--all");
        } else {
            command.arg("--all").arg("--receipt");
        }
        command
            .output()
            .expect("spawn imported verifier-order receipt inspection")
    };
    let all_first = inspect(false);
    let receipt_first = inspect(true);
    for (label, output) in [("all-first", &all_first), ("receipt-first", &receipt_first)] {
        assert!(
            output.status.success(),
            "{label} imported receipt failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest = String::from_utf8_lossy(&output.stdout);
        let parsed = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(&manifest)
            .unwrap_or_else(|error| panic!("{label} imported receipt round-trip failed: {error}"));
        assert_eq!(parsed, checked);
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    assert_eq!(all_first.stdout, receipt_first.stdout);
    assert_eq!(all_first.stderr, receipt_first.stderr);

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&main)
            .output()
            .expect("spawn imported verifier-order verifier")
    };
    let runs = [verify(false), verify(false), verify(true), verify(true)];
    for (index, output) in runs.iter().enumerate() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(1),
            "imported verifier run {index}"
        );
        assert!(stdout.contains("1/4 verified"));
        assert_eq!(
            stdout
                .matches("canonical MIR extern requires contract proven")
                .count(),
            1,
            "imported verifier run {index} must report one proven FFI receipt"
        );
        assert_eq!(
            stderr
                .matches("canonical MIR extern requires contract disproven")
                .count(),
            1,
            "imported verifier run {index} must report one failed FFI receipt"
        );
        assert!(stderr.contains("leaf.mimi"));
        assert!(stderr.contains("leaf_bad(x)"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert_eq!(
        runs[0].stderr, runs[1].stderr,
        "repeated imported default verifier diagnostics must match"
    );
    assert_eq!(
        runs[2].stderr, runs[3].stderr,
        "repeated imported MIR verifier diagnostics must match"
    );
    assert_eq!(
        runs[0].stderr, runs[2].stderr,
        "imported default and explicit MIR diagnostics must match"
    );
    let stable_summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains("canonical MIR") || line.contains("verified in"))
            .map(|line| {
                let semantic = line.split(" (").next().unwrap_or(line);
                semantic.split(" in ").next().unwrap_or(semantic).to_owned()
            })
            .collect::<Vec<_>>()
    };
    for (index, output) in runs.iter().enumerate().skip(1) {
        assert_eq!(
            stable_summary(&runs[0]),
            stable_summary(output),
            "imported verifier run {index} changed semantic summary"
        );
    }

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_import_graph_failure_is_atomic_across_source_and_all() {
    let dir = project_root().join("target").join(format!(
        "mimi-ffi-import-failure-atomic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create imported FFI failure directory");
    fs::write(
        dir.join("left.mimi"),
        "pub type LeftArg = i64\npub type LeftResult = i64\nextern \"C\" {\n    func clash(value: LeftArg) -> LeftResult;\n}\npub func call_left(value: i64) -> LeftResult { clash(value) }\n",
    )
    .expect("write left imported FFI declaration");
    fs::write(
        dir.join("right.mimi"),
        "pub type RightArg = i64\npub type RightResult = i64\nextern \"C\" {\n    func clash(value: RightArg) -> RightResult;\n}\npub func call_right(value: i64) -> RightResult { clash(value) }\n",
    )
    .expect("write right imported FFI declaration");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use left;\nuse right;\nfunc main() -> i32 { println(call_left(1)); println(call_right(2)); 0 }\n",
    )
    .expect("write imported FFI failure entry");

    let inspect = |flags: &[&str]| {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .args(flags)
            .output()
            .expect("spawn imported FFI failure inspection")
    };
    let source_scope = inspect(&["--receipt"]);
    let all_scope = inspect(&["--receipt", "--all"]);
    let all_reordered = inspect(&["--all", "--receipt"]);
    for (label, output) in [
        ("source", &source_scope),
        ("all", &all_scope),
        ("all-reordered", &all_reordered),
    ] {
        assert_eq!(output.status.code(), Some(1), "{label} failure exit code");
        assert!(
            output.stdout.is_empty(),
            "{label} failure emitted a partial receipt manifest"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("E0402"));
        assert!(stderr.contains("duplicate extern function 'clash'"));
        assert!(stderr.contains("left.mimi") && stderr.contains("right.mimi"));
        assert!(stderr.contains("previous extern declaration is here"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
        assert!(!stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER));
    }
    assert_eq!(source_scope.status.code(), all_scope.status.code());
    assert_eq!(source_scope.stdout, all_scope.stdout);
    assert_eq!(source_scope.stderr, all_scope.stderr);
    assert_eq!(source_scope.stdout, all_reordered.stdout);
    assert_eq!(source_scope.stderr, all_reordered.stderr);

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&main)
            .output()
            .expect("spawn imported FFI failure verifier")
    };
    let verify_default = verify(false);
    let verify_mir = verify(true);
    for (label, output) in [("default", &verify_default), ("mir", &verify_mir)] {
        assert_eq!(output.status.code(), Some(1), "{label} verifier exit code");
        assert!(
            output.stdout.is_empty(),
            "{label} verifier emitted a result"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("E0402"));
        assert!(stderr.contains("duplicate extern function 'clash'"));
        assert!(stderr.contains("left.mimi") && stderr.contains("right.mimi"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
        assert!(!stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER));
    }
    assert_eq!(verify_default.status.code(), verify_mir.status.code());
    assert_eq!(verify_default.stdout, verify_mir.stdout);
    assert_eq!(verify_default.stderr, verify_mir.stderr);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_verify_counts_all_failed_receipts_and_preserves_order() {
    let dir = std::env::temp_dir().join(format!(
        "mimi-ffi-verify-all-failed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create all-failed verifier directory");
    let source = dir.join("all-failed.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func first(value: i64) -> i64 requires: value >= 0;\n    func second(value: i64) -> i64 requires: value >= 0 ensures: result == value;\n    func third(value: i64) -> i64 requires: value >= 0;\n}\nfunc main() -> i64 {\n    first(-1 as i64);\n    second(-2 as i64);\n    third(-3 as i64);\n    0\n}\n",
    )
    .expect("write all-failed verifier source");

    let checked = checked_route_receipt(&source);
    let inspect = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn all-failed verifier receipt inspection");
    assert!(inspect.status.success());
    let manifest = String::from_utf8_lossy(&inspect.stdout);
    assert_eq!(
        mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(&manifest),
        Ok(checked)
    );

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .output()
            .expect("spawn all-failed verifier")
    };
    let default_verify = verify(false);
    let mir_verify = verify(true);
    for (label, output) in [("default", &default_verify), ("mir", &mir_verify)] {
        assert_eq!(output.status.code(), Some(1), "{label} verifier exit code");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("0/3 verified"));
        assert!(stdout.contains("12 total constraints"));
        assert!(!stdout.contains("canonical MIR extern requires contract proven"));
        assert_eq!(
            stderr
                .matches("canonical MIR extern requires contract disproven")
                .count(),
            3,
            "{label} verifier must report every failed receipt"
        );
        let first = stderr
            .find("first(-1 as i64)")
            .expect("first failure source");
        let second = stderr
            .find("second(-2 as i64)")
            .expect("second failure source");
        let third = stderr
            .find("third(-3 as i64)")
            .expect("third failure source");
        assert!(
            first < second && second < third,
            "{label} receipt order changed"
        );
        assert_eq!(stderr.matches("first(-1 as i64)").count(), 1);
        assert_eq!(stderr.matches("second(-2 as i64)").count(), 1);
        assert_eq!(stderr.matches("third(-3 as i64)").count(), 1);
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert_eq!(
        default_verify.stderr, mir_verify.stderr,
        "default and explicit MIR all-failed diagnostics must match"
    );
    let stable_summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains("verified in"))
            .map(|line| line.split(" in ").next().unwrap_or(line).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        stable_summary(&default_verify),
        stable_summary(&mir_verify),
        "default and explicit MIR all-failed summaries must match"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_cli_mixed_verify_and_build_share_receipt_failures() {
    let dir = std::env::temp_dir().join(format!(
        "mimi-ffi-mixed-verify-build-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create mixed verifier/build directory");
    let source = dir.join("mixed.mimi");
    fs::write(
        &source,
        "extern \"C\" {\n    func first(value: i64) -> i64 requires: value >= 0 ensures: true;\n    func second(value: i64) -> i64 requires: value >= 0 ensures: result == value;\n    func third(value: i64) -> i64 requires: value >= 0 ensures: true;\n}\nfunc main() -> i64 {\n    first(7 as i64);\n    second(-8 as i64);\n    third(-9 as i64);\n    0\n}\n",
    )
    .expect("write mixed verifier/build source");

    let checked = checked_route_receipt(&source);
    let inspect = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&source)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn mixed verifier/build receipt inspection");
    assert!(inspect.status.success());
    assert_eq!(
        mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(&String::from_utf8_lossy(
            &inspect.stdout
        )),
        Ok(checked)
    );

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command.arg(&source).output().expect("spawn mixed verifier")
    };
    let default_verify = verify(false);
    let mir_verify = verify(true);
    for (label, output) in [("default", &default_verify), ("mir", &mir_verify)] {
        assert_eq!(output.status.code(), Some(1), "{label} verifier exit code");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("1/3 verified"));
        assert!(stdout.contains("11 total constraints"));
        assert_eq!(
            stdout
                .matches("canonical MIR extern requires contract proven")
                .count(),
            1,
            "{label} verifier must report the one proven receipt"
        );
        assert_eq!(
            stderr
                .matches("canonical MIR extern requires contract disproven")
                .count(),
            2,
            "{label} verifier must report both failed receipts"
        );
        assert!(stderr.find("second(-8 as i64)") < stderr.find("third(-9 as i64)"));
        assert!(!stderr.contains("first(7 as i64)"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert_eq!(default_verify.stderr, mir_verify.stderr);

    let build = |explicit_mir: bool, binary: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg("--verify-ffi")
            .arg("--emit-ir")
            .arg(&source)
            .arg("-o")
            .arg(binary)
            .output()
            .expect("spawn mixed build verifier")
    };
    let default_binary = dir.join("mixed-default");
    let mir_binary = dir.join("mixed-mir");
    let default_build = build(false, &default_binary);
    let mir_build = build(true, &mir_binary);
    for (label, output, binary) in [
        ("default", &default_build, &default_binary),
        ("mir", &mir_build, &mir_binary),
    ] {
        assert_eq!(output.status.code(), Some(1), "{label} build exit code");
        assert!(output.stdout.is_empty(), "{label} build emitted stdout");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.matches("FFI violation").count(), 2);
        assert_eq!(
            stderr.matches("FFI contract verification failed").count(),
            1
        );
        assert_eq!(stderr.matches("second(-8 as i64)").count(), 1);
        assert_eq!(stderr.matches("third(-9 as i64)").count(), 1);
        assert!(!stderr.contains("first(7 as i64)"));
        assert!(!stderr.contains("canonical route disposition: legacy"));
        assert!(!binary.exists(), "{label} build left an output binary");
    }
    assert_eq!(default_build.stderr, mir_build.stderr);
    for snippet in ["second(-8 as i64)", "third(-9 as i64)"] {
        assert!(
            String::from_utf8_lossy(&default_verify.stderr).contains(snippet)
                && String::from_utf8_lossy(&default_build.stderr).contains(snippet),
            "verify/build lost the shared failed receipt source: {snippet}"
        );
    }

    fs::remove_dir_all(&dir).ok();
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
fn canonical_mir_cli_receipt_manifest_deduplicates_flow_root_owners() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_native_flow_transition.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--receipt")
        .output()
        .expect("failed to spawn Flow MIR receipt");
    assert!(
        output.status.success(),
        "Flow MIR receipt failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let manifest = parse_route_receipt_manifest(&output.stdout);
    assert_eq!(
        manifest.get("root_owners").map(String::as_str),
        Some("function:main,transition:Counter::inc::Zero")
    );
    let owners = manifest
        .get("root_owners")
        .expect("Flow receipt root owners")
        .split(',')
        .collect::<Vec<_>>();
    assert!(owners.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        owners.len(),
        owners
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    for field in mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_FIELDS {
        let value = manifest
            .get(field)
            .unwrap_or_else(|| panic!("Flow receipt missing manifest field {field}"));
        assert!(!value.is_empty(), "Flow receipt field {field} is empty");
        if field.ends_with("digest") {
            assert_eq!(
                value.len(),
                64,
                "Flow receipt field {field} has unstable width"
            );
            assert!(value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        }
    }
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
        let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(
            &String::from_utf8_lossy(&manifest_output.stdout),
        )
        .unwrap_or_else(|error| panic!("{fixture_name}: receipt round-trip failed: {error}"));
        assert_eq!(
            manifest_receipt, checked,
            "{fixture_name}: manifest round-trip changed the checked receipt"
        );
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
    let helper_two = dir.join("helper_two.mimi");
    fs::write(&helper_two, "pub func imported_value_two() -> i32 { 1 }\n")
        .expect("write second imported helper");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use helper;\nuse helper_two;\nfunc main() -> i32 { imported_value() + imported_value_two() }\n",
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
    assert!(
        source_only.stdout.is_empty(),
        "source-only import graph must not emit a partial receipt: {}",
        String::from_utf8_lossy(&source_only.stdout)
    );
    let source_only_stderr = String::from_utf8_lossy(&source_only.stderr);
    assert!(
        source_only_stderr.contains("MIR inspection input rejected"),
        "source-only import graph rejection lost its diagnostic: {source_only_stderr}"
    );
    assert!(
        source_only_stderr.contains("MIR validation failed"),
        "source-only import graph lost its stable validation phase: {source_only_stderr}"
    );
    assert!(
        !source_only_stderr.contains("Validation(["),
        "source-only import graph leaked debug-shaped MIR error: {source_only_stderr}"
    );
    assert!(
        !source_only_stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
        "source-only import graph claimed a receipt manifest on failure: {source_only_stderr}"
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
    let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(
        &String::from_utf8_lossy(&first.stdout),
    )
    .expect("import graph receipt must round-trip through the public API");
    assert_eq!(
        manifest_receipt, checked,
        "import graph receipt round-trip changed the checked receipt"
    );
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
    assert!(root_owners.contains("function:imported_value_two"));
    assert_eq!(
        manifest.len(),
        mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_receipt_manifest_imported_transparent_alias_matches_checker_route() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-import-alias-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create imported alias receipt fixture directory");
    let helper = dir.join("ffi_types.mimi");
    fs::write(
        &helper,
        "pub type Scalar = f64\npub type Real = Scalar\npub type ScalarInt = i64\npub type ResultId = ScalarInt\nextern \"C\" { func mir_ffi_cli_import_alias(value: Real) -> ResultId; }\npub func imported_alias(value: i64) -> ResultId { mir_ffi_cli_import_alias(value) }\n",
    )
    .expect("write imported alias receipt helper");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use ffi_types;\nfunc main() -> i64 { imported_alias(7 as i64) }\n",
    )
    .expect("write imported alias receipt entry");

    let run = || {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .arg("--all")
            .arg("--receipt")
            .output()
            .expect("spawn imported alias MIR receipt")
    };
    let first = run();
    let second = run();
    assert!(
        first.status.success(),
        "imported alias MIR receipt failed:\n{}\n{}",
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&first.stdout)
    );
    assert!(
        second.status.success(),
        "repeated imported alias MIR receipt failed:\n{}\n{}",
        String::from_utf8_lossy(&second.stderr),
        String::from_utf8_lossy(&second.stdout)
    );
    assert_eq!(
        first.stdout, second.stdout,
        "imported transparent alias receipt must be byte-deterministic"
    );

    let checked = checked_route_receipt(&main);
    let manifest_text = String::from_utf8_lossy(&first.stdout);
    let manifest = parse_route_receipt_manifest(&first.stdout);
    let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(&manifest_text)
        .expect("imported alias receipt must round-trip through the public API");
    assert_eq!(manifest_receipt, checked);
    assert_eq!(manifest.get("ffi_digest"), Some(&checked.ffi_digest));
    assert_eq!(manifest.get("mir_digest"), Some(&checked.mir_digest));
    assert_eq!(manifest.get("abi_digest"), Some(&checked.abi_digest));
    assert!(
        manifest
            .get("root_owners")
            .is_some_and(|owners| owners.contains("function:main")
                && owners.contains("function:imported_alias")),
        "imported alias receipt lost merged callable owners: {manifest:?}"
    );
    assert_eq!(
        manifest.len(),
        mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        stderr.contains("lowered") && stderr.contains("canonical MIR"),
        "imported alias receipt lost lowering diagnostic: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "imported transparent alias receipt must not report legacy: {stderr}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_imported_alias_default_consumers_match_explicit_mir() {
    if !can_link() {
        return;
    }
    let dir = project_root().join("target").join(format!(
        "mimi-cli-imported-alias-consumers-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create imported alias consumer fixture directory");
    let library = dir.join("ffi.so");
    fs::write(
        dir.join("ffi.c"),
        "#include <stdint.h>\nint64_t mir_ffi_cli_import_alias(double value) { return value == 7.0 ? 42 : -1; }\n",
    )
    .expect("write imported alias consumer C fixture");
    let compile_c = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2"])
        .arg(dir.join("ffi.c"))
        .arg("-o")
        .arg(&library)
        .output()
        .expect("compile imported alias consumer C fixture");
    assert!(
        compile_c.status.success(),
        "{}",
        String::from_utf8_lossy(&compile_c.stderr)
    );
    fs::write(
        dir.join("ffi_types.mimi"),
        "pub type Scalar = f64\npub type Real = Scalar\npub type ScalarInt = i64\npub type ResultId = ScalarInt\nextern \"C\" { func mir_ffi_cli_import_alias(value: Real) -> ResultId requires: value >= 0; }\npub func imported_alias(value: i64) -> ResultId { requires: value > 0\n    mir_ffi_cli_import_alias(value)\n}\n",
    )
    .expect("write imported alias consumer helper");
    let source = dir.join("main.mimi");
    fs::write(
        &source,
        "use ffi_types;\nfunc main() -> i64 { println(imported_alias(7 as i64)); 0 }\n",
    )
    .expect("write imported alias consumer entry");

    for explicit_mir in [false, true] {
        let mut run = Command::new(mimi_bin());
        run.current_dir(project_root()).arg("run");
        if explicit_mir {
            run.arg("--mir");
        }
        let run = run
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .env("MIMI_FFI_LIB", &library)
            .output()
            .expect("spawn imported alias consumer run");
        assert!(
            run.status.success(),
            "explicit_mir={explicit_mir}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.stdout, b"42\n", "explicit_mir={explicit_mir}");
        assert!(
            !String::from_utf8_lossy(&run.stderr).contains("canonical route disposition: legacy"),
            "imported alias run must not fall back to legacy"
        );

        let mut build = Command::new(mimi_bin());
        build.current_dir(project_root()).arg("build");
        if explicit_mir {
            build.arg("--mir");
        }
        let build = build
            .arg("--emit-ir")
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .output()
            .expect("spawn imported alias consumer build");
        assert!(
            build.status.success(),
            "explicit_mir={explicit_mir}: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        assert!(
            String::from_utf8_lossy(&build.stdout).contains("mir_ffi_cli_import_alias"),
            "imported alias build must retain the external symbol"
        );
        assert!(
            !String::from_utf8_lossy(&build.stderr).contains("canonical route disposition: legacy"),
            "imported alias build must not fall back to legacy"
        );

        let mut verify = Command::new(mimi_bin());
        verify.current_dir(project_root()).arg("verify");
        if explicit_mir {
            verify.arg("--mir");
        }
        let verify = verify
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .output()
            .expect("spawn imported alias consumer verify");
        assert!(
            verify.status.success(),
            "explicit_mir={explicit_mir}: {}",
            String::from_utf8_lossy(&verify.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&verify.stderr)
                .contains("canonical route disposition: legacy"),
            "imported alias verify must not fall back to legacy"
        );
        assert!(
            String::from_utf8_lossy(&verify.stdout)
                .contains("canonical MIR extern requires contract proven"),
            "imported alias verify must expose the canonical FFI proof: {}",
            String::from_utf8_lossy(&verify.stdout)
        );
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_imported_alias_negative_contract_matches_explicit_mir() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-imported-alias-negative-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create imported alias negative fixture directory");
    fs::write(
        dir.join("ffi_types.mimi"),
        "pub type Scalar = f64\npub type Real = Scalar\npub type ScalarInt = i64\npub type ResultId = ScalarInt\nextern \"C\" { func mir_ffi_cli_import_alias(value: Real) -> ResultId requires: value >= 0; }\npub func imported_alias(value: i64) -> ResultId { mir_ffi_cli_import_alias(value) }\n",
    )
    .expect("write imported alias negative helper");
    let source = dir.join("main.mimi");
    fs::write(
        &source,
        "use ffi_types;\nfunc main() -> i64 { imported_alias(-7 as i64) }\n",
    )
    .expect("write imported alias negative entry");

    for explicit_mir in [false, true] {
        let mut verify = Command::new(mimi_bin());
        verify.current_dir(project_root()).arg("verify");
        if explicit_mir {
            verify.arg("--mir");
        }
        let verify = verify
            .arg(&source)
            .env("MIMI_VERBOSE", "1")
            .output()
            .expect("spawn imported alias negative verification");
        assert!(
            !verify.status.success(),
            "invalid imported alias contract must fail verification (explicit_mir={explicit_mir})"
        );
        let stderr = String::from_utf8_lossy(&verify.stderr);
        assert!(
            stderr.contains("canonical MIR extern requires contract disproven"),
            "negative imported alias lost canonical disproven diagnostic: {stderr}"
        );
        assert!(
            stderr.contains("ffi_types.mimi")
                && stderr.contains("pub func imported_alias(value: i64)"),
            "negative imported alias lost verifier source provenance: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "negative imported alias must not be reclassified as legacy: {stderr}"
        );
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_native_declaration_error_keeps_call_span() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-native-ffi-diagnostic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create native FFI diagnostic directory");
    let source = dir.join("main.mimi");
    fs::write(
        &source,
        "extern \"C\" { func mimi_session_pair(value: i64) -> i64; }\nfunc main() -> i64 { mimi_session_pair(1 as i64) }\n",
    )
    .expect("write native FFI diagnostic source");
    let binary = dir.join("out");
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("spawn native FFI diagnostic build");

    assert!(
        !build.status.success(),
        "reserved FFI symbol unexpectedly built"
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains("main.mimi")
            && stderr.contains("main.mimi:2:")
            && stderr.contains("src: func main() -> i64 { mimi_session_pair")
            && stderr.contains("FFI symbol collides with an already-declared native MIR function"),
        "native FFI diagnostic lost receipt source span: {stderr}"
    );
    assert!(
        stderr.contains("canonical MIR native backend capability check failed"),
        "native FFI diagnostic lost canonical failure stage: {stderr}"
    );
    assert!(
        !binary.exists(),
        "failed native FFI build left an output binary"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_native_multi_module_declaration_failure_is_source_ordered() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-native-ffi-multi-module-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-module native FFI directory");
    fs::write(
        dir.join("left.mimi"),
        "extern \"C\" { func mimi_session_pair(value: i64) -> i64; }\npub func left(value: i64) -> i64 { mimi_session_pair(value) }\n",
    )
    .expect("write first reserved FFI module");
    fs::write(
        dir.join("right.mimi"),
        "extern \"C\" { func mimi_channel_drop(value: i64) -> i64; }\npub func right(value: i64) -> i64 { mimi_channel_drop(value) }\n",
    )
    .expect("write second reserved FFI module");
    let source = dir.join("main.mimi");
    fs::write(
        &source,
        "use left;\nuse right;\nfunc main() -> i64 { left(1 as i64) + right(2 as i64) }\n",
    )
    .expect("write multi-module native FFI entry");
    let run_build = |explicit_mir: bool, output: &Path| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("build");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&source)
            .arg("-o")
            .arg(output)
            .output()
            .expect("spawn multi-module native FFI build")
    };
    let default_binary = dir.join("default.out");
    let mir_binary = dir.join("mir.out");
    let build_default = run_build(false, &default_binary);
    let build_mir = run_build(true, &mir_binary);
    for (label, build) in [("default", &build_default), ("mir", &build_mir)] {
        assert!(
            !build.status.success(),
            "reserved multi-module FFI symbols unexpectedly built ({label})"
        );
        assert!(
            build.stdout.is_empty(),
            "failed multi-module FFI build emitted stdout ({label})"
        );
    }
    assert_eq!(
        build_default.stderr, build_mir.stderr,
        "default and explicit MIR builds changed multi-module native FFI diagnostics"
    );
    let stderr = String::from_utf8_lossy(&build_default.stderr);
    assert!(
        stderr.contains("left.mimi")
            && stderr.contains("left.mimi:2:")
            && stderr.contains("mimi_session_pair(value)")
            && stderr.contains("FFI symbol collides with an already-declared native MIR function"),
        "multi-module native FFI diagnostic did not use the first canonical caller span: {stderr}"
    );
    assert!(
        stderr.contains("canonical MIR native backend capability check failed"),
        "multi-module native FFI failure lost canonical capability stage: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "multi-module native FFI failure fell back to legacy: {stderr}"
    );
    assert!(
        !default_binary.exists() && !mir_binary.exists(),
        "failed multi-module native FFI build left an output binary"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_all_receipt_multi_module_failure_is_atomic() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-multi-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-module failure directory");
    fs::write(dir.join("good.mimi"), "pub func good() -> i32 { 41 }\n")
        .expect("write supported helper");
    fs::write(
        dir.join("bad.mimi"),
        "pub func bad(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use good;\nuse bad;\nfunc main() -> i32 { good() }\n",
    )
    .expect("write multi-module entry");

    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn multi-module failure receipt");
    assert!(
        !output.status.success(),
        "unsupported imported helper must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.is_empty(),
        "multi-module failure emitted a partial manifest: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MIR inspection input rejected") && stderr.contains("Copy scalar"),
        "multi-module failure lost its canonical lowering diagnostic: {stderr}"
    );
    assert!(
        !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
        "multi-module failure claimed a receipt manifest: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "multi-module failure fell back to legacy: {stderr}"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_all_receipt_multi_module_failure_is_import_order_stable() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-multi-order-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-module order directory");
    fs::write(dir.join("good.mimi"), "pub func good() -> i32 { 41 }\n")
        .expect("write supported helper");
    fs::write(
        dir.join("bad.mimi"),
        "pub func bad(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write unsupported helper");
    let bad_first = dir.join("main_bad_first.mimi");
    fs::write(
        &bad_first,
        "use bad;\nuse good;\nfunc main() -> i32 { good() }\n",
    )
    .expect("write bad-first entry");
    let good_first = dir.join("main_good_first.mimi");
    fs::write(
        &good_first,
        "use good;\nuse bad;\nfunc main() -> i32 { good() }\n",
    )
    .expect("write good-first entry");

    let run = |main: &Path| {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(main)
            .arg("--all")
            .arg("--receipt")
            .output()
            .expect("spawn ordered multi-module failure receipt")
    };
    let bad_first_output = run(&bad_first);
    let good_first_output = run(&good_first);
    for output in [&bad_first_output, &good_first_output] {
        assert!(
            !output.status.success(),
            "unsupported imported helper must fail closed:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "failure emitted a partial manifest"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected") && stderr.contains("Copy scalar"),
            "multi-module failure lost its canonical lowering diagnostic: {stderr}"
        );
        assert!(
            !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "multi-module failure claimed a receipt manifest: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "multi-module failure fell back to legacy: {stderr}"
        );
    }
    assert_eq!(
        bad_first_output.stderr, good_first_output.stderr,
        "import declaration order changed the canonical failure classification"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_all_receipt_multi_module_failures_are_sorted_and_atomic() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-multi-failures-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-failure directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let a_first = dir.join("main_a_first.mimi");
    fs::write(
        &a_first,
        "use bad_a;\nuse bad_b;\nfunc main() -> i32 { 0 }\n",
    )
    .expect("write first-order entry");
    let b_first = dir.join("main_b_first.mimi");
    fs::write(
        &b_first,
        "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n",
    )
    .expect("write second-order entry");

    let run = |main: &Path| {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(main)
            .arg("--all")
            .arg("--receipt")
            .output()
            .expect("spawn multi-failure receipt")
    };
    let a_first_output = run(&a_first);
    let b_first_output = run(&b_first);
    for output in [&a_first_output, &b_first_output] {
        assert!(
            !output.status.success(),
            "unsupported imported helpers must fail closed:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "failure emitted a partial manifest"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected")
                && stderr.contains("MIR lowering failed (2 errors)")
                && stderr.contains("function:bad_a/")
                && stderr.contains("function:bad_b/")
                && stderr.contains("Copy scalar"),
            "multi-module failure lost one of its canonical lowering diagnostics: {stderr}"
        );
        assert!(
            !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "multi-module failure claimed a receipt manifest: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "multi-module failure fell back to legacy: {stderr}"
        );
    }
    assert_eq!(
        a_first_output.stderr, b_first_output.stderr,
        "import declaration order changed the sorted canonical failures"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_all_receipt_multi_module_failure_repeats_byte_identically() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-multi-repeat-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-repeat failure directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write repeated-failure entry");

    let run = || {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .arg("--all")
            .arg("--receipt")
            .output()
            .expect("spawn repeated multi-failure receipt")
    };
    let outputs = (0..3).map(|_| run()).collect::<Vec<_>>();
    for output in &outputs {
        assert!(
            !output.status.success(),
            "unsupported imported helpers must fail closed:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "failure emitted a partial manifest"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected")
                && stderr.contains("MIR lowering failed (2 errors)")
                && stderr.contains("function:bad_a/")
                && stderr.contains("function:bad_b/")
                && stderr.contains("Copy scalar"),
            "repeated failure lost one of its canonical lowering diagnostics: {stderr}"
        );
        assert!(
            !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "repeated failure claimed a receipt manifest: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "repeated failure fell back to legacy: {stderr}"
        );
    }
    for pair in outputs.windows(2) {
        assert_eq!(
            pair[0].status.code(),
            pair[1].status.code(),
            "repeated failure exit code changed"
        );
        assert_eq!(
            pair[0].stdout, pair[1].stdout,
            "repeated failure stdout changed"
        );
        assert_eq!(
            pair[0].stderr, pair[1].stderr,
            "repeated failure stderr changed"
        );
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_all_receipt_failure_matches_plain_mir_entry() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-cross-entry-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create cross-entry failure directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write cross-entry failure entry");

    let run = |with_receipt: bool| {
        let mut command = Command::new(mimi_bin());
        command
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .arg("--all");
        if with_receipt {
            command.arg("--receipt");
        }
        command.output().expect("spawn cross-entry failure receipt")
    };
    let plain = run(false);
    let receipt = run(true);
    for output in [&plain, &receipt] {
        assert!(
            !output.status.success(),
            "unsupported imported helpers must fail closed:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "failure emitted a partial manifest"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected")
                && stderr.contains("MIR lowering failed (2 errors)")
                && stderr.contains("function:bad_a/")
                && stderr.contains("function:bad_b/")
                && stderr.contains("Copy scalar"),
            "cross-entry failure lost one of its canonical lowering diagnostics: {stderr}"
        );
        assert!(
            !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "cross-entry failure claimed a receipt manifest: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "cross-entry failure fell back to legacy: {stderr}"
        );
    }
    assert_eq!(plain.status.code(), receipt.status.code());
    assert_eq!(plain.stdout, receipt.stdout);
    assert_eq!(
        plain.stderr, receipt.stderr,
        "receipt flag changed the canonical failure classification"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_cli_all_receipt_failure_ignores_option_order() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-option-order-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create option-order failure directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write option-order failure entry");

    let run = |receipt_first: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("mir").arg(&main);
        if receipt_first {
            command.arg("--receipt").arg("--all");
        } else {
            command.arg("--all").arg("--receipt");
        }
        command
            .output()
            .expect("spawn option-order failure receipt")
    };
    let all_first = run(false);
    let receipt_first = run(true);
    for output in [&all_first, &receipt_first] {
        assert!(
            !output.status.success(),
            "unsupported imported helpers must fail closed:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stdout.is_empty(),
            "failure emitted a partial manifest"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected")
                && stderr.contains("MIR lowering failed (2 errors)")
                && stderr.contains("function:bad_a/")
                && stderr.contains("function:bad_b/")
                && stderr.contains("Copy scalar"),
            "option-order failure lost one of its canonical lowering diagnostics: {stderr}"
        );
        assert!(
            !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "option-order failure claimed a receipt manifest: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "option-order failure fell back to legacy: {stderr}"
        );
    }
    assert_eq!(all_first.status.code(), receipt_first.status.code());
    assert_eq!(all_first.stdout, receipt_first.stdout);
    assert_eq!(
        all_first.stderr, receipt_first.stderr,
        "CLI option order changed the canonical failure classification"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_multi_module_consumers_reject_unsupported_helpers_without_fallback() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-mir-multi-consumer-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-consumer failure directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write multi-consumer failure entry");

    for command in ["run", "build", "verify"] {
        let output = Command::new(mimi_bin())
            .current_dir(project_root())
            .arg(command)
            .arg(&main)
            .arg("--mir")
            .output()
            .unwrap_or_else(|error| panic!("{command} multi-module MIR failure: {error}"));
        assert!(
            !output.status.success(),
            "{command} must reject unsupported imported helpers"
        );
        assert!(
            output.stdout.is_empty(),
            "{command} emitted output before canonical rejection: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR lowering failed (2 errors)")
                && stderr.contains("function:bad_a/")
                && stderr.contains("function:bad_b/")
                && stderr.contains("Copy scalar"),
            "{command} lost a multi-module canonical lowering diagnostic: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("flow_ast")
                && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "{command} leaked a compatibility route or receipt: {stderr}"
        );
        if command == "verify" {
            assert!(
                stderr.contains("canonical MIR verifier input rejected"),
                "verify lost its verifier-stage classification: {stderr}"
            );
        } else {
            assert!(
                stderr.contains("canonical MIR build error"),
                "{command} lost its build-stage classification: {stderr}"
            );
        }
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_multi_module_consumer_option_order_is_stable() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-mir-consumer-option-order-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create consumer option-order directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write consumer option-order entry");

    for command in ["run", "build", "verify"] {
        let run = |flag_first: bool| {
            let mut invocation = Command::new(mimi_bin());
            invocation.current_dir(project_root()).arg(command);
            if flag_first {
                invocation.arg("--mir").arg(&main);
            } else {
                invocation.arg(&main).arg("--mir");
            }
            invocation
                .output()
                .unwrap_or_else(|error| panic!("{command} option-order MIR failure: {error}"))
        };
        let flag_first = run(true);
        let path_first = run(false);
        for output in [&flag_first, &path_first] {
            assert!(
                !output.status.success(),
                "{command} must reject unsupported imported helpers"
            );
            assert!(
                output.stdout.is_empty(),
                "{command} emitted output before canonical rejection: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("MIR lowering failed (2 errors)")
                    && stderr.contains("function:bad_a/")
                    && stderr.contains("function:bad_b/")
                    && stderr.contains("Copy scalar"),
                "{command} option order lost a canonical lowering diagnostic: {stderr}"
            );
            assert!(
                !stderr.contains("canonical route disposition: legacy")
                    && !stderr.contains("flow_ast")
                    && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
                "{command} option order leaked a compatibility route or receipt: {stderr}"
            );
        }
        assert_eq!(flag_first.status.code(), path_first.status.code());
        assert_eq!(flag_first.stdout, path_first.stdout);
        assert_eq!(
            flag_first.stderr, path_first.stderr,
            "{command} option order changed the canonical failure classification"
        );
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_multi_module_consumer_failures_repeat_byte_identically() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-mir-consumer-repeat-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create consumer repeat directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write consumer repeat entry");

    for command in ["run", "build", "verify"] {
        let run = || {
            Command::new(mimi_bin())
                .current_dir(project_root())
                .arg(command)
                .arg(&main)
                .arg("--mir")
                .output()
                .unwrap_or_else(|error| panic!("{command} repeated MIR failure: {error}"))
        };
        let first = run();
        let second = run();
        for output in [&first, &second] {
            assert!(
                !output.status.success(),
                "{command} must reject unsupported imported helpers"
            );
            assert!(
                output.stdout.is_empty(),
                "{command} emitted output before canonical rejection: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("MIR lowering failed (2 errors)")
                    && stderr.contains("function:bad_a/")
                    && stderr.contains("function:bad_b/")
                    && stderr.contains("Copy scalar"),
                "{command} repeated failure lost a canonical lowering diagnostic: {stderr}"
            );
            assert!(
                !stderr.contains("canonical route disposition: legacy")
                    && !stderr.contains("flow_ast")
                    && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
                "{command} repeated failure leaked a compatibility route or receipt: {stderr}"
            );
        }
        assert_eq!(first.status.code(), second.status.code());
        assert_eq!(first.stdout, second.stdout);
        assert_eq!(
            first.stderr, second.stderr,
            "{command} repeated failure changed its canonical diagnostic"
        );
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_receipt_source_scope_and_all_classify_imported_failure_distinctly() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-receipt-source-scope-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create source-scope failure directory");
    fs::write(
        dir.join("bad.mimi"),
        "pub func bad(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write unsupported imported helper");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use bad;\nfunc main() -> i32 { println(bad([\"x\"])); 0 }\n",
    )
    .expect("write source-scope failure entry");

    let run = |include_all: bool| {
        let mut command = Command::new(mimi_bin());
        command
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .arg("--receipt");
        if include_all {
            command.arg("--all");
        }
        command
            .output()
            .expect("spawn source-scope receipt failure")
    };
    let source_scope = run(false);
    let all_scope = run(true);
    for output in [&source_scope, &all_scope] {
        assert!(
            !output.status.success(),
            "unsupported imported call must fail closed"
        );
        assert!(
            output.stdout.is_empty(),
            "failure emitted a partial manifest: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected")
                && !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "source-scope receipt failure leaked an invalid route or manifest: {stderr}"
        );
    }
    let source_stderr = String::from_utf8_lossy(&source_scope.stderr);
    assert!(
        source_stderr.contains("MIR validation failed") && source_stderr.contains("function:main"),
        "source-scope failure lost its validation-stage classification: {source_stderr}"
    );
    assert!(
        !source_stderr.contains("function:bad/node:expr.index"),
        "source-scope failure lowered an imported helper unexpectedly: {source_stderr}"
    );
    let all_stderr = String::from_utf8_lossy(&all_scope.stderr);
    assert!(
        all_stderr.contains("MIR lowering failed")
            && all_stderr.contains("function:bad/node:expr.index"),
        "--all failure lost its imported-helper lowering classification: {all_stderr}"
    );
    assert!(
        !all_stderr.contains("MIR validation failed"),
        "--all failure regressed to source-scope validation: {all_stderr}"
    );
    assert_eq!(source_scope.status.code(), all_scope.status.code());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_consumers_require_complete_import_graph_after_source_scope_inspection() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-consumer-import-scope-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create consumer import-scope directory");
    fs::write(
        dir.join("bad.mimi"),
        "pub func bad(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write unsupported imported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad;\nfunc main() -> i32 { println(41); 0 }\n")
        .expect("write source-scope entry");

    let source_scope = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--receipt")
        .output()
        .expect("spawn source-scope inspection");
    assert!(
        source_scope.status.success(),
        "source-scope inspection unexpectedly lowered imported helper:\n{}\n{}",
        String::from_utf8_lossy(&source_scope.stdout),
        String::from_utf8_lossy(&source_scope.stderr)
    );
    let source_stdout = String::from_utf8_lossy(&source_scope.stdout);
    assert!(
        source_stdout.contains("root_owners=function:main")
            && !source_stdout.contains("function:bad"),
        "source-scope receipt included an imported helper: {source_stdout}"
    );
    assert!(
        source_scope.stderr.is_empty()
            || !String::from_utf8_lossy(&source_scope.stderr).contains("function:bad"),
        "source-scope inspection leaked an imported helper diagnostic: {}",
        String::from_utf8_lossy(&source_scope.stderr)
    );

    let all_scope = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--receipt")
        .arg("--all")
        .output()
        .expect("spawn complete-graph inspection");
    assert!(
        !all_scope.status.success(),
        "complete import graph inspection silently skipped unsupported helper"
    );
    assert!(
        all_scope.stdout.is_empty(),
        "complete-graph inspection emitted a partial manifest: {}",
        String::from_utf8_lossy(&all_scope.stdout)
    );
    let all_stderr = String::from_utf8_lossy(&all_scope.stderr);
    assert!(
        all_stderr.contains("MIR inspection input rejected")
            && all_stderr.contains("function:bad/node:expr.index")
            && !all_stderr.contains("canonical route disposition: legacy"),
        "complete-graph inspection lost its fail-closed lowering diagnostic: {all_stderr}"
    );

    let binary = dir.join("consumer-output");
    for (consumer, stage) in [
        ("run", "canonical MIR build error"),
        ("build", "canonical MIR build error"),
        ("verify", "canonical MIR verifier input rejected"),
    ] {
        let mut command = Command::new(mimi_bin());
        command
            .current_dir(project_root())
            .arg(consumer)
            .arg(&main)
            .arg("--mir");
        if consumer == "build" {
            command.arg("-o").arg(&binary);
        }
        let output = command
            .output()
            .expect("spawn explicit MIR complete-graph consumer");
        assert!(
            !output.status.success(),
            "{consumer} --mir silently selected source scope"
        );
        assert!(
            output.stdout.is_empty(),
            "{consumer} --mir emitted output before rejecting imported helper: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(stage)
                && stderr.contains("MIR lowering failed")
                && stderr.contains("function:bad/node:expr.index")
                && !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("bytecode runtime error"),
            "{consumer} --mir did not preserve complete-graph fail-closed classification: {stderr}"
        );
    }
    fs::remove_file(&binary).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_multi_failure_aggregate_is_consumer_invariant() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-multi-failure-consumer-invariant-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create multi-failure consumer directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write multi-failure consumer entry");

    let binary = dir.join("consumer-output");
    let invocations = [
        ("mir", "MIR inspection input rejected"),
        ("run", "canonical MIR build error"),
        ("build", "canonical MIR build error"),
        ("verify", "canonical MIR verifier input rejected"),
    ];
    let mut outputs = Vec::new();
    for (consumer, stage) in invocations {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg(consumer);
        if consumer == "mir" {
            command.arg(&main).arg("--all").arg("--receipt");
        } else {
            command.arg(&main).arg("--mir");
            if consumer == "build" {
                command.arg("-o").arg(&binary);
            }
        }
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("spawn multi-failure {consumer}: {error}"));
        assert!(
            !output.status.success(),
            "{consumer} unexpectedly accepted both unsupported helpers"
        );
        assert!(
            output.stdout.is_empty(),
            "{consumer} emitted partial output before rejecting both helpers: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(stage)
                && stderr.contains("MIR lowering failed (2 errors)")
                && stderr.contains("function:bad_a/")
                && stderr.contains("function:bad_b/")
                && stderr.matches("Copy scalar").count() == 2
                && !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains("flow_ast")
                && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "{consumer} changed the multi-failure aggregate or leaked a fallback: {stderr}"
        );
        let bad_a = stderr
            .find("function:bad_a/")
            .expect("bad_a diagnostic identity");
        let bad_b = stderr
            .find("function:bad_b/")
            .expect("bad_b diagnostic identity");
        assert!(
            bad_a < bad_b,
            "{consumer} changed stable helper error ordering: {stderr}"
        );
        let aggregate = stderr
            .find("MIR lowering failed")
            .map(|index| stderr[index..].to_owned())
            .expect("canonical lowering aggregate");
        outputs.push((consumer, aggregate));
    }
    let expected = &outputs[0].1;
    for (consumer, aggregate) in &outputs[1..] {
        assert_eq!(
            aggregate, expected,
            "{consumer} changed the canonical multi-failure aggregate"
        );
    }
    fs::remove_file(&binary).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_source_scope_and_all_modes_are_repeatable_and_option_order_stable() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-source-all-mode-stability-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create source/all stability directory");
    fs::write(
        dir.join("bad_a.mimi"),
        "pub func bad_a(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write first unsupported helper");
    fs::write(
        dir.join("bad_b.mimi"),
        "pub func bad_b(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write second unsupported helper");
    let main = dir.join("main.mimi");
    fs::write(&main, "use bad_b;\nuse bad_a;\nfunc main() -> i32 { 0 }\n")
        .expect("write source/all stability entry");

    let source_scope = || {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .arg("--receipt")
            .output()
            .expect("spawn source-scope stability receipt")
    };
    let source_first = source_scope();
    let source_second = source_scope();
    for output in [&source_first, &source_second] {
        assert!(
            output.status.success(),
            "source scope unexpectedly rejected its own main:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("root_owners=function:main") && !stdout.contains("function:bad_"),
            "source scope included an imported helper: {stdout}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("function:bad_"),
            "source scope emitted an imported helper diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        source_first.status.code(),
        source_second.status.code(),
        "source-scope receipt status changed across repetitions"
    );
    assert_eq!(
        source_first.stdout, source_second.stdout,
        "source-scope receipt changed across repetitions"
    );
    assert_eq!(
        source_first.stderr, source_second.stderr,
        "source-scope diagnostic changed across repetitions"
    );

    let all_scope = |receipt_first: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("mir").arg(&main);
        if receipt_first {
            command.arg("--receipt").arg("--all");
        } else {
            command.arg("--all").arg("--receipt");
        }
        command
            .output()
            .expect("spawn complete-graph stability receipt")
    };
    let all_first = all_scope(false);
    let all_second = all_scope(false);
    let receipt_first = all_scope(true);
    let receipt_second = all_scope(true);
    for (label, output) in [
        ("all-first", &all_first),
        ("all-second", &all_second),
        ("receipt-first", &receipt_first),
        ("receipt-second", &receipt_second),
    ] {
        assert!(
            !output.status.success(),
            "{label} silently accepted unsupported imported helpers"
        );
        assert!(
            output.stdout.is_empty(),
            "{label} emitted a partial manifest: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected")
                && stderr.contains("MIR lowering failed (2 errors)")
                && stderr.contains("function:bad_a/")
                && stderr.contains("function:bad_b/")
                && stderr.matches("Copy scalar").count() == 2
                && stderr.find("function:bad_a/") < stderr.find("function:bad_b/")
                && !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "{label} changed complete-graph failure classification: {stderr}"
        );
    }
    assert_eq!(
        all_first.status.code(),
        all_second.status.code(),
        "repeated --all receipt status changed"
    );
    assert_eq!(all_first.stdout, all_second.stdout);
    assert_eq!(all_first.stderr, all_second.stderr);
    assert_eq!(
        all_first.status.code(),
        receipt_first.status.code(),
        "receipt option order changed complete-graph status"
    );
    assert_eq!(all_first.stdout, receipt_first.stdout);
    assert_eq!(
        all_first.stderr, receipt_first.stderr,
        "receipt option order changed complete-graph diagnostics"
    );
    assert_ne!(
        source_first.status.code(),
        all_first.status.code(),
        "source scope and --all lost their distinct success/failure boundary"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_all_closes_transitive_import_failures_without_partial_receipt() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-transitive-import-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create transitive import directory");
    fs::write(
        dir.join("bad.mimi"),
        "pub func bad(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write transitive unsupported helper");
    fs::write(
        dir.join("mid.mimi"),
        "use bad;\npub func mid() -> i32 { 1 }\n",
    )
    .expect("write transitive middle module");
    let main = dir.join("main.mimi");
    fs::write(&main, "use mid;\nfunc main() -> i32 { 0 }\n")
        .expect("write transitive import entry");

    let inspect = |include_imports: bool, receipt_first: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("mir").arg(&main);
        if receipt_first {
            command.arg("--receipt");
        }
        if include_imports {
            command.arg("--all");
        }
        if !receipt_first {
            command.arg("--receipt");
        }
        command
            .output()
            .expect("spawn transitive import inspection")
    };

    let source_first = inspect(false, false);
    let source_second = inspect(false, false);
    for output in [&source_first, &source_second] {
        assert!(
            output.status.success(),
            "source scope rejected main before traversing the imported chain:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("root_owners=function:main")
                && !stdout.contains("function:mid")
                && !stdout.contains("function:bad"),
            "source scope leaked transitive imported owners: {stdout}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("function:bad"),
            "source scope leaked a transitive helper diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        source_first.status.code(),
        source_second.status.code(),
        "source-scope transitive receipt status changed across repetitions"
    );
    assert_eq!(
        source_first.stdout, source_second.stdout,
        "source-scope transitive receipt changed across repetitions"
    );
    assert_eq!(
        source_first.stderr, source_second.stderr,
        "source-scope transitive diagnostic changed across repetitions"
    );

    let all_first = inspect(true, false);
    let all_second = inspect(true, false);
    let all_reordered = inspect(true, true);
    for (label, output) in [
        ("all-first", &all_first),
        ("all-second", &all_second),
        ("all-reordered", &all_reordered),
    ] {
        assert!(
            !output.status.success(),
            "{label} silently omitted the transitive unsupported helper"
        );
        assert!(
            output.stdout.is_empty(),
            "{label} emitted a partial transitive receipt: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("MIR inspection input rejected")
                && stderr.contains("MIR lowering failed (1 errors)")
                && stderr.contains("function:bad/node:expr.index")
                && stderr.matches("Copy scalar").count() == 1
                && !stderr.contains("function:mid/node:expr.index")
                && !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "{label} changed transitive failure classification: {stderr}"
        );
    }
    assert_eq!(all_first.status.code(), all_second.status.code());
    assert_eq!(all_first.stdout, all_second.stdout);
    assert_eq!(all_first.stderr, all_second.stderr);
    assert_eq!(all_first.status.code(), all_reordered.status.code());
    assert_eq!(all_first.stdout, all_reordered.stdout);
    assert_eq!(all_first.stderr, all_reordered.stderr);
    assert_ne!(
        source_first.status.code(),
        all_first.status.code(),
        "source scope and transitive --all lost their success/failure boundary"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_transitive_imports_match_checked_receipt_and_consumers() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-transitive-import-success-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create transitive success directory");
    fs::write(dir.join("leaf.mimi"), "pub func leaf() -> i32 { 41 }\n")
        .expect("write transitive leaf");
    fs::write(
        dir.join("mid.mimi"),
        "use leaf;\npub func mid() -> i32 { leaf() + 1 }\n",
    )
    .expect("write transitive middle module");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use mid;\nfunc main() -> i32 { println(mid()); 42 }\n",
    )
    .expect("write transitive success entry");

    let checked = checked_route_receipt(&main);
    let inspect = |receipt_first: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("mir").arg(&main);
        if receipt_first {
            command.arg("--receipt").arg("--all");
        } else {
            command.arg("--all").arg("--receipt");
        }
        command.output().expect("spawn transitive success receipt")
    };
    let receipt_all_first = inspect(false);
    let receipt_all_reordered = inspect(true);
    for (label, output) in [
        ("all-first", &receipt_all_first),
        ("all-reordered", &receipt_all_reordered),
    ] {
        assert!(
            output.status.success(),
            "{label} rejected a supported transitive import graph:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest = parse_route_receipt_manifest(&output.stdout);
        let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(
            &String::from_utf8_lossy(&output.stdout),
        )
        .unwrap_or_else(|error| panic!("{label}: transitive receipt round-trip failed: {error}"));
        assert_eq!(
            manifest_receipt, checked,
            "{label}: CLI transitive receipt diverged from checked API"
        );
        assert_eq!(
            manifest.get("root_owners").map(String::as_str),
            Some("function:leaf,function:main,function:mid"),
            "{label}: transitive root owner order changed"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr)
                .contains("canonical route disposition: legacy"),
            "{label}: transitive receipt selected legacy fallback: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        receipt_all_first.status.code(),
        receipt_all_reordered.status.code()
    );
    assert_eq!(receipt_all_first.stdout, receipt_all_reordered.stdout);
    assert_eq!(receipt_all_first.stderr, receipt_all_reordered.stderr);

    let run_first = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn transitive explicit MIR run");
    let run_second = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&main)
        .arg("--mir")
        .output()
        .expect("spawn transitive reordered MIR run");
    for (label, output) in [("run-first", &run_first), ("run-second", &run_second)] {
        assert_eq!(
            output.status.code(),
            Some(42),
            "{label} returned the wrong transitive result: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n", "{label} changed transitive stdout");
        assert!(
            output.stderr.is_empty(),
            "{label} emitted a transitive legacy diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(run_first.stdout, run_second.stdout);
    assert_eq!(run_first.stderr, run_second.stderr);
    assert_eq!(run_first.status.code(), run_second.status.code());

    let binary = dir.join("transitive-native");
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&main)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("spawn transitive explicit MIR build");
    assert!(
        build.status.success(),
        "transitive explicit MIR build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&build.stderr).contains("canonical route disposition: legacy"),
        "transitive explicit MIR build selected legacy fallback: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = Command::new(&binary)
        .output()
        .expect("execute transitive explicit MIR binary");
    assert_eq!(native.status.code(), Some(42));
    assert_eq!(native.stdout, b"42\n");
    assert!(native.stderr.is_empty());

    let verify_first = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn transitive explicit MIR verifier");
    let verify_second = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&main)
        .arg("--mir")
        .output()
        .expect("spawn transitive reordered MIR verifier");
    let expected_verify = format!("No contracts to verify in {}\n", main.display());
    for (label, output) in [
        ("verify-first", &verify_first),
        ("verify-second", &verify_second),
    ] {
        assert!(
            output.status.success(),
            "{label} rejected supported transitive MIR:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            expected_verify.as_bytes(),
            "{label} changed transitive verifier output"
        );
        assert!(
            output.stderr.is_empty(),
            "{label} emitted a transitive verifier diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(verify_first.stdout, verify_second.stdout);
    assert_eq!(verify_first.stderr, verify_second.stderr);
    assert_eq!(verify_first.status.code(), verify_second.status.code());

    fs::remove_file(&binary).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_import_declaration_order_preserves_checked_graph_identity_and_consumers() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-import-order-identity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create import-order identity directory");
    fs::write(
        dir.join("ffi_leaf.mimi"),
        "extern \"C\" { func labs(value: i64) -> i64; }\npub func ffi_labs(value: i64) -> i64 { labs(value) }\n",
    )
    .expect("write shared FFI leaf");
    fs::write(
        dir.join("leaf_a.mimi"),
        "use ffi_leaf;\npub func leaf_a() -> i64 { ffi_labs(10 as i64) }\n",
    )
    .expect("write first transitive leaf");
    fs::write(
        dir.join("leaf_b.mimi"),
        "use ffi_leaf;\npub func leaf_b() -> i64 { ffi_labs(20 as i64) }\n",
    )
    .expect("write second transitive leaf");
    fs::write(
        dir.join("mid_a.mimi"),
        "use leaf_a;\npub func mid_a() -> i64 { leaf_a() }\n",
    )
    .expect("write first transitive middle module");
    fs::write(
        dir.join("mid_b.mimi"),
        "use leaf_b;\npub func mid_b() -> i64 { leaf_b() }\n",
    )
    .expect("write second transitive middle module");
    let main = dir.join("main.mimi");
    let write_main = |first: &str, second: &str| {
        fs::write(
            &main,
            format!(
                "use {first};\nuse {second};\nfunc main() -> i64 {{ println(mid_a() + mid_b()); 30 }}\n"
            ),
        )
        .expect("write import-order identity entry");
    };
    let inspect = || {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&main)
            .arg("--all")
            .arg("--receipt")
            .output()
            .expect("spawn import-order identity receipt")
    };
    let run = || {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("run")
            .arg("--mir")
            .arg(&main)
            .output()
            .expect("spawn import-order identity MIR run")
    };

    write_main("mid_a", "mid_b");
    let checked_first = checked_route_receipt(&main);
    let receipt_first = inspect();
    let run_first = run();

    write_main("mid_b", "mid_a");
    let checked_second = checked_route_receipt(&main);
    let receipt_second = inspect();
    let run_second = run();

    assert_eq!(
        checked_first, checked_second,
        "swapping import declarations changed checked graph identity"
    );
    assert_eq!(
        checked_first.ffi_digest, checked_second.ffi_digest,
        "swapping import declarations changed the merged FFI receipt digest"
    );
    for (label, output, checked) in [
        ("first", &receipt_first, &checked_first),
        ("second", &receipt_second, &checked_second),
    ] {
        assert!(
            output.status.success(),
            "{label} import order rejected a supported graph:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest = parse_route_receipt_manifest(&output.stdout);
        let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(
            &String::from_utf8_lossy(&output.stdout),
        )
        .unwrap_or_else(|error| panic!("{label}: import-order receipt round-trip failed: {error}"));
        assert_eq!(
            &manifest_receipt, checked,
            "{label}: CLI import-order receipt diverged from checked API"
        );
        assert_eq!(
            manifest.get("ffi_digest").map(String::as_str),
            Some(checked.ffi_digest.as_str()),
            "{label}: CLI import-order FFI digest diverged"
        );
        assert_eq!(
            manifest.get("root_owners").map(String::as_str),
            Some(
                "function:ffi_labs,function:leaf_a,function:leaf_b,function:main,function:mid_a,function:mid_b",
            ),
            "{label}: import-order root owner order changed"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr)
                .contains("canonical route disposition: legacy"),
            "{label}: import-order receipt selected legacy fallback: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(receipt_first.status.code(), receipt_second.status.code());
    assert_eq!(receipt_first.stdout, receipt_second.stdout);
    assert_eq!(receipt_first.stderr, receipt_second.stderr);

    for (label, output) in [("first", &run_first), ("second", &run_second)] {
        assert_eq!(
            output.status.code(),
            Some(30),
            "{label} import order returned the wrong result: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout, b"30\n",
            "{label} import order changed stdout"
        );
        assert!(
            output.stderr.is_empty(),
            "{label} import order emitted a legacy diagnostic: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(run_first.status.code(), run_second.status.code());
    assert_eq!(run_first.stdout, run_second.stdout);
    assert_eq!(run_first.stderr, run_second.stderr);

    let default_binary = dir.join("import-order-ffi-default");
    let mir_binary = dir.join("import-order-ffi-mir");
    let build_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&main)
        .arg("-o")
        .arg(&default_binary)
        .output()
        .expect("spawn default import-order FFI build");
    let build_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&main)
        .arg("-o")
        .arg(&mir_binary)
        .output()
        .expect("spawn explicit import-order FFI build");
    for (label, output) in [("default", &build_default), ("mir", &build_mir)] {
        assert!(
            output.status.success(),
            "{label} import-order FFI build failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    let native_default = Command::new(&default_binary)
        .output()
        .expect("execute default import-order FFI binary");
    let native_mir = Command::new(&mir_binary)
        .output()
        .expect("execute explicit import-order FFI binary");
    for (label, output) in [("default", &native_default), ("mir", &native_mir)] {
        assert_eq!(
            output.status.code(),
            Some(30),
            "{label} import-order FFI binary returned the wrong result"
        );
        assert_eq!(output.stdout, b"30\n");
        assert!(output.stderr.is_empty());
    }

    let verify_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&main)
        .output()
        .expect("spawn default import-order FFI verifier");
    let verify_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn explicit import-order FFI verifier");
    for (label, output) in [("default", &verify_default), ("mir", &verify_mir)] {
        assert!(
            output.status.success(),
            "{label} import-order FFI verifier failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    assert_eq!(verify_default.stdout, verify_mir.stdout);
    assert_eq!(verify_default.stderr, verify_mir.stderr);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_alias_qualified_import_call_fails_closed_before_consumers() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-qualified-import-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create qualified import directory");
    fs::write(dir.join("helper.mimi"), "pub func value() -> i32 { 42 }\n")
        .expect("write qualified import helper");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use helper as left;\nfunc main() -> i32 { left::value() }\n",
    )
    .expect("write qualified import entry");

    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&main)
        .output()
        .expect("spawn default qualified import run");
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&main)
        .output()
        .expect("spawn default qualified import build");
    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--all")
        .output()
        .expect("spawn qualified import MIR inspection");

    for (label, output) in [("run", &run), ("build", &build), ("mir", &mir)] {
        assert_eq!(
            output.status.code(),
            Some(1),
            "{label} unexpectedly accepted an alias-qualified import call:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{label} emitted output before rejecting alias-qualified call: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("E0830") && stderr.contains("left.value"),
            "{label} lost the explicit qualified-call boundary: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy"),
            "{label} reopened a legacy route for alias-qualified import call: {stderr}"
        );
    }

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_duplicate_imported_declarations_fail_closed_across_cli_consumers() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-duplicate-imported-ffi-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create duplicate imported FFI directory");
    fs::write(
        dir.join("left.mimi"),
        "pub type LeftArg = i64\npub type LeftResult = i64\nextern \"C\" {\n    func clash(value: LeftArg) -> LeftResult;\n}\npub func call_left(value: i64) -> LeftResult { clash(value) }\n",
    )
    .expect("write left duplicate imported FFI declaration");
    fs::write(
        dir.join("right.mimi"),
        "pub type RightArg = i64\npub type RightResult = i64\nextern \"C\" {\n    func clash(value: RightArg) -> RightResult;\n}\npub func call_right(value: i64) -> RightResult { clash(value) }\n",
    )
    .expect("write right duplicate imported FFI declaration");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use left;\nuse right;\nfunc main() -> i32 { println(call_left(1)); println(call_right(2)); 0 }\n",
    )
    .expect("write duplicate imported FFI entry");
    let binary = dir.join("duplicate-imported-ffi-output");

    let mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--receipt")
        .arg("--all")
        .output()
        .expect("spawn duplicate imported FFI MIR inspection");
    let run = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn duplicate imported FFI MIR run");
    let build = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&main)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("spawn duplicate imported FFI MIR build");
    let verify = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn duplicate imported FFI MIR verify");

    for (label, output) in [
        ("mir", &mir),
        ("run", &run),
        ("build", &build),
        ("verify", &verify),
    ] {
        assert_eq!(
            output.status.code(),
            Some(1),
            "{label} unexpectedly accepted duplicate imported FFI declarations:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{label} emitted a partial artifact before rejecting duplicate imported FFI declarations: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("E0402") && stderr.contains("duplicate extern function 'clash'"),
            "{label} lost the checker-owned duplicate declaration diagnostic: {stderr}"
        );
        assert!(
            stderr.contains("right.mimi")
                && stderr.contains("left.mimi")
                && stderr.contains("previous extern declaration is here"),
            "{label} lost cross-file duplicate declaration provenance: {stderr}"
        );
        assert!(
            !stderr.contains("canonical route disposition: legacy")
                && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
            "{label} emitted a legacy disposition or receipt manifest after duplicate declaration failure: {stderr}"
        );
    }
    assert!(
        !binary.exists(),
        "failed duplicate imported FFI build left an output binary"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_transitive_import_graph_matches_receipt_and_consumers() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = project_root().join("target").join(format!(
        "mimi-cli-transitive-scalar-ffi-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create transitive scalar FFI directory");
    fs::write(
        dir.join("leaf.mimi"),
        "pub type ScalarInt = i64\npub type ResultId = ScalarInt\nextern \"C\" {\n    func labs(x: i64) -> ResultId requires: x >= 0;\n}\npub func imported_labs(x: i64) -> ResultId {\n    requires: x >= 0\n    labs(x)\n}\n",
    )
    .expect("write transitive scalar FFI leaf");
    fs::write(
        dir.join("mid.mimi"),
        "use leaf;\npub func mid(x: i64) -> ResultId {\n    requires: x >= 0\n    imported_labs(x)\n}\n",
    )
    .expect("write transitive scalar FFI middle module");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use mid;\nfunc main() -> i32 { println(mid(42 as i64)); 0 }\n",
    )
    .expect("write transitive scalar FFI entry");

    let source_scope = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--receipt")
        .output()
        .expect("spawn transitive scalar FFI source-scope inspection");
    assert!(
        !source_scope.status.success(),
        "source-scope inspection silently omitted the imported scalar FFI callee"
    );
    assert!(source_scope.stdout.is_empty());
    let source_scope_stderr = String::from_utf8_lossy(&source_scope.stderr);
    assert!(
        source_scope_stderr.contains("callee 'function:mid' is absent")
            || source_scope_stderr.contains("MIR inspection input rejected"),
        "source-scope rejection lost its imported callee boundary: {source_scope_stderr}"
    );
    assert!(!source_scope_stderr.contains("canonical route disposition: legacy"));
    assert!(!source_scope_stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER));

    let checked = checked_route_receipt(&main);
    let inspect = |receipt_first: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("mir").arg(&main);
        if receipt_first {
            command.arg("--receipt").arg("--all");
        } else {
            command.arg("--all").arg("--receipt");
        }
        command
            .output()
            .expect("spawn transitive scalar FFI receipt")
    };
    let receipt_first = inspect(false);
    let receipt_reordered = inspect(true);
    for (label, output) in [
        ("all-first", &receipt_first),
        ("receipt-first", &receipt_reordered),
    ] {
        assert!(
            output.status.success(),
            "{label} rejected imported scalar FFI graph:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(
            &String::from_utf8_lossy(&output.stdout),
        )
        .unwrap_or_else(|error| panic!("{label}: scalar FFI receipt round-trip failed: {error}"));
        assert_eq!(manifest_receipt, checked);
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    assert_eq!(receipt_first.stdout, receipt_reordered.stdout);
    assert_eq!(receipt_first.stderr, receipt_reordered.stderr);
    assert_eq!(
        checked.root_owners,
        vec![
            mimi::core::NodeId("function:imported_labs".into()),
            mimi::core::NodeId("function:main".into()),
            mimi::core::NodeId("function:mid".into()),
        ]
    );

    let run_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg(&main)
        .output()
        .expect("spawn default transitive scalar FFI run");
    let run_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("run")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn explicit transitive scalar FFI run");
    for (label, output) in [("default", &run_default), ("mir", &run_mir)] {
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} transitive scalar FFI run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"42\n");
        assert!(output.stderr.is_empty());
    }

    let default_binary = dir.join("transitive-scalar-default");
    let mir_binary = dir.join("transitive-scalar-mir");
    let build_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&main)
        .arg("-o")
        .arg(&default_binary)
        .output()
        .expect("spawn default transitive scalar FFI build");
    let build_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&main)
        .arg("-o")
        .arg(&mir_binary)
        .output()
        .expect("spawn explicit transitive scalar FFI build");
    for (label, output) in [("default", &build_default), ("mir", &build_mir)] {
        assert!(
            output.status.success(),
            "{label} transitive scalar FFI build failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    let native_default = Command::new(&default_binary)
        .output()
        .expect("execute default transitive scalar FFI binary");
    let native_mir = Command::new(&mir_binary)
        .output()
        .expect("execute explicit transitive scalar FFI binary");
    for (label, output) in [("default", &native_default), ("mir", &native_mir)] {
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} transitive scalar FFI binary failed"
        );
        assert_eq!(output.stdout, b"42\n");
        assert!(output.stderr.is_empty());
    }

    let verify_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&main)
        .output()
        .expect("spawn default transitive scalar FFI verifier");
    let verify_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn explicit transitive scalar FFI verifier");
    for (label, output) in [("default", &verify_default), ("mir", &verify_mir)] {
        assert!(
            output.status.success(),
            "{label} transitive scalar FFI verifier failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(combined.contains("canonical MIR extern requires contract"));
        assert!(!combined.contains("canonical route disposition: legacy"));
    }

    fs::remove_file(&default_binary).ok();
    fs::remove_file(&mir_binary).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_multi_source_same_symbol_verifier_order_matches_mir() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-multi-source-same-symbol-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create same-symbol verifier fixture directory");
    fs::write(
        dir.join("ffi_types.mimi"),
        "pub type Scalar = f64\npub type ResultId = i64\nextern \"C\" { func mir_ffi_cli_shared(value: Scalar) -> ResultId requires: value >= 0; }\n",
    )
    .expect("write shared FFI declaration module");
    fs::write(
        dir.join("left.mimi"),
        "use ffi_types\npub func left_call(value: i64) -> i64 {\n    requires: value >= 0\n    mir_ffi_cli_shared(value)\n}\n",
    )
    .expect("write left shared-symbol caller");
    fs::write(
        dir.join("right.mimi"),
        "use ffi_types\npub func right_call(value: i64) -> i64 {\n    requires: value >= 0\n    mir_ffi_cli_shared(value)\n}\n",
    )
    .expect("write right shared-symbol caller");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use left\nuse right\nfunc main() -> i64 {\n    let left_value = left_call(1 as i64)\n    let right_value = right_call(2 as i64)\n    println(left_value)\n    println(right_value)\n    0\n}\n",
    )
    .expect("write same-symbol verifier entry");

    let receipt = checked_route_receipt(&main);
    assert_eq!(receipt.ffi_digest.len(), 64);
    let receipt_output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("spawn same-symbol receipt inspection");
    assert!(
        receipt_output.status.success(),
        "same-symbol receipt inspection failed:\n{}\n{}",
        String::from_utf8_lossy(&receipt_output.stdout),
        String::from_utf8_lossy(&receipt_output.stderr)
    );
    assert_eq!(
        parse_route_receipt_manifest(&receipt_output.stdout).get("ffi_digest"),
        Some(&receipt.ffi_digest)
    );

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&main)
            .output()
            .expect("spawn same-symbol verifier")
    };
    let default_verify = verify(false);
    let mir_verify = verify(true);
    let left_marker = "function:left_call: canonical MIR extern requires contract proven";
    let right_marker = "function:right_call: canonical MIR extern requires contract proven";
    let verifier_markers = |label: &str, output: &std::process::Output| {
        assert!(
            output.status.success(),
            "{label} same-symbol verifier failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert_eq!(text.matches(left_marker).count(), 1, "{label}: {text}");
        assert_eq!(text.matches(right_marker).count(), 1, "{label}: {text}");
        let left = text.find(left_marker).expect("left caller verifier marker");
        let right = text
            .find(right_marker)
            .expect("right caller verifier marker");
        assert!(
            left < right,
            "{label}: verifier caller order changed: {text}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    };
    verifier_markers("default", &default_verify);
    verifier_markers("mir", &mir_verify);
    let semantic_summary = |output: &std::process::Output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains("canonical MIR verifier: no ensures contract")
                    || line.contains("canonical MIR extern requires contract proven")
            })
            .map(|line| line.split(" (").next().unwrap_or(line).to_owned())
            .collect::<Vec<_>>()
    };
    let repeated_default = verify(false);
    assert!(repeated_default.status.success());
    assert_eq!(
        semantic_summary(&default_verify),
        semantic_summary(&repeated_default),
        "repeated verifier runs must preserve caller/status/constraint summaries"
    );
    assert_eq!(
        semantic_summary(&default_verify),
        semantic_summary(&mir_verify),
        "default and explicit MIR must preserve semantic verifier summaries"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_multi_source_same_symbol_mixed_verdict_keeps_caller_span() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-multi-source-same-symbol-mixed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create mixed-verdict fixture directory");
    fs::write(
        dir.join("ffi_types.mimi"),
        "pub type Scalar = f64\npub type ResultId = i64\nextern \"C\" { func mir_ffi_cli_shared(value: Scalar) -> ResultId requires: value > 10; }\n",
    )
    .expect("write mixed-verdict FFI declaration module");
    fs::write(
        dir.join("left.mimi"),
        "use ffi_types\npub func left_call(value: i64) -> i64 {\n    requires: value > 10\n    mir_ffi_cli_shared(value)\n}\n",
    )
    .expect("write proven shared-symbol caller");
    fs::write(
        dir.join("right.mimi"),
        "use ffi_types\npub func right_call(value: i64) -> i64 {\n    requires: value >= 0\n    mir_ffi_cli_shared(value)\n}\n",
    )
    .expect("write disproven shared-symbol caller");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use left\nuse right\nfunc main() -> i64 {\n    let left_value = left_call(20 as i64)\n    let right_value = right_call(2 as i64)\n    println(left_value)\n    println(right_value)\n    0\n}\n",
    )
    .expect("write mixed-verdict verifier entry");

    let verify = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&main)
            .output()
            .expect("spawn mixed-verdict verifier")
    };
    let default_verify = verify(false);
    let mir_verify = verify(true);
    for (label, output) in [("default", &default_verify), ("mir", &mir_verify)] {
        assert!(
            !output.status.success(),
            "{label} mixed-verdict verifier unexpectedly passed"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("function:left_call: canonical MIR extern requires contract proven"),
            "{label} lost the proven caller result: {stdout}"
        );
        assert!(
            !stdout.contains("function:right_call: canonical MIR extern requires contract proven"),
            "{label} reported a proven result for the failing caller: {stdout}"
        );
        assert!(
            stderr.contains("right.mimi")
                && stderr.contains("canonical MIR extern requires contract disproven")
                && stderr.contains("mir_ffi_cli_shared(value)"),
            "{label} lost failing caller source provenance: {stderr}"
        );
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert_eq!(
        default_verify.stderr, mir_verify.stderr,
        "default and explicit MIR must preserve mixed-verdict diagnostic identity"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_transitive_import_graph_merges_mixed_width_call_sites() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = project_root().join("target").join(format!(
        "mimi-cli-transitive-scalar-ffi-mixed-width-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create transitive mixed-width FFI directory");
    fs::write(
        dir.join("leaf.mimi"),
        "extern \"C\" {\n    func labs(x: i64) -> i64;\n}\npub func narrow(x: i32) -> i64 { labs(x) }\npub func wide(x: i64) -> i64 { labs(x) }\n",
    )
    .expect("write transitive mixed-width FFI leaf");
    fs::write(
        dir.join("mid.mimi"),
        "use leaf;\npub func combine(x: i32, y: i64) -> i64 { narrow(x) + wide(y) }\n",
    )
    .expect("write transitive mixed-width FFI middle module");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use mid;\nfunc main() -> i32 { println(combine(7, 8 as i64)); 0 }\n",
    )
    .expect("write transitive mixed-width FFI entry");

    let source_scope = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&main)
        .arg("--receipt")
        .output()
        .expect("spawn transitive mixed-width source-scope inspection");
    assert!(!source_scope.status.success());
    assert!(source_scope.stdout.is_empty());
    let source_scope_stderr = String::from_utf8_lossy(&source_scope.stderr);
    assert!(source_scope_stderr.contains("MIR inspection input rejected"));
    assert!(!source_scope_stderr.contains("canonical route disposition: legacy"));
    assert!(!source_scope_stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER));

    let checked = checked_route_receipt(&main);
    assert_eq!(
        checked.root_owners,
        vec![
            mimi::core::NodeId("function:combine".into()),
            mimi::core::NodeId("function:main".into()),
            mimi::core::NodeId("function:narrow".into()),
            mimi::core::NodeId("function:wide".into()),
        ]
    );
    assert_eq!(checked.ffi_digest.len(), 64);
    let inspect = |receipt_first: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("mir").arg(&main);
        if receipt_first {
            command.arg("--receipt").arg("--all");
        } else {
            command.arg("--all").arg("--receipt");
        }
        command
            .output()
            .expect("spawn transitive mixed-width receipt")
    };
    let receipt_first = inspect(false);
    let receipt_reordered = inspect(true);
    for (label, output) in [
        ("all-first", &receipt_first),
        ("receipt-first", &receipt_reordered),
    ] {
        assert!(
            output.status.success(),
            "{label} rejected mixed-width imported FFI graph:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let manifest = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(
            &String::from_utf8_lossy(&output.stdout),
        )
        .unwrap_or_else(|error| panic!("{label}: mixed-width receipt round-trip failed: {error}"));
        assert_eq!(manifest, checked);
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    assert_eq!(receipt_first.stdout, receipt_reordered.stdout);
    assert_eq!(receipt_first.stderr, receipt_reordered.stderr);

    let run = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("run");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&main)
            .output()
            .expect("spawn transitive mixed-width run")
    };
    let run_default = run(false);
    let run_mir = run(true);
    for (label, output) in [("default", &run_default), ("mir", &run_mir)] {
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} mixed-width imported FFI run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"15\n");
        assert!(output.stderr.is_empty());
    }
    assert_eq!(run_default.status.code(), run_mir.status.code());
    assert_eq!(run_default.stdout, run_mir.stdout);
    assert_eq!(run_default.stderr, run_mir.stderr);

    let default_binary = dir.join("transitive-mixed-width-default");
    let mir_binary = dir.join("transitive-mixed-width-mir");
    let build_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&main)
        .arg("-o")
        .arg(&default_binary)
        .output()
        .expect("spawn default transitive mixed-width build");
    let build_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&main)
        .arg("-o")
        .arg(&mir_binary)
        .output()
        .expect("spawn explicit transitive mixed-width build");
    for (label, output) in [("default", &build_default), ("mir", &build_mir)] {
        assert!(
            output.status.success(),
            "{label} mixed-width imported FFI build failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    let native_default = Command::new(&default_binary)
        .output()
        .expect("execute default transitive mixed-width binary");
    let native_mir = Command::new(&mir_binary)
        .output()
        .expect("execute explicit transitive mixed-width binary");
    for (label, output) in [("default", &native_default), ("mir", &native_mir)] {
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label} mixed-width imported FFI binary failed"
        );
        assert_eq!(output.stdout, b"15\n");
        assert!(output.stderr.is_empty());
    }

    let verify_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg(&main)
        .output()
        .expect("spawn default transitive mixed-width verifier");
    let verify_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("verify")
        .arg("--mir")
        .arg(&main)
        .output()
        .expect("spawn explicit transitive mixed-width verifier");
    for (label, output) in [("default", &verify_default), ("mir", &verify_mir)] {
        assert!(
            output.status.success(),
            "{label} mixed-width imported FFI verifier failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(combined.contains("No contracts to verify"));
        assert!(!combined.contains("canonical route disposition: legacy"));
    }

    fs::remove_file(&default_binary).ok();
    fs::remove_file(&mir_binary).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_scalar_ffi_transitive_failure_preserves_side_effect_order() {
    if !can_link() {
        eprintln!("SKIP: cc not available");
        return;
    }
    let dir = project_root().join("target").join(format!(
        "mimi-cli-transitive-scalar-ffi-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create transitive scalar FFI failure directory");
    fs::write(
        dir.join("leaf.mimi"),
        "extern \"C\" {\n    func srand(x: i32);\n    func putchar(x: i32) -> i32;\n    func labs(x: i64) -> i64 requires: x > 0;\n}\npub func seed(x: i32) { srand(x) }\npub func emit_marker(x: i32) -> i32 { putchar(x) }\npub func checked_labs(x: i64) -> i64 {\n    requires: x > 0\n    labs(x)\n}\n",
    )
    .expect("write transitive scalar FFI failure leaf");
    fs::write(
        dir.join("mid.mimi"),
        "use leaf;\npub func forward_seed(x: i32) { seed(x) }\npub func forward_marker(x: i32) -> i32 { emit_marker(x) }\npub func forward_labs(x: i64) -> i64 {\n    requires: x > 0\n    checked_labs(x)\n}\n",
    )
    .expect("write transitive scalar FFI failure middle module");
    let main = dir.join("main.mimi");
    fs::write(
        &main,
        "use mid;\nfunc main() -> i32 {\n    forward_seed(4)\n    forward_marker(52)\n    forward_labs(-1 as i64)\n    forward_seed(2)\n    0\n}\n",
    )
    .expect("write transitive scalar FFI failure entry");

    let checked = checked_route_receipt(&main);
    assert_eq!(
        checked.root_owners,
        vec![
            mimi::core::NodeId("function:checked_labs".into()),
            mimi::core::NodeId("function:emit_marker".into()),
            mimi::core::NodeId("function:forward_labs".into()),
            mimi::core::NodeId("function:forward_marker".into()),
            mimi::core::NodeId("function:forward_seed".into()),
            mimi::core::NodeId("function:main".into()),
            mimi::core::NodeId("function:seed".into()),
        ]
    );

    let run = |explicit_mir: bool| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("run");
        if explicit_mir {
            command.arg("--mir");
        }
        command
            .arg(&main)
            .output()
            .expect("spawn transitive scalar FFI failure run")
    };
    let run_default = run(false);
    let run_mir = run(true);
    for (label, output) in [("default", &run_default), ("mir", &run_mir)] {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{label} failure run returned the wrong status: {stderr}"
        );
        assert_eq!(
            output.stdout, b"4",
            "{label} failure run changed the side-effect prefix"
        );
        assert!(
            stderr.contains("[E0808]"),
            "{label} failure lost E0808: {stderr}"
        );
        assert!(!stderr.contains("canonical route disposition: legacy"));
    }
    assert_eq!(run_default.stdout, run_mir.stdout);
    assert_eq!(run_default.stderr, run_mir.stderr);
    assert_eq!(run_default.status.code(), run_mir.status.code());

    let default_binary = dir.join("transitive-scalar-failure-default");
    let mir_binary = dir.join("transitive-scalar-failure-mir");
    let build_default = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg(&main)
        .arg("-o")
        .arg(&default_binary)
        .output()
        .expect("spawn default transitive scalar FFI failure build");
    let build_mir = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("build")
        .arg("--mir")
        .arg(&main)
        .arg("-o")
        .arg(&mir_binary)
        .output()
        .expect("spawn explicit transitive scalar FFI failure build");
    for (label, output) in [("default", &build_default), ("mir", &build_mir)] {
        assert!(
            output.status.success(),
            "{label} failure build failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr)
            .contains("canonical route disposition: legacy"));
    }
    for (label, binary) in [("default", &default_binary), ("mir", &mir_binary)] {
        let output = Command::new(binary)
            .output()
            .expect("execute transitive scalar FFI failure binary");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{label} failure binary returned the wrong status: {stderr}"
        );
        assert_eq!(output.stdout, b"4");
        assert!(
            stderr.contains("[E0808]"),
            "{label} binary lost E0808: {stderr}"
        );
    }

    for explicit_mir in [false, true] {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg("verify");
        if explicit_mir {
            command.arg("--mir");
        }
        let output = command
            .arg(&main)
            .output()
            .expect("spawn transitive scalar FFI failure verifier");
        assert!(
            output.status.success(),
            "verifier unexpectedly rejected conditional helper preconditions:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(combined.contains("canonical MIR extern requires contract proven"));
        assert!(!combined.contains("canonical route disposition: legacy"));
    }

    fs::remove_file(&default_binary).ok();
    fs::remove_file(&mir_binary).ok();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn canonical_mir_transitive_failure_is_consumer_invariant_and_atomic() {
    let dir = project_root().join("target").join(format!(
        "mimi-cli-transitive-import-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create transitive failure directory");
    fs::write(
        dir.join("bad.mimi"),
        "pub func bad(xs: List<string>) -> string { xs[0] }\n",
    )
    .expect("write transitive unsupported helper");
    fs::write(
        dir.join("mid.mimi"),
        "use bad;\npub func mid() -> i32 { 1 }\n",
    )
    .expect("write transitive middle module");
    let main = dir.join("main.mimi");
    fs::write(&main, "use mid;\nfunc main() -> i32 { 0 }\n")
        .expect("write transitive failure entry");

    let binary = dir.join("transitive-consumer-output");
    let invoke = |consumer: &str| {
        let mut command = Command::new(mimi_bin());
        command.current_dir(project_root()).arg(consumer);
        if consumer == "build" {
            command.arg("--mir").arg(&main).arg("-o").arg(&binary);
        } else {
            command.arg("--mir").arg(&main);
        }
        command
            .output()
            .unwrap_or_else(|error| panic!("spawn transitive {consumer} failure: {error}"))
    };

    let mut outputs = Vec::new();
    for consumer in ["run", "build", "verify"] {
        let first = invoke(consumer);
        let second = invoke(consumer);
        let stage = match consumer {
            "verify" => "canonical MIR verifier input rejected",
            _ => "canonical MIR build error",
        };
        for (label, output) in [
            (format!("{consumer}-first"), &first),
            (format!("{consumer}-second"), &second),
        ] {
            assert!(
                !output.status.success(),
                "{label} silently selected source scope instead of closing the transitive graph"
            );
            assert!(
                output.stdout.is_empty(),
                "{label} emitted output before transitive lowering failed: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains(stage)
                    && stderr.contains("MIR lowering failed (1 errors)")
                    && stderr.contains("function:bad/node:expr.index")
                    && stderr.matches("Copy scalar").count() == 1
                    && !stderr.contains("function:mid/node:expr.index")
                    && !stderr.contains("canonical route disposition: legacy")
                    && !stderr.contains("flow_ast")
                    && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
                "{label} changed transitive failure classification: {stderr}"
            );
        }
        assert_eq!(first.status.code(), second.status.code());
        assert_eq!(first.stdout, second.stdout);
        assert_eq!(first.stderr, second.stderr);
        let stderr = String::from_utf8_lossy(&first.stderr);
        let aggregate = stderr
            .find("MIR lowering failed")
            .map(|index| stderr[index..].to_owned())
            .expect("transitive canonical lowering aggregate");
        outputs.push((consumer, aggregate));
    }
    let expected = &outputs[0].1;
    for (consumer, aggregate) in &outputs[1..] {
        assert_eq!(
            aggregate, expected,
            "{consumer} changed the transitive canonical lowering aggregate"
        );
    }
    assert!(
        !binary.exists(),
        "failed transitive build left a partial native output"
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
fn canonical_mir_cli_all_receipt_snapshot_includes_imported_instances() {
    let fixture = project_root()
        .join("tests")
        .join("real_world")
        .join("std_set.mimi");
    let run = || {
        Command::new(mimi_bin())
            .current_dir(project_root())
            .arg("mir")
            .arg(&fixture)
            .arg("--all")
            .arg("--receipt")
            .output()
            .expect("failed to spawn imported canonical MIR receipt")
    };

    let first = run();
    let second = run();
    assert!(
        first.status.success(),
        "mimi mir --all --receipt failed:\n{}\n{}",
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&first.stdout)
    );
    assert_eq!(
        first.stdout, second.stdout,
        "imported-instance receipt manifest must be deterministic"
    );
    let checked = checked_route_receipt(&fixture);
    let manifest_receipt = mimi::core::mir::CanonicalMirRouteReceipt::from_manifest(
        &String::from_utf8_lossy(&first.stdout),
    )
    .expect("imported-instance receipt must round-trip through the public API");
    assert_eq!(
        manifest_receipt, checked,
        "imported-instance receipt round-trip changed the checked receipt"
    );
    let checked_again = checked_route_receipt(&fixture);
    assert_eq!(
        checked_again, checked,
        "rebuilding the imported-instance checked receipt changed canonical identity"
    );
    let manifest = parse_route_receipt_manifest(&first.stdout);
    assert_eq!(
        manifest.get("schema").map(String::as_str),
        Some(mimi::core::mir::MIR_ROUTE_RECEIPT_SCHEMA)
    );
    assert_eq!(
        manifest.get("profile").map(String::as_str),
        Some("cli-mir-v1")
    );
    let root_owners = manifest.get("root_owners").expect("root owners field");
    assert!(
        root_owners.contains("function:mir:instance:function:set_insert"),
        "--all receipt omitted materialized Set instance: {root_owners}"
    );
    assert_eq!(
        manifest.len(),
        mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_FIELDS.len()
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        stderr.contains("lowered 7 callable(s) to canonical MIR"),
        "--all receipt lost imported callable diagnostic: {stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "--all receipt must not report a legacy disposition: {stderr}"
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
fn canonical_mir_cli_all_receipt_rejects_without_partial_manifest() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    let output = Command::new(mimi_bin())
        .current_dir(project_root())
        .arg("mir")
        .arg(&fixture)
        .arg("--all")
        .arg("--receipt")
        .output()
        .expect("failed to spawn rejected canonical MIR receipt");
    assert!(
        !output.status.success(),
        "unsupported receipt snapshot must fail closed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.is_empty(),
        "failed receipt must not emit a partial manifest: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MIR inspection input rejected") && stderr.contains("Copy scalar"),
        "unexpected receipt rejection diagnostic:\n{stderr}"
    );
    assert!(
        !stderr.contains("canonical route disposition: legacy")
            && !stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER),
        "failed receipt must not fall back or claim a manifest:\n{stderr}"
    );
}

#[test]
fn canonical_mir_rejection_diagnostics_are_stable_across_cli_entries() {
    let fixture = project_root()
        .join("tests")
        .join("fixtures")
        .join("mir_list_string_index_rejected.mimi");
    for command in ["run", "build", "verify", "mir"] {
        let mut invocation = Command::new(mimi_bin());
        invocation
            .current_dir(project_root())
            .arg(command)
            .arg(&fixture);
        if command == "mir" {
            invocation.arg("--all").arg("--receipt");
        } else {
            invocation.arg("--mir");
        }
        let output = invocation
            .output()
            .unwrap_or_else(|error| panic!("rejected {command} invocation: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "rejected {command} must fail closed"
        );
        assert!(
            stdout.is_empty(),
            "rejected {command} emitted stdout: {stdout}"
        );
        assert!(
            stderr.contains("Copy scalar"),
            "rejected {command}: {stderr}"
        );
        assert!(
            stderr.contains(match command {
                "mir" => "MIR inspection input rejected",
                "verify" => "canonical MIR verifier input rejected",
                _ => "canonical MIR build error",
            }),
            "rejected {command} lost its failure phase: {stderr}"
        );
        assert!(
            !stderr.contains("Validation(["),
            "rejected {command} leaked debug-shaped MIR error: {stderr}"
        );
        assert!(!stderr.contains("canonical route disposition: legacy"));
        assert!(!stderr.contains("flow_ast"));
        if command == "mir" {
            assert!(!stderr.contains(mimi::core::mir::MIR_ROUTE_RECEIPT_MANIFEST_HEADER));
        }
    }
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
    assert!(mir_stdout.contains("recoverable_boundary"));
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
    assert!(mir_stdout.contains("recoverable_boundary"));
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
    assert!(mir_stdout.contains("recoverable_boundary"), "{mir_stdout}");
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
    assert!(mir_stdout.contains("recoverable_boundary"));
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
fn canonical_mir_cli_rejects_ffi_declaration_boundaries_without_fallback() {
    let dir = std::env::temp_dir().join(format!(
        "mimi_ffi_boundary_cli_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create FFI boundary CLI fixture directory");
    let fixtures = [
        (
            "non_c_abi",
            r#"extern "Rust" { func foreign(value: i64) -> i64; }
func main() -> i64 { foreign(42 as i64) }
"#,
            "ABI 'Rust' is outside the canonical C ABI",
        ),
        (
            "no_panic",
            r#"#[no_panic]
extern "C" { func foreign(value: i64) -> i64; }
func main() -> i64 { foreign(42 as i64) }
"#,
            "unsupported no_panic FFI protection semantics",
        ),
        (
            "variadic",
            r#"extern "C" { func foreign(value: i64 ...) -> i64; }
func main() -> i64 { foreign(42 as i64) }
"#,
            "unsupported variadic ABI semantics in canonical scalar FFI",
        ),
    ];

    for (label, source_text, boundary) in fixtures {
        let source = dir.join(format!("{label}.mimi"));
        fs::write(&source, source_text).expect("write FFI boundary CLI fixture");
        for command in ["run", "build", "verify"] {
            let output = Command::new(mimi_bin())
                .current_dir(project_root())
                .arg(command)
                .arg(&source)
                .arg("--mir")
                .output()
                .unwrap_or_else(|error| panic!("{label} {command}: {error}"));
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "{label} {command} must reject an unmigrated declaration boundary"
            );
            assert!(stdout.is_empty(), "{label} {command}: {stdout}");
            assert!(stderr.contains(boundary), "{label} {command}: {stderr}");
            assert!(
                stderr.contains("MIR validation failed"),
                "{label} {command}: {stderr}"
            );
            assert!(
                !stderr.contains("Validation(["),
                "{label} {command} leaked debug-shaped MIR error: {stderr}"
            );
            assert!(
                !stderr.contains("canonical route disposition: legacy"),
                "{label} {command} leaked a legacy route: {stderr}"
            );
            assert!(!stderr.contains("flow_ast"), "{label} {command}: {stderr}");
        }
    }
    fs::remove_dir_all(&dir).expect("remove FFI boundary CLI fixture directory");
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
