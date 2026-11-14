#![cfg(unix)]

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROFILE: &str = "map-any-owner-v2-dynamic-keys-get-v1";
const FIXTURE_NAME: &str = "mir_ordinary_map_any_owner_v2_dynamic_keys_get.mimi";
const EXPECTED_PREFIX_OUTPUT: &[u8] = b"pr\xc3\xa9\0\xe9\x9b\xaa\ntrue\n";
const EXPECTED_EMPTY_ARGUMENT_OUTPUT: &[u8] = b"\0\xe9\x9b\xaa\ntrue\n";
const VALGRIND_ERROR_EXIT_CODE: i32 = 97;
const SOURCE_MANIFEST_PATHS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "src/core/cfg/resource_lower.rs",
    "src/core/mir/eligibility.rs",
    "src/core/mir/islands.rs",
    "src/core/mir/lower.rs",
    "src/core/mir/managed_any.rs",
    "src/core/mir/mod.rs",
    "src/core/mir/receipt.rs",
    "src/core/mir/reference.rs",
    "src/core/mir/route.rs",
    "src/core/mir/types.rs",
    "src/core/ownership.rs",
    "src/interp/bytecode/disasm.rs",
    "src/interp/bytecode/instr.rs",
    "src/interp/bytecode/mir.rs",
    "src/interp/bytecode/vm.rs",
    "src/codegen/mir/eligibility.rs",
    "src/codegen/mir/mod.rs",
    "src/codegen/mir/ordinary_map_owned_any_v2.rs",
    "src/codegen/mir/ownership.rs",
    "src/codegen/mir/scalar.rs",
    "src/runtime/env.rs",
    "src/runtime/any_owner.rs",
    "src/runtime/managed_any_payload_v2.rs",
    "src/runtime/map_any_snapshot.rs",
    "src/runtime/mod.rs",
    "src/runtime/ordinary_map_domain.rs",
    "src/main/build.rs",
    "src/main/canonical_dispatch.rs",
    "src/main/disasm_cmd.rs",
    "src/main/run.rs",
    "src/main/test.rs",
    "src/main/verify.rs",
    "src/verifier/mir_capability.rs",
    "src/verifier/mod.rs",
    "scripts/verify-ordinary-map-owned-any-v2-dynamic-keys-get-l3.sh",
    "tests/canonical_mir_ordinary_map_any_v2_dynamic_keys_get.rs",
    "tests/canonical_mir_ordinary_map_any_v2_dynamic_keys_get_memcheck_l3.rs",
    "tests/support/memcheck_summary.rs",
    "tests/fixtures/mir_ordinary_map_any_owner_v2_dynamic_keys_get.mimi",
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
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .expect("Cargo must provide mimi for DynamicKeysGet native Memcheck")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(FIXTURE_NAME)
}

fn create_directory(path: &Path) {
    fs::create_dir(path).unwrap_or_else(|error| {
        panic!(
            "create fresh DynamicKeysGet L3 directory {}: {error}",
            path.display()
        )
    });
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

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn save_command(directory: &Path, label: &str, command: &mut Command) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run {label}: {error}"));
    write_new(&directory.join(format!("{label}.stdout")), &output.stdout);
    write_new(&directory.join(format!("{label}.stderr")), &output.stderr);
    write_new(
        &directory.join(format!("{label}.status")),
        format!(
            "exit_code={}\nsuccess={}\n",
            output.status.code().unwrap_or(-1),
            output.status.success()
        )
        .as_bytes(),
    );
    output
}

fn route_identity(output: &Output, mode: &str) -> (String, String) {
    let stderr = output_text(&output.stderr);
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&format!("route-admission consumer=build mode={mode}")))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "one exact {mode} route census row: {stderr}"
    );
    let line = matching[0];
    assert!(
        line.contains(&format!("checker_profile={PROFILE}")),
        "{line}"
    );
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("route census lacks {name}: {line}"))
            .to_owned()
    };
    (field("mir_digest"), field("receipt_identity_digest"))
}

fn source_hash(source: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(source)
        .output()
        .expect("sha256sum must be available for source-bound L3 evidence");
    assert!(
        output.status.success(),
        "sha256sum failed: {}",
        output_text(&output.stderr)
    );
    output_text(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum output contains digest")
        .to_owned()
}

fn route_source_manifest() -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new("sha256sum");
    for path in SOURCE_MANIFEST_PATHS {
        command.arg(root.join(path));
    }
    let output = command
        .output()
        .expect("sha256sum must be available for source manifest evidence");
    assert!(
        output.status.success(),
        "hash DynamicKeysGet route sources: {}",
        output_text(&output.stderr)
    );
    output.stdout
}

fn strict_valgrind() -> PathBuf {
    assert_eq!(
        std::env::var("MIMI_REQUIRE_VALGRIND").as_deref(),
        Ok("1"),
        "strict DynamicKeysGet L3 requires MIMI_REQUIRE_VALGRIND=1"
    );
    let configured = std::env::var_os("MIMI_L3_VALGRIND")
        .expect("MIMI_L3_VALGRIND must name the resolved Valgrind executable");
    let configured = PathBuf::from(configured);
    assert!(configured.is_absolute(), "Valgrind path must be absolute");
    let canonical = fs::canonicalize(&configured)
        .unwrap_or_else(|error| panic!("resolve Valgrind {}: {error}", configured.display()));
    assert_eq!(configured, canonical, "Valgrind path must be canonical");
    let metadata = fs::metadata(&canonical).expect("inspect resolved Valgrind");
    assert!(metadata.is_file(), "Valgrind must be a regular file");
    assert_ne!(
        metadata.permissions().mode() & 0o111,
        0,
        "Valgrind must be executable"
    );
    canonical
}

fn run_memcheck(
    valgrind: &Path,
    binary: &Path,
    argument: &str,
    expected_stdout: &[u8],
    directory: &Path,
    label: &str,
) -> memcheck_summary::MemcheckSummary {
    create_directory(directory);
    let home = directory.join("home");
    create_directory(&home);
    let log = directory.join("valgrind.log");
    let mut command = Command::new(valgrind);
    command
        .current_dir(directory)
        .env("LC_ALL", "C")
        .env("HOME", &home)
        .env_remove("VALGRIND_OPTS")
        .args([
            OsString::from("--default-suppressions=yes"),
            OsString::from("--leak-check=full"),
            OsString::from("--show-leak-kinds=all"),
            OsString::from("--errors-for-leak-kinds=definite,indirect,possible,reachable"),
            OsString::from(format!("--error-exitcode={VALGRIND_ERROR_EXIT_CODE}")),
            OsString::from(format!("--log-file={}", log.display())),
            binary.as_os_str().to_owned(),
            OsString::from(argument),
        ]);
    let output = save_command(directory, label, &mut command);
    let report = fs::read(&log)
        .unwrap_or_else(|error| panic!("read strict Memcheck log {}: {error}", log.display()));
    write_new(&directory.join("valgrind.raw.log"), &report);
    let report_text = output_text(&report);
    assert!(
        report_text.ends_with('\n'),
        "truncated Memcheck report: {report_text}"
    );
    let summary = memcheck_summary::parse(&report_text);
    write_new(
        &directory.join("memcheck-summary.txt"),
        summary.rendered.as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "native Bool result changed or Memcheck found a defect:\n{report_text}\n{}",
        output_text(&output.stderr)
    );
    assert_eq!(
        output.stdout, expected_stdout,
        "native output bytes for {label}"
    );
    assert!(
        output.stderr.is_empty(),
        "native stderr for {label}: {}",
        output_text(&output.stderr)
    );
    assert_eq!(
        summary.still_reachable_bytes, 0,
        "{label}: {}",
        summary.rendered
    );
    assert_eq!(
        summary.still_reachable_blocks, 0,
        "{label}: {}",
        summary.rendered
    );
    write_new(
        &directory.join("validation.status"),
        format!(
            "validation=PASS\nprofile={PROFILE}\nmode={label}\nexpected_exit_code=1\nobserved_exit_code={}\nstdout_hex={}\nvalgrind_error_exit_code={VALGRIND_ERROR_EXIT_CODE}\nvalgrind_errors=0\nstill_reachable_bytes={}\nstill_reachable_blocks={}\n",
            output.status.code().unwrap_or(-1),
            expected_stdout.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            summary.still_reachable_bytes,
            summary.still_reachable_blocks,
        )
        .as_bytes(),
    );
    summary
}

#[test]
#[ignore = "strict DynamicKeysGet native Memcheck L3; require MIMI_REQUIRE_VALGRIND=1"]
fn ordinary_map_owned_any_v2_dynamic_keys_get_default_and_explicit_are_memcheck_clean() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "DynamicKeysGet must not open global owner-v2 production readiness"
        );
    };
    let artifact_root = std::env::var_os("MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR")
        .map(PathBuf::from)
        .expect("strict L3 requires MIMI_MAP_DYNAMIC_KEYS_GET_L3_ARTIFACT_DIR");
    assert!(
        artifact_root.is_absolute(),
        "artifact path must be absolute"
    );
    create_directory(&artifact_root);
    let native_root = artifact_root.join("native");
    create_directory(&native_root);
    let valgrind = strict_valgrind();
    let mimi = mimi_bin();
    let source = fixture();
    let bytes = fs::read(&source).expect("read exact DynamicKeysGet fixture");
    write_new(&native_root.join("source.fixture.mimi"), &bytes);
    let digest = source_hash(&source);
    let source_manifest_before = route_source_manifest();
    write_new(
        &native_root.join("source-manifest.before.sha256"),
        &source_manifest_before,
    );
    write_new(
        &native_root.join("source.sha256"),
        format!("{digest}  {}\n", source.display()).as_bytes(),
    );
    write_new(
        &native_root.join("tool-and-contract.txt"),
        format!(
            "profile={PROFILE}\nmimi={}\nvalgrind={}\nsource_sha256={digest}\nMIMI_REQUIRE_VALGRIND=1\n",
            mimi.display(),
            valgrind.display(),
        )
        .as_bytes(),
    );

    let mut route_identities = Vec::new();
    let mut reachable = Vec::new();
    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let mode_root = native_root.join(mode);
        create_directory(&mode_root);
        let binary = mode_root.join("program");
        let mut build = Command::new(&mimi);
        build
            .env("LC_ALL", "C")
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .env(
                "MIMI_STDLIB",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("std"),
            )
            .arg("build")
            .arg(&source)
            .arg("--output")
            .arg(&binary);
        if explicit_mir {
            build.arg("--mir");
        }
        let build_dir = mode_root.join("build");
        create_directory(&build_dir);
        build.current_dir(&build_dir);
        let build_output = save_command(&mode_root, "build", &mut build);
        assert!(
            build_output.status.success(),
            "{mode} build failed: stdout={} stderr={}",
            output_text(&build_output.stdout),
            output_text(&build_output.stderr)
        );
        assert!(binary.is_file(), "{mode} build omitted native artifact");
        let identity = route_identity(&build_output, mode);
        route_identities.push(identity);
        write_new(
            &mode_root.join("source.sha256"),
            format!("{digest}  {}\n", source.display()).as_bytes(),
        );
        for (argument, expected, label) in [
            ("pré", EXPECTED_PREFIX_OUTPUT, "utf8-prefix"),
            ("", EXPECTED_EMPTY_ARGUMENT_OUTPUT, "empty-argv-value"),
        ] {
            let summary = run_memcheck(
                &valgrind,
                &binary,
                argument,
                expected,
                &mode_root.join(format!("memcheck-{label}")),
                &format!("{mode}-{label}"),
            );
            reachable.push((
                summary.still_reachable_bytes,
                summary.still_reachable_blocks,
            ));
        }
    }
    assert_eq!(
        route_identities[0], route_identities[1],
        "default and explicit MIR native builds used different DynamicKeysGet receipts"
    );
    assert!(
        reachable.iter().all(|counts| *counts == (0, 0)),
        "all strict Memcheck runs must settle every owned String and Map root"
    );
    let source_manifest_after = route_source_manifest();
    write_new(
        &native_root.join("source-manifest.after.sha256"),
        &source_manifest_after,
    );
    assert_eq!(
        source_manifest_before, source_manifest_after,
        "the DynamicKeysGet source manifest changed during strict L3"
    );
    write_new(
        &native_root.join("run-summary.txt"),
        format!(
            "profile={PROFILE}\nglobal_map_any_v2_production_ready=false\nsource_sha256={digest}\ndefault_mir_digest={}\ndefault_route_identity_digest={}\nexplicit_mir_digest={}\nexplicit_route_identity_digest={}\nmemcheck_run_count={}\nstill_reachable_bytes=0\nstill_reachable_blocks=0\n",
            route_identities[0].0,
            route_identities[0].1,
            route_identities[1].0,
            route_identities[1].1,
            reachable.len(),
        )
        .as_bytes(),
    );
}
