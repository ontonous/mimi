#![cfg(unix)]

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROFILE: &str = "map-any-owner-v2-removed-key-checked-miss-v1";
const FIXTURE_NAME: &str = "mir_ordinary_map_any_owner_v2_removed_key_checked_miss.mimi";
const STDLIB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/std");
const EXPECTED_STDOUT: &[u8] = b"";
const EXPECTED_EXIT_CODE: i32 = 1;
const VALGRIND_ERROR_EXIT_CODE: i32 = 97;
const VALGRIND_WRAPPER_BODY: &[&str] = &[
    "DBGPATH=/usr/lib/debug",
    "if [ -z \"$LD_LIBRARY_PATH\" ]; then",
    "export LD_LIBRARY_PATH=\"$DBGPATH\"",
    "else",
    "export LD_LIBRARY_PATH=\"$LD_LIBRARY_PATH:$DBGPATH\"",
    "fi",
    "export GLIBCPP_FORCE_NEW=1",
    "export GLIBCXX_FORCE_NEW=1",
    "exec $0.bin \"$@\"",
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
        .expect("Cargo must provide mimi for RemovedKeyCheckedMiss native Memcheck evidence")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(FIXTURE_NAME)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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

fn create_capture_directory(path: &Path) {
    fs::create_dir(path).unwrap_or_else(|error| {
        panic!(
            "create fresh evidence directory {} without replacing prior evidence: {error}",
            path.display()
        )
    });
}

fn evidence_root() -> PathBuf {
    let configured = std::env::var_os("MIMI_MAP_V2_REMOVED_KEY_CHECKED_MISS_L3_ARTIFACT_DIR")
        .filter(|value| !value.is_empty())
        .expect("strict RemovedKeyCheckedMiss L3 requires a retained artifact directory");
    let configured = PathBuf::from(configured);
    let root = if configured.is_absolute() {
        configured
    } else {
        std::env::current_dir()
            .expect("resolve current directory for RemovedKeyCheckedMiss artifacts")
            .join(configured)
    };
    if !root.exists() {
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent).expect("create parent for retained L3 artifacts");
        }
        fs::create_dir(&root).expect("create fresh RemovedKeyCheckedMiss L3 artifact directory");
    }
    fs::canonicalize(&root).expect("canonicalize RemovedKeyCheckedMiss L3 artifact directory")
}

fn required_valgrind() -> (PathBuf, PathBuf) {
    let configured = std::env::var_os("MIMI_L3_VALGRIND")
        .expect("MIMI_L3_VALGRIND must identify the resolved Valgrind executable");
    let configured = PathBuf::from(configured);
    assert!(
        configured.is_absolute(),
        "MIMI_L3_VALGRIND must be absolute"
    );
    let canonical = fs::canonicalize(&configured)
        .unwrap_or_else(|error| panic!("resolve Valgrind {}: {error}", configured.display()));
    assert_eq!(configured, canonical, "Valgrind path must be canonical");
    let metadata = fs::metadata(&canonical).expect("inspect resolved Valgrind");
    assert!(metadata.is_file(), "Valgrind is not a regular file");
    assert_ne!(
        metadata.permissions().mode() & 0o111,
        0,
        "Valgrind is not executable"
    );

    let mut file = File::open(&canonical).expect("open resolved Valgrind");
    let mut magic = [0_u8; 4];
    let read = file
        .read(&mut magic)
        .expect("read Valgrind executable header");
    if read == magic.len() && magic == *b"\x7fELF" {
        return (canonical.clone(), canonical);
    }

    let bytes = fs::read(&canonical).expect("read Valgrind wrapper");
    let wrapper = std::str::from_utf8(&bytes).expect("Valgrind wrapper is UTF-8");
    assert_eq!(
        wrapper.lines().next(),
        Some("#!/bin/sh -e"),
        "refusing an unrecognized Valgrind script shim"
    );
    let body = wrapper
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(
        body.as_slice(),
        VALGRIND_WRAPPER_BODY,
        "refusing an unrecognized Valgrind script shim"
    );
    let file_name = canonical
        .file_name()
        .expect("Valgrind wrapper has a filename")
        .to_string_lossy();
    let companion = fs::canonicalize(canonical.with_file_name(format!("{file_name}.bin")))
        .expect("resolve Valgrind wrapper companion");
    let companion_metadata = fs::metadata(&companion).expect("inspect Valgrind companion");
    assert!(
        companion_metadata.is_file(),
        "Valgrind companion is not a file"
    );
    assert_ne!(
        companion_metadata.permissions().mode() & 0o111,
        0,
        "Valgrind companion is not executable"
    );
    let companion_path = companion;
    let mut companion = File::open(&companion_path).expect("open Valgrind companion");
    let mut companion_magic = [0_u8; 4];
    companion
        .read_exact(&mut companion_magic)
        .expect("read Valgrind companion executable header");
    assert_eq!(
        companion_magic, *b"\x7fELF",
        "Valgrind companion is not ELF"
    );
    (canonical, companion_path)
}

fn record_output(directory: &Path, command: &str, args: &[OsString], output: &Output) {
    let mut invocation = format!("program={command}\n");
    for (index, argument) in args.iter().enumerate() {
        invocation.push_str(&format!("arg{index}={}\n", argument.to_string_lossy()));
    }
    write_new(&directory.join("command.txt"), invocation.as_bytes());
    write_new(&directory.join("stdout.bin"), &output.stdout);
    write_new(&directory.join("stderr.bin"), &output.stderr);
    let status = match output.status.code() {
        Some(code) => format!("exit_code={code}\nsuccess={}\n", output.status.success()),
        None => format!(
            "exit_code=signal\nsuccess=false\nstatus={:?}\n",
            output.status
        ),
    };
    write_new(&directory.join("process.status"), status.as_bytes());
}

fn run_recorded(
    command: &Path,
    args: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
) -> Output {
    let mut invocation = Command::new(command);
    invocation.args(args);
    invocation.current_dir(directory);
    invocation.env_remove("VALGRIND_OPTS");
    for (key, value) in environment {
        invocation.env(key, value);
    }
    match invocation.output() {
        Ok(output) => {
            record_output(directory, &command.to_string_lossy(), args, &output);
            write_new(
                &directory.join("command.environment.txt"),
                environment
                    .iter()
                    .map(|(key, value)| format!("{key}={value}\n"))
                    .chain([
                        "VALGRIND_OPTS=<unset>\n".to_owned(),
                        format!("cwd={}\n", directory.display()),
                    ])
                    .collect::<String>()
                    .as_bytes(),
            );
            output
        }
        Err(error) => {
            write_new(
                &directory.join("command.txt"),
                format!("program={}\nspawn_error={error}\n", command.display()).as_bytes(),
            );
            write_new(&directory.join("stdout.bin"), b"");
            write_new(
                &directory.join("stderr.bin"),
                format!("failed to start command: {error}\n").as_bytes(),
            );
            write_new(
                &directory.join("process.status"),
                format!("spawn_error={error}\nsuccess=false\n").as_bytes(),
            );
            panic!(
                "start {} for strict L3 evidence: {error}",
                command.display()
            );
        }
    }
}

fn assert_success(output: &Output, mode: &str) {
    assert!(
        output.status.success(),
        "{mode} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn parse_route_identity(output: &Output, expected_mode: &str) -> (String, String) {
    assert_success(output, &format!("{expected_mode} build"));
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{expected_mode} build selected a legacy route:\n{stderr}"
    );
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{expected_mode} build route census:\n{stderr}"
    );
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
        .expect("route-admission prefix")
        .split_whitespace()
        .map(|field| {
            field
                .split_once('=')
                .unwrap_or_else(|| panic!("malformed route field {field:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&"build"));
    assert_eq!(fields.get("mode"), Some(&expected_mode));
    assert_eq!(fields.get("checker_profile"), Some(&PROFILE));
    let mir_digest = *fields.get("mir_digest").expect("route MIR digest");
    let route_digest = *fields
        .get("receipt_identity_digest")
        .expect("route identity digest");
    for digest in [mir_digest, route_digest] {
        assert!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "malformed route digest {digest}"
        );
    }
    (mir_digest.to_string(), route_digest.to_string())
}

fn assert_native_build_receipt(output: &Output, identity: &(String, String), mode: &str) {
    let stderr = text(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{mode} build native receipt:\n{stderr}");
    let fields = lines[0]
        .strip_prefix("MIMI_ROUTE_CENSUS canonical-native-build ")
        .expect("native build receipt prefix")
        .split_whitespace()
        .map(|field| {
            field
                .split_once('=')
                .unwrap_or_else(|| panic!("malformed native receipt field {field:?}"))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(fields.get("consumer"), Some(&mode));
    assert_eq!(fields.get("route_id"), Some(&PROFILE));
    assert_eq!(fields.get("profile"), Some(&PROFILE));
    assert_eq!(fields.get("mir_digest"), Some(&identity.0.as_str()));
    assert_eq!(
        fields.get("route_identity_digest"),
        Some(&identity.1.as_str())
    );
}

fn strict_memcheck(
    valgrind: &Path,
    binary: &Path,
    directory: &Path,
    mode: &str,
) -> memcheck_summary::MemcheckSummary {
    let isolated_home = directory.join("valgrind-home");
    create_capture_directory(&isolated_home);
    assert!(
        !directory.join(".valgrindrc").exists(),
        "fresh Memcheck working directory unexpectedly contains .valgrindrc"
    );
    assert!(
        !isolated_home.join(".valgrindrc").exists(),
        "fresh isolated HOME unexpectedly contains .valgrindrc"
    );
    let isolated_home_text = isolated_home
        .to_str()
        .expect("isolated Valgrind HOME path must be UTF-8");
    write_new(
        &directory.join("valgrind-config-isolation.txt"),
        format!(
            "VALGRIND_OPTS=<unset>\nHOME={isolated_home_text}\nHOME/.valgrindrc=absent-at-start\nworking_directory={}\nworking_directory/.valgrindrc=absent-at-start\n",
            directory.display()
        )
        .as_bytes(),
    );
    let log = directory.join("valgrind.log");
    let args = vec![
        OsString::from("--default-suppressions=yes"),
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible,reachable"),
        OsString::from(format!("--error-exitcode={VALGRIND_ERROR_EXIT_CODE}")),
        OsString::from(format!("--log-file={}", log.display())),
        binary.as_os_str().to_owned(),
    ];
    let environment = [("LC_ALL", "C"), ("HOME", isolated_home_text)];
    let output = run_recorded(valgrind, &args, &environment, directory);
    let report = fs::read(&log)
        .unwrap_or_else(|error| panic!("read strict Memcheck report {}: {error}", log.display()));
    write_new(&directory.join("valgrind.raw.log"), &report);
    let report_text = text(&report);
    assert!(
        report_text.ends_with('\n'),
        "{mode} Valgrind report is truncated:\n{report_text}"
    );
    let summary = memcheck_summary::parse(&report_text);
    write_new(
        &directory.join("memcheck-summary.txt"),
        summary.rendered.as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(EXPECTED_EXIT_CODE),
        "{mode} native result changed or Memcheck found an error:\n{report_text}\n{}",
        text(&output.stderr)
    );
    assert_eq!(output.stdout, EXPECTED_STDOUT, "{mode} native stdout");
    assert!(
        output.stderr.is_empty(),
        "{mode} native stderr: {}",
        text(&output.stderr)
    );
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
    write_new(
        &directory.join("validation.status"),
        format!(
            "validation=PASS\nexpected_native_exit_code={EXPECTED_EXIT_CODE}\nobserved_native_exit_code={}\nvalgrind_memory_error_exit_code={VALGRIND_ERROR_EXIT_CODE}\nvalgrind_error_summary=0\nstill_reachable_bytes={}\nstill_reachable_blocks={}\n",
            output.status.code().expect("validated native exit code"),
            summary.still_reachable_bytes,
            summary.still_reachable_blocks,
        )
        .as_bytes(),
    );
    summary
}

#[test]
#[ignore = "strict RemovedKeyCheckedMiss native Memcheck L3; run scripts/verify-ordinary-map-v2-removed-key-checked-miss.sh"]
fn ordinary_map_owned_any_v2_removed_key_checked_miss_native_builds_are_strict_memcheck_clean() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the narrow RemovedKeyCheckedMiss slice must not open global V2 readiness"
        );
    };
    let root = evidence_root();
    let native_root = root.join("native");
    create_capture_directory(&native_root);
    let (valgrind, valgrind_binary) = required_valgrind();
    write_new(
        &native_root.join("tool-identity.txt"),
        format!(
            "valgrind_path={}\nvalgrind_binary_path={}\nmimi_path={}\nprofile={}\n",
            valgrind.display(),
            valgrind_binary.display(),
            mimi_bin().display(),
            PROFILE
        )
        .as_bytes(),
    );

    let source = fixture();
    let source_bytes = fs::read(&source)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", source.display()));
    write_new(&native_root.join("source.fixture.mimi"), &source_bytes);
    let mimi = mimi_bin();
    let mut identities = Vec::new();
    let mut summaries = Vec::new();

    for explicit_mir in [false, true] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let case_root = native_root.join(mode);
        create_capture_directory(&case_root);
        let build_root = case_root.join("build");
        create_capture_directory(&build_root);
        let binary = build_root.join("program");
        let mut args = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if explicit_mir {
            args.push(OsString::from("--mir"));
        }
        let mode_name = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let build_env = [
            ("MIMI_ROUTE_CENSUS", "1"),
            ("MIMI_VERBOSE", "1"),
            ("MIMI_STDLIB", STDLIB),
        ];
        let build = run_recorded(&mimi, &args, &build_env, &build_root);
        let identity = parse_route_identity(&build, mode_name);
        assert_native_build_receipt(&build, &identity, mode_name);
        assert!(binary.is_file(), "{mode} build produced no native binary");
        assert_ne!(
            fs::metadata(&binary)
                .expect("inspect native binary")
                .permissions()
                .mode()
                & 0o111,
            0,
            "{mode} native binary is not executable"
        );

        let memcheck_root = case_root.join("memcheck");
        create_capture_directory(&memcheck_root);
        let summary = strict_memcheck(&valgrind, &binary, &memcheck_root, mode);
        identities.push(identity);
        summaries.push(summary);
    }

    assert_eq!(
        identities[0], identities[1],
        "default and explicit MIR native builds used different route identities"
    );
    assert_eq!(summaries.len(), 2);
    write_new(
        &native_root.join("run-summary.txt"),
        format!(
            "route_profile={PROFILE}\nglobal_map_any_v2_production_ready=false\nnative_build_count=2\nmemcheck_run_count=2\nmir_digest={}\nroute_identity_digest={}\ndefault_still_reachable_bytes={}\ndefault_still_reachable_blocks={}\nexplicit_still_reachable_bytes={}\nexplicit_still_reachable_blocks={}\n",
            identities[0].0,
            identities[0].1,
            summaries[0].still_reachable_bytes,
            summaries[0].still_reachable_blocks,
            summaries[1].still_reachable_bytes,
            summaries[1].still_reachable_blocks,
        )
        .as_bytes(),
    );
    const {
        assert!(!mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY);
    };
}
