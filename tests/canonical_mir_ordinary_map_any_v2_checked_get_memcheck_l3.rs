use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "map-any-owner-v2-checked-get-v1";
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
const RECEIPT_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];

struct Case {
    label: &'static str,
    fixture: &'static str,
    expected_stdout: &'static [u8],
    expected_exit: i32,
}

const CASES: &[Case] = &[
    Case {
        label: "hit-i32",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_hit_i32.mimi",
        expected_stdout: b"",
        expected_exit: 1,
    },
    Case {
        label: "hit-bool",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_hit_bool.mimi",
        expected_stdout: b"",
        expected_exit: 1,
    },
    Case {
        label: "miss-i32",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_miss_i32.mimi",
        expected_stdout: b"",
        expected_exit: 0,
    },
    Case {
        label: "hit-string",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_hit_string.mimi",
        expected_stdout: b"",
        expected_exit: 1,
    },
    Case {
        label: "hit-label",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_hit_label.mimi",
        expected_stdout: b"",
        expected_exit: 1,
    },
    Case {
        label: "settled-branch-tail-i32",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_settled_branch_tail.mimi",
        expected_stdout: b"",
        expected_exit: 1,
    },
    Case {
        label: "multi-lookup",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_multi_lookup.mimi",
        expected_stdout: b"",
        expected_exit: 1,
    },
    Case {
        label: "multi-lookup-reversed",
        fixture: "mir_ordinary_map_any_owner_v2_checked_get_multi_lookup_reversed.mimi",
        expected_stdout: b"",
        expected_exit: 1,
    },
];

fn mimi_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mimi"))
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn required_elf_tool(variable: &str) -> PathBuf {
    let configured = std::env::var_os(variable)
        .unwrap_or_else(|| panic!("{variable} must name the pinned L3 tool executable"));
    let configured = PathBuf::from(configured);
    assert!(
        configured.is_absolute(),
        "{variable} must be an absolute path"
    );
    let canonical = fs::canonicalize(&configured)
        .unwrap_or_else(|error| panic!("resolve {variable} {}: {error}", configured.display()));
    assert_eq!(
        configured, canonical,
        "{variable} must already be the real path, without a PATH shim or symlink"
    );
    let metadata = fs::metadata(&canonical)
        .unwrap_or_else(|error| panic!("inspect {variable} {}: {error}", canonical.display()));
    assert!(metadata.is_file(), "{variable} is not a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            metadata.permissions().mode() & 0o111,
            0,
            "{variable} is not executable"
        );
    }
    let mut file = fs::File::open(&canonical)
        .unwrap_or_else(|error| panic!("open {variable} {}: {error}", canonical.display()));
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)
        .unwrap_or_else(|error| panic!("read {variable} executable header: {error}"));
    assert_eq!(
        magic, *b"\x7fELF",
        "{variable} is not an ELF executable; refusing a script shim"
    );
    canonical
}

fn required_valgrind_tool() -> (PathBuf, PathBuf) {
    let configured = std::env::var_os("MIMI_L3_VALGRIND")
        .expect("MIMI_L3_VALGRIND must name the resolved Valgrind executable");
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            metadata.permissions().mode() & 0o111,
            0,
            "Valgrind is not executable"
        );
    }
    let bytes = fs::read(&canonical).expect("read resolved Valgrind executable");
    if bytes.starts_with(b"\x7fELF") {
        return (canonical.clone(), canonical);
    }
    let wrapper = std::str::from_utf8(&bytes)
        .expect("non-ELF Valgrind must be the recognized Debian wrapper");
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
    let name = canonical
        .file_name()
        .expect("Valgrind wrapper has a filename")
        .to_string_lossy();
    let companion = canonical.with_file_name(format!("{name}.bin"));
    let companion = fs::canonicalize(&companion)
        .unwrap_or_else(|error| panic!("resolve Valgrind companion binary: {error}"));
    let companion_metadata = fs::metadata(&companion).expect("inspect Valgrind companion binary");
    assert!(
        companion_metadata.is_file(),
        "Valgrind companion is not a file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            companion_metadata.permissions().mode() & 0o111,
            0,
            "Valgrind companion is not executable"
        );
    }
    let companion_bytes = fs::read(&companion).expect("read Valgrind companion binary");
    assert!(
        companion_bytes.starts_with(b"\x7fELF"),
        "Valgrind wrapper companion is not ELF"
    );
    (canonical, companion)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn json_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character if character.is_control() => {
                encoded.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}

fn command_json(program: &OsStr, arguments: &[OsString]) -> String {
    let mut values = Vec::with_capacity(arguments.len() + 1);
    values.push(json_string(&program.to_string_lossy()));
    values.extend(
        arguments
            .iter()
            .map(|argument| json_string(&argument.to_string_lossy())),
    );
    format!("[{}]\n", values.join(", "))
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

fn process_status(output: &Output) -> String {
    match output.status.code() {
        Some(code) => format!("exit_code={code}\nsuccess={}\n", output.status.success()),
        None => format!(
            "exit_code=signal\nsuccess={}\nstatus={:?}\n",
            output.status.success(),
            output.status
        ),
    }
}

fn retain_command_output(
    directory: &Path,
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    output: &Output,
) {
    write_new(
        &directory.join("command.argv.json"),
        command_json(program, arguments).as_bytes(),
    );
    write_new(&directory.join("stdout.bin"), &output.stdout);
    write_new(&directory.join("stderr.bin"), &output.stderr);
    write_new(
        &directory.join("process.status"),
        process_status(output).as_bytes(),
    );
    let environment = environment
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect::<String>();
    write_new(
        &directory.join("command.environment.txt"),
        environment.as_bytes(),
    );
}

fn run_recorded(
    program: &OsStr,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
) -> Output {
    let mut command = Command::new(program);
    command.args(arguments);
    for (key, value) in environment {
        command.env(key, value);
    }
    match command.output() {
        Ok(output) => {
            retain_command_output(directory, program, arguments, environment, &output);
            output
        }
        Err(error) => {
            write_new(
                &directory.join("command.argv.json"),
                command_json(program, arguments).as_bytes(),
            );
            write_new(&directory.join("stdout.bin"), b"");
            write_new(
                &directory.join("stderr.bin"),
                format!("failed to spawn command: {error}\n").as_bytes(),
            );
            write_new(
                &directory.join("process.status"),
                format!("spawn_error={error}\nsuccess=false\n").as_bytes(),
            );
            let environment = environment
                .iter()
                .map(|(key, value)| format!("{key}={value}\n"))
                .collect::<String>();
            write_new(
                &directory.join("command.environment.txt"),
                environment.as_bytes(),
            );
            panic!(
                "invoke {} for L3 evidence: {error}",
                program.to_string_lossy()
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

fn assert_no_legacy_fallback(stderr: &str, mode: &str) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} selected a legacy route: {stderr}"
    );
}

fn parse_manifest(output: &Output, mode: &str) -> BTreeMap<String, String> {
    assert_success(output, mode);
    let fields = text(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some("cli-mir-v1"),
        "{mode} changed the CLI MIR receipt schema"
    );
    for field in RECEIPT_FIELDS {
        let digest = fields
            .get(*field)
            .unwrap_or_else(|| panic!("{mode} receipt omitted {field}"));
        assert!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{mode} receipt has malformed {field}: {digest}"
        );
    }
    fields
}

fn assert_default_route(stderr: &str, expected_digest: &str, mode: &str) {
    let marker = format!("canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest=");
    let matching = stderr
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit exactly one CheckedGet route receipt: {stderr}"
    );
    let digest = matching[0]
        .split_once("mir_digest=")
        .expect("CheckedGet route receipt contains a MIR digest")
        .1
        .trim();
    assert_eq!(
        digest, expected_digest,
        "{mode} route receipt digest differs"
    );
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_explicit_mir_census(stderr: &str, expected_digest: &str, mode: &str) {
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one actual native MIR census: {stderr}"
    );
    let (counts, digest) = matching[0]
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("MIR native census has its stable prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("MIR native census reports no fallback and a digest");
    let (emitted, expected) = counts
        .split_once('/')
        .expect("MIR native census reports emitted/expected counts");
    let emitted = emitted
        .parse::<usize>()
        .expect("MIR native emitted count is numeric");
    let expected = expected
        .parse::<usize>()
        .expect("MIR native expected count is numeric");
    assert!(expected > 0, "{mode} MIR native census is empty");
    assert_eq!(emitted, expected, "{mode} omitted a MIR function");
    assert_eq!(
        digest, expected_digest,
        "{mode} native census digest differs"
    );
    assert_no_legacy_fallback(stderr, mode);
}

fn retain_sha256(
    sha256sum: &Path,
    path: &Path,
    label: &str,
    binary_source: &str,
    evidence_root: &Path,
) -> String {
    let canonical = fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()));
    let directory = evidence_root.join(format!("{label}-sha256"));
    create_capture_directory(&directory);
    let arguments = vec![OsString::from("--"), canonical.as_os_str().to_owned()];
    let output = run_recorded(sha256sum.as_os_str(), &arguments, &[], &directory);
    assert_success(&output, &format!("sha256sum {}", canonical.display()));
    let line = text(&output.stdout);
    let (digest, hashed_path) = line
        .trim_end()
        .split_once("  ")
        .expect("sha256sum output has digest and path");
    assert_eq!(
        hashed_path,
        canonical.to_string_lossy().as_ref(),
        "sha256sum did not hash the canonical binary path"
    );
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "sha256sum returned malformed digest {digest}"
    );
    write_new(
        &evidence_root.join(format!("{label}-identity.txt")),
        format!(
            "binary_source={binary_source}\ncanonical_path={}\nsha256={digest}\n",
            canonical.display()
        )
        .as_bytes(),
    );
    digest.to_string()
}

fn valgrind_row(report: &str, marker: &str, mode: &str) -> String {
    let rows = report
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("==")?;
            let (pid, message) = rest.split_once("==")?;
            message
                .trim_start()
                .starts_with(marker)
                .then(|| format!("=={pid}== {}", message.trim()))
        })
        .collect::<Vec<_>>();
    let [row] = rows.as_slice() else {
        panic!(
            "{mode} Valgrind report must contain one {marker:?} summary row; found {}:\n{report}",
            rows.len()
        );
    };
    row.clone()
}

fn valgrind_pid(row: &str, mode: &str) -> String {
    let pid = row
        .strip_prefix("==")
        .and_then(|row| row.split_once("=="))
        .map(|(pid, _)| pid)
        .unwrap_or_else(|| panic!("{mode} Valgrind summary lacks a PID: {row}"));
    assert!(
        !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
        "{mode} Valgrind summary has a malformed PID: {row}"
    );
    pid.to_string()
}

fn memcheck_count(value: &str, mode: &str) -> u64 {
    let groups = value.split(',').collect::<Vec<_>>();
    let valid = match groups.as_slice() {
        [digits] => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        [first, rest @ ..] => {
            (1..=3).contains(&first.len())
                && first.bytes().all(|byte| byte.is_ascii_digit())
                && rest.iter().all(|group| {
                    group.len() == 3 && group.bytes().all(|byte| byte.is_ascii_digit())
                })
        }
        [] => false,
    };
    assert!(valid, "{mode} has a malformed Memcheck count: {value:?}");
    value
        .bytes()
        .filter(|byte| *byte != b',')
        .map(char::from)
        .collect::<String>()
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("{mode} Memcheck count overflows u64: {error}"))
}

fn memcheck_bytes_blocks(row: &str, marker: &str, mode: &str) -> (u64, u64) {
    let message = row
        .strip_prefix("==")
        .and_then(|row| row.split_once("=="))
        .map(|(_, message)| message.trim())
        .unwrap_or_else(|| panic!("{mode} has a malformed Valgrind row: {row}"));
    let value = message
        .strip_prefix(marker)
        .map(str::trim_start)
        .unwrap_or_else(|| panic!("{mode} has a malformed {marker} row: {row}"));
    let (bytes, blocks) = value
        .strip_suffix(" blocks")
        .and_then(|value| value.split_once(" bytes in "))
        .unwrap_or_else(|| panic!("{mode} has a malformed {marker} row: {row}"));
    (memcheck_count(bytes, mode), memcheck_count(blocks, mode))
}

fn assert_zero_memcheck_row(row: &str, marker: &str, mode: &str) {
    assert_eq!(
        memcheck_bytes_blocks(row, marker, mode),
        (0, 0),
        "{mode} reported nonzero {marker} memory: {row}"
    );
}

fn memcheck_summary(report: &str, mode: &str) -> String {
    assert!(
        report.ends_with('\n'),
        "{mode} Valgrind report is truncated or lacks its final newline"
    );
    let report_rows = report.lines().collect::<Vec<_>>();
    assert!(!report_rows.is_empty(), "{mode} Valgrind report is empty");
    let first_pid = valgrind_pid(report_rows[0], mode);
    for row in &report_rows {
        assert_eq!(
            valgrind_pid(row, mode),
            first_pid,
            "{mode} Valgrind report contains a malformed or foreign PID: {row}"
        );
    }

    let in_use = valgrind_row(report, "in use at exit:", mode);
    let total = valgrind_row(report, "total heap usage:", mode);
    let errors = valgrind_row(report, "ERROR SUMMARY:", mode);
    let pid = valgrind_pid(&in_use, mode);
    let row_prefix = format!("=={pid}== ");
    assert_eq!(
        valgrind_pid(&total, mode),
        pid,
        "{mode} heap total belongs to a different process"
    );
    assert_eq!(
        valgrind_pid(&errors, mode),
        pid,
        "{mode} error summary belongs to a different process"
    );
    let usage = total
        .strip_prefix(&row_prefix)
        .and_then(|message| message.strip_prefix("total heap usage:"))
        .unwrap_or_else(|| panic!("{mode} has a malformed heap usage row: {total}"))
        .trim();
    let (allocations, rest) = usage
        .split_once(" allocs, ")
        .unwrap_or_else(|| panic!("{mode} has a malformed allocation total: {total}"));
    let (frees, bytes) = rest
        .split_once(" frees, ")
        .and_then(|(frees, bytes)| {
            bytes
                .strip_suffix(" bytes allocated")
                .map(|bytes| (frees, bytes))
        })
        .unwrap_or_else(|| panic!("{mode} has a malformed free total: {total}"));
    let allocations = memcheck_count(allocations, mode);
    let frees = memcheck_count(frees, mode);
    let _allocated_bytes = memcheck_count(bytes, mode);
    let error_message = errors
        .strip_prefix(&row_prefix)
        .unwrap_or_else(|| panic!("{mode} error summary has the wrong process identity"));
    assert_eq!(
        error_message, "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        "{mode} reported Memcheck errors or suppressed errors: {errors}"
    );

    let all_free = report
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("==")?;
            let (pid, message) = rest.split_once("==")?;
            message
                .trim_start()
                .starts_with("All heap blocks were freed")
                .then(|| format!("=={pid}== {}", message.trim()))
        })
        .collect::<Vec<_>>();
    assert!(
        all_free.len() <= 1,
        "{mode} has duplicate all-free summaries:\n{report}"
    );
    let categories = [
        "definitely lost:",
        "indirectly lost:",
        "possibly lost:",
        "still reachable:",
    ];
    let category_rows = categories
        .iter()
        .map(|marker| {
            report
                .lines()
                .filter_map(|line| {
                    let rest = line.strip_prefix("==")?;
                    let (pid, message) = rest.split_once("==")?;
                    message
                        .trim_start()
                        .starts_with(marker)
                        .then(|| format!("=={pid}== {}", message.trim()))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let counts = category_rows.iter().map(Vec::len).collect::<Vec<_>>();
    assert!(
        counts.iter().all(|count| *count == 0) || counts.iter().all(|count| *count == 1),
        "{mode} has a partial or duplicate leak-summary set {counts:?}:\n{report}"
    );
    let leak_headers = report
        .lines()
        .filter(|line| {
            line.strip_prefix("==")
                .and_then(|rest| rest.split_once("=="))
                .is_some_and(|(_, message)| message.trim_start().starts_with("LEAK SUMMARY:"))
        })
        .count();
    if counts.iter().all(|count| *count == 0) {
        assert_eq!(
            leak_headers, 0,
            "{mode} has a LEAK SUMMARY header without its complete set of rows:\n{report}"
        );
    } else {
        assert!(
            leak_headers <= 1,
            "{mode} has duplicate LEAK SUMMARY headers:\n{report}"
        );
        for (row, marker) in category_rows.iter().zip(categories) {
            let [row] = row.as_slice() else {
                unreachable!("complete leak-summary set was checked above")
            };
            assert_eq!(
                valgrind_pid(row, mode),
                pid,
                "{mode} leak category belongs to a different process"
            );
            if marker != "still reachable:" {
                assert_zero_memcheck_row(row, marker, mode);
            } else {
                if all_free.is_empty() {
                    let _ = memcheck_bytes_blocks(row, marker, mode);
                } else {
                    assert_zero_memcheck_row(row, marker, mode);
                }
            }
        }
    }

    let suppressed = report
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("==")?;
            let (pid, message) = rest.split_once("==")?;
            message
                .trim_start()
                .starts_with("suppressed:")
                .then(|| format!("=={pid}== {}", message.trim()))
        })
        .collect::<Vec<_>>();
    assert!(
        suppressed.len() <= 1,
        "{mode} has duplicate suppression rows:\n{report}"
    );
    if let Some(row) = suppressed.first() {
        assert_eq!(
            valgrind_pid(row, mode),
            pid,
            "{mode} suppressed heap row belongs to a different process"
        );
        assert_zero_memcheck_row(row, "suppressed:", mode);
    }
    assert!(
        !report.lines().any(|line| line.contains(" loss record ")),
        "{mode} has a Valgrind loss detail record:\n{report}"
    );

    if let [all_free] = all_free.as_slice() {
        assert_eq!(
            valgrind_pid(all_free, mode),
            pid,
            "{mode} all-free summary belongs to a different process"
        );
        assert_eq!(
            in_use.strip_prefix(&row_prefix),
            Some("in use at exit: 0 bytes in 0 blocks"),
            "{mode} all-free report must show zero live heap bytes"
        );
        assert_eq!(
            all_free.strip_prefix(&row_prefix),
            Some("All heap blocks were freed -- no leaks are possible"),
            "{mode} all-free summary has an unexpected marker"
        );
        assert_eq!(
            allocations, frees,
            "{mode} all-free report has different allocation and free totals"
        );
        let mut selected = vec![in_use, all_free.clone()];
        selected
            .extend(categories.map(|marker| format!("{row_prefix}{marker} 0 bytes in 0 blocks")));
        selected.push(format!("{row_prefix}suppressed: 0 bytes in 0 blocks"));
        selected.push(errors);
        selected.push("ERROR SUPPRESSION SUMMARY: 0 from 0".to_string());
        format!("{}\n", selected.join("\n"))
    } else {
        assert!(
            counts.iter().all(|count| *count == 1),
            "{mode} live heap report lacks complete leak categories:\n{report}"
        );
        let mut selected = vec![in_use, total];
        selected.extend(category_rows.into_iter().map(|rows| rows[0].clone()));
        if let Some(row) = suppressed.first() {
            selected.push(row.clone());
        }
        selected.push(errors);
        selected.push("ERROR SUPPRESSION SUMMARY: 0 from 0".to_string());
        format!("{}\n", selected.join("\n"))
    }
}

#[cfg(test)]
mod memcheck_summary_tests {
    use super::memcheck_summary;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    const ALL_FREE_REPORT: &str = "==31415== Memcheck, a memory error detector\n\
==31415== HEAP SUMMARY:\n\
==31415==     in use at exit: 0 bytes in 0 blocks\n\
==31415==   total heap usage: 31 allocs, 31 frees, 2,694 bytes allocated\n\
==31415== \n\
==31415== All heap blocks were freed -- no leaks are possible\n\
==31415== \n\
==31415== For lists of detected and suppressed errors, rerun with: -s\n\
==31415== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n";

    fn all_free_report_with_zero_rows() -> String {
        ALL_FREE_REPORT.replace(
            "==31415== ERROR SUMMARY:",
            "==31415== LEAK SUMMARY:\n\
==31415==    definitely lost: 0 bytes in 0 blocks\n\
==31415==    indirectly lost: 0 bytes in 0 blocks\n\
==31415==      possibly lost: 0 bytes in 0 blocks\n\
==31415==    still reachable: 0 bytes in 0 blocks\n\
==31415==         suppressed: 0 bytes in 0 blocks\n\
==31415== ERROR SUMMARY:",
        )
    }

    fn assert_rejected(report: &str) {
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                memcheck_summary(report, "synthetic strict CheckedGet Memcheck report")
            }))
            .is_err(),
            "malformed or nonzero Memcheck report was accepted:\n{report}"
        );
    }

    #[test]
    fn accepts_valgrind_all_free_report_without_optional_leak_rows() {
        let summary = memcheck_summary(ALL_FREE_REPORT, "all-free CheckedGet fixture");
        for expected in [
            "in use at exit: 0 bytes in 0 blocks",
            "All heap blocks were freed -- no leaks are possible",
            "definitely lost: 0 bytes in 0 blocks",
            "indirectly lost: 0 bytes in 0 blocks",
            "possibly lost: 0 bytes in 0 blocks",
            "still reachable: 0 bytes in 0 blocks",
            "suppressed: 0 bytes in 0 blocks",
            "ERROR SUMMARY: 0 errors",
            "ERROR SUPPRESSION SUMMARY: 0 from 0",
        ] {
            assert!(
                summary.lines().any(|line| line.contains(expected)),
                "{summary}"
            );
        }
    }

    #[test]
    fn accepts_complete_zero_leak_rows_and_zero_suppression() {
        memcheck_summary(
            &all_free_report_with_zero_rows(),
            "all-free CheckedGet fixture with explicit zero rows",
        );
    }

    #[test]
    fn rejects_nonzero_loss_and_partial_loss_rows() {
        let nonzero = all_free_report_with_zero_rows().replace(
            "definitely lost: 0 bytes in 0 blocks",
            "definitely lost: 1 bytes in 1 blocks",
        );
        assert_rejected(&nonzero);

        let partial = ALL_FREE_REPORT.replace(
            "==31415== ERROR SUMMARY:",
            "==31415== definitely lost: 0 bytes in 0 blocks\n==31415== ERROR SUMMARY:",
        );
        assert_rejected(&partial);
    }

    #[test]
    fn rejects_nonzero_suppression_and_memcheck_errors() {
        let suppressed = ALL_FREE_REPORT.replace(
            "==31415== ERROR SUMMARY:",
            "==31415== suppressed: 1 bytes in 1 blocks\n==31415== ERROR SUMMARY:",
        );
        assert_rejected(&suppressed);

        let errors = ALL_FREE_REPORT.replace(
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
            "ERROR SUMMARY: 1 errors from 1 contexts (suppressed: 0 from 0)",
        );
        assert_rejected(&errors);
    }

    #[test]
    fn rejects_foreign_process_and_inconsistent_all_free_totals() {
        let foreign =
            ALL_FREE_REPORT.replace("==31415== ERROR SUMMARY:", "==27182== ERROR SUMMARY:");
        assert_rejected(&foreign);

        let mismatched = ALL_FREE_REPORT.replace("31 frees", "30 frees");
        assert_rejected(&mismatched);

        let live = ALL_FREE_REPORT.replace(
            "in use at exit: 0 bytes in 0 blocks",
            "in use at exit: 1 bytes in 1 blocks",
        );
        assert_rejected(&live);
    }
}

fn assert_strict_memcheck(
    valgrind: &Path,
    binary: &Path,
    case: &Case,
    mode: &str,
    evidence_directory: &Path,
) -> (i32, Vec<u8>) {
    write_new(
        &evidence_directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code={}\nexpected_stdout_bytes={}\n",
            case.expected_exit,
            case.expected_stdout.len()
        )
        .as_bytes(),
    );
    write_new(
        &evidence_directory.join("expected-native-stdout.bin"),
        case.expected_stdout,
    );
    let valgrind_log = evidence_directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=97"),
        OsString::from(format!("--log-file={}", valgrind_log.display())),
        binary.as_os_str().to_owned(),
    ];
    let output = run_recorded(valgrind.as_os_str(), &arguments, &[], evidence_directory);
    let report = fs::read(&valgrind_log).unwrap_or_else(|error| {
        panic!("read raw Valgrind log {}: {error}", valgrind_log.display())
    });
    let report_text = text(&report);
    write_new(
        &evidence_directory.join("memcheck-summary.txt"),
        memcheck_summary(&report_text, mode).as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(case.expected_exit),
        "{mode} changed the expected process status or Valgrind found an error:\n{}\n{}",
        report_text,
        text(&output.stderr)
    );
    assert_eq!(
        output.stdout.as_slice(),
        case.expected_stdout,
        "{mode} native stdout differs"
    );
    (
        output
            .status
            .code()
            .expect("validated Valgrind exit status"),
        output.stdout,
    )
}

struct EvidenceDirectory {
    root: PathBuf,
    native: PathBuf,
}

impl EvidenceDirectory {
    fn new() -> Self {
        let configured = std::env::var_os("MIMI_MAP_V2_CHECKED_GET_L3_ARTIFACT_DIR")
            .filter(|value| !value.is_empty())
            .expect("strict CheckedGet L3 requires retained artifact output");
        let configured = PathBuf::from(configured);
        let root = if configured.is_absolute() {
            configured
        } else {
            std::env::current_dir()
                .expect("resolve current directory for CheckedGet artifacts")
                .join(configured)
        };
        if !root.exists() {
            if let Some(parent) = root.parent() {
                fs::create_dir_all(parent)
                    .expect("create parent for CheckedGet artifact directory");
            }
            fs::create_dir(&root).expect("create fresh CheckedGet artifact directory");
        }
        let root = fs::canonicalize(&root).expect("canonicalize CheckedGet artifact directory");
        let native = root.join("native");
        create_capture_directory(&native);
        Self { root, native }
    }
}

#[test]
#[ignore = "strict CheckedGet native Memcheck L3; run scripts/verify-ordinary-map-v2-checked-get-l3.sh"]
fn ordinary_map_owned_any_v2_checked_get_native_builds_are_strict_memcheck_clean() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the CheckedGet slice must not open global Map/Any V2 readiness"
        );
    };
    let mimi = mimi_bin();
    let evidence = EvidenceDirectory::new();
    let (valgrind, valgrind_binary) = required_valgrind_tool();
    let sha256sum = required_elf_tool("MIMI_L3_SHA256SUM");
    write_new(
        &evidence.native.join("valgrind-tool-identity.txt"),
        format!(
            "real_path={}\nbinary_path={}\n",
            valgrind.display(),
            valgrind_binary.display()
        )
        .as_bytes(),
    );
    write_new(
        &evidence.native.join("sha256sum-tool-identity.txt"),
        format!("real_path={}\n", sha256sum.display()).as_bytes(),
    );
    retain_sha256(
        &sha256sum,
        &mimi,
        "mimi-binary",
        "CARGO_BIN_EXE_mimi",
        &evidence.native,
    );

    let valgrind_version = evidence.native.join("valgrind-version");
    create_capture_directory(&valgrind_version);
    let version_arguments = vec![OsString::from("--version")];
    let version = run_recorded(
        valgrind.as_os_str(),
        &version_arguments,
        &[],
        &valgrind_version,
    );
    assert_success(&version, "Valgrind version query");
    assert!(
        text(&version.stdout).trim().starts_with("valgrind-"),
        "Valgrind version query did not identify Valgrind: {}",
        text(&version.stdout)
    );

    let mut default_receipt_count = 0;
    let mut explicit_census_count = 0;
    let mut native_build_count = 0;
    let mut memcheck_run_count = 0;
    for (index, case) in CASES.iter().enumerate() {
        let case_directory = evidence
            .native
            .join(format!("case-{index:02}-{}", case.label));
        create_capture_directory(&case_directory);
        let source = fixture(case.fixture);
        let source_bytes = fs::read(&source).unwrap_or_else(|error| {
            panic!("read CheckedGet fixture {}: {error}", source.display())
        });
        write_new(&case_directory.join("source.fixture.mimi"), &source_bytes);

        let receipt_directory = case_directory.join("receipt");
        create_capture_directory(&receipt_directory);
        let receipt_arguments = vec![
            OsString::from("mir"),
            OsString::from("--all"),
            OsString::from("--receipt"),
            source.as_os_str().to_owned(),
        ];
        let receipt = run_recorded(
            mimi.as_os_str(),
            &receipt_arguments,
            &[],
            &receipt_directory,
        );
        let manifest = parse_manifest(&receipt, "mimi mir --all --receipt");
        let digest = manifest
            .get("mir_digest")
            .expect("validated MIR receipt contains its digest")
            .clone();

        let mut default_result: Option<(i32, Vec<u8>)> = None;
        let mut explicit_result: Option<(i32, Vec<u8>)> = None;
        for direct in [false, true] {
            let label = if direct {
                "explicit-build"
            } else {
                "default-build"
            };
            let build_directory = case_directory.join(label);
            create_capture_directory(&build_directory);
            let binary = build_directory.join("program");
            let mut arguments = vec![
                OsString::from("build"),
                source.as_os_str().to_owned(),
                OsString::from("--output"),
                binary.as_os_str().to_owned(),
            ];
            if direct {
                arguments.push(OsString::from("--mir"));
            }
            let environment = [("MIMI_ROUTE_CENSUS", "1")];
            let build = run_recorded(mimi.as_os_str(), &arguments, &environment, &build_directory);
            assert_success(&build, label);
            assert!(binary.is_file(), "{label} did not produce a native binary");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_ne!(
                    fs::metadata(&binary)
                        .expect("inspect native binary")
                        .permissions()
                        .mode()
                        & 0o111,
                    0,
                    "{label} binary is not executable"
                );
            }
            retain_sha256(
                &sha256sum,
                &binary,
                "program",
                "mimi build --output",
                &build_directory,
            );
            let stderr = text(&build.stderr);
            if direct {
                assert_explicit_mir_census(&stderr, &digest, label);
                explicit_census_count += 1;
            } else {
                assert_default_route(&stderr, &digest, label);
                default_receipt_count += 1;
            }
            native_build_count += 1;

            let memcheck_directory = case_directory.join(if direct {
                "explicit-memcheck"
            } else {
                "default-memcheck"
            });
            create_capture_directory(&memcheck_directory);
            let result =
                assert_strict_memcheck(&valgrind, &binary, case, label, &memcheck_directory);
            memcheck_run_count += 1;
            if direct {
                explicit_result = Some(result);
            } else {
                default_result = Some(result);
            }
        }
        assert_eq!(
            default_result, explicit_result,
            "default and explicit CheckedGet native results differ for {}",
            case.label
        );
    }

    write_new(
        &evidence.native.join("run-summary.txt"),
        format!(
            "route_profile={ROUTE_PROFILE}\nglobal_map_any_v2_production_ready=false\ncase_count={}\nnative_build_count={native_build_count}\nmemcheck_run_count={memcheck_run_count}\ndefault_route_receipt_count={default_receipt_count}\nexplicit_mir_census_count={explicit_census_count}\nlegacy_fallback_count=0\n",
            CASES.len()
        )
        .as_bytes(),
    );
    assert_eq!(default_receipt_count, CASES.len());
    assert_eq!(explicit_census_count, CASES.len());
    assert_eq!(native_build_count, CASES.len() * 2);
    assert_eq!(memcheck_run_count, CASES.len() * 2);
    assert!(evidence.root.is_dir());
}
