use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ROUTE_PROFILE: &str = "map-any-owner-v2-values-snapshot-replacement-v1";
const MIR_RECEIPT_PROFILE: &str = "cli-mir-v1";
const EXPECTED_STDOUT: &[u8] = b"true\ntrue\n";
const RECEIPT_FIELDS: &[&str] = &[
    "schema",
    "profile",
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
    "root_owners",
];
const RECEIPT_DIGEST_FIELDS: &[&str] = &[
    "mir_digest",
    "type_desc_digest",
    "abi_digest",
    "ffi_digest",
    "ownership_digest",
    "flow_transition_digest",
];
const NEGATIVE_COMMANDS_PER_CASE: usize = 6;

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
        .expect("Cargo must provide mimi for ValuesSnapshotReplacement production L3 evidence")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
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
                "invoke {} for ValuesSnapshotReplacement L3 evidence: {error}",
                program.to_string_lossy()
            );
        }
    }
}

fn run_mimi(
    mimi: &Path,
    arguments: &[OsString],
    environment: &[(&str, &str)],
    directory: &Path,
) -> Output {
    run_recorded(mimi.as_os_str(), arguments, environment, directory)
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

fn parse_key_values<'a>(
    tokens: impl Iterator<Item = &'a str>,
    mode: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for token in tokens {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{mode} contains a malformed receipt token: {token}"));
        assert!(
            fields.insert(key.to_string(), value.to_string()).is_none(),
            "{mode} repeats receipt field {key}"
        );
    }
    fields
}

fn parse_mir_receipt(output: &Output, mode: &str) -> BTreeMap<String, String> {
    assert_success(output, mode);
    let fields = text(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields.len(),
        RECEIPT_FIELDS.len(),
        "{mode} changed the canonical MIR receipt field set: {fields:?}"
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(MIR_RECEIPT_PROFILE),
        "{mode} changed the CLI receipt schema"
    );
    for field in RECEIPT_FIELDS {
        assert!(fields.contains_key(*field), "{mode} omitted {field}");
    }
    for field in [
        "mir_digest",
        "type_desc_digest",
        "abi_digest",
        "ffi_digest",
        "ownership_digest",
        "flow_transition_digest",
    ] {
        let digest = &fields[field];
        assert!(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{mode} has malformed {field}: {digest}"
        );
    }
    assert!(
        fields
            .get("root_owners")
            .is_some_and(|owners| owners.split(',').any(|owner| owner == "function:main")),
        "{mode} omitted the main owner: {fields:?}"
    );
    assert!(
        !fields.contains_key("execution_route")
            && !fields.contains_key("production_ready")
            && !fields.contains_key("global_map_any_v2_production_ready"),
        "{mode} receipt claimed global production readiness: {fields:?}"
    );
    fields
}

fn assert_receipt_identity(
    observed: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
    mode: &str,
) {
    for field in RECEIPT_FIELDS {
        if *field == "profile" {
            continue;
        }
        assert_eq!(
            observed.get(*field),
            expected.get(*field),
            "{mode} changed MIR receipt identity field {field}"
        );
    }
}

fn assert_default_profile(stderr: &str, digest: &str, mode: &str) {
    let expected =
        format!("canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest={digest}");
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("canonical route disposition: canonical ("))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} did not select exactly the dedicated ValuesSnapshotReplacement route: {stderr}"
    );
    assert_eq!(matching[0], expected.as_str());
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_route_admission(
    stderr: &str,
    consumer: &str,
    mode_name: &str,
    digest: &str,
    mode: &str,
) -> String {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS route-admission "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must report exactly one Checker route-admission record: {stderr}"
    );
    let fields = parse_key_values(
        lines[0]
            .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
            .expect("route-admission census has its stable prefix")
            .split_whitespace(),
        mode,
    );
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some(consumer),
        "{mode} reported the wrong route-admission consumer"
    );
    assert_eq!(
        fields.get("mode").map(String::as_str),
        Some(mode_name),
        "{mode} reported the wrong route-admission mode"
    );
    assert_eq!(
        fields.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} did not report the dedicated Checker admission profile"
    );
    assert_eq!(
        fields.get("mir_digest").map(String::as_str),
        Some(digest),
        "{mode} route-admission record is not bound to the exact MIR digest"
    );
    fields
        .get("checker_profile")
        .expect("route-admission record has checker_profile")
        .clone()
}

fn assert_native_census(stderr: &str, digest: &str, mode: &str) {
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("info: canonical MIR native compiled "))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} must emit one complete canonical MIR native census: {stderr}"
    );
    let (counts, observed_digest) = matching[0]
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("native census has its stable prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .expect("native census reports no legacy fallback and a MIR digest");
    let (emitted, expected) = counts
        .split_once('/')
        .expect("native census reports emitted and expected function counts");
    let emitted = emitted
        .parse::<usize>()
        .expect("native census emitted function count is numeric");
    let expected = expected
        .parse::<usize>()
        .expect("native census expected function count is numeric");
    assert!(expected > 0, "{mode} native census is empty");
    assert_eq!(emitted, expected, "{mode} omitted a MIR function");
    assert_eq!(
        observed_digest, digest,
        "{mode} native census MIR digest differs"
    );
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_explicit_run_profile(stderr: &str, digest: &str, mode: &str) {
    let expected = format!(
        "canonical route disposition: explicit canonical ({ROUTE_PROFILE}) mir_digest={digest}"
    );
    let matching = stderr
        .lines()
        .filter(|line| line.starts_with("canonical route disposition: explicit canonical ("))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "{mode} did not report one explicit MIR run receipt: {stderr}"
    );
    assert_eq!(matching[0], expected.as_str());
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_build_receipt(
    stderr: &str,
    consumer: &str,
    expected: &BTreeMap<String, String>,
    mode: &str,
) {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("MIMI_ROUTE_CENSUS canonical-native-build "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must emit one build receipt: {stderr}"
    );
    let mut tokens = lines[0].split_whitespace();
    assert_eq!(tokens.next(), Some("MIMI_ROUTE_CENSUS"));
    assert_eq!(tokens.next(), Some("canonical-native-build"));
    let fields = parse_key_values(tokens, mode);
    assert_eq!(
        fields.get("consumer").map(String::as_str),
        Some(consumer),
        "{mode} reported the wrong build consumer"
    );
    assert_eq!(
        fields.get("route_id").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} changed the build receipt identity"
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} changed the build receipt profile"
    );
    assert_receipt_identity(&fields, expected, mode);
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_verifier_receipt(stderr: &str, expected: &BTreeMap<String, String>, mode: &str) {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with("canonical MIR verifier provenance: "))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "{mode} must expose the actual verifier receipt (Z3 must be available): {stderr}"
    );
    let fields = parse_key_values(
        lines[0]
            .strip_prefix("canonical MIR verifier provenance: ")
            .expect("verifier receipt has its prefix")
            .split_whitespace(),
        mode,
    );
    assert_eq!(
        fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{mode} changed the verifier receipt profile"
    );
    for field in RECEIPT_DIGEST_FIELDS {
        assert_eq!(
            fields.get(*field),
            expected.get(*field),
            "{mode} changed MIR receipt identity field {field}"
        );
    }
    assert_no_legacy_fallback(stderr, mode);
}

fn assert_no_legacy_or_route_success(output: &Output, mode: &str) {
    let stderr = text(&output.stderr);
    let diagnostic = format!("{}\n{stderr}", text(&output.stdout));
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{mode} attempted legacy fallback:\n{diagnostic}"
    );
    assert!(
        !diagnostic.contains("canonical route disposition: canonical ("),
        "{mode} reported a selected canonical route despite rejecting the shape:\n{diagnostic}"
    );
}

fn assert_fail_closed(output: &Output, mode: &str) {
    assert!(
        !output.status.success(),
        "{mode} unexpectedly accepted a shape outside ValuesSnapshotReplacement:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let diagnostic = format!("{}\n{}", text(&output.stdout), text(&output.stderr));
    assert!(
        diagnostic.contains("MIR-COVERAGE-001") || diagnostic.contains("not production-ready"),
        "{mode} did not fail at the canonical coverage/readiness boundary:\n{diagnostic}"
    );
    assert_no_legacy_or_route_success(output, mode);
}

fn retain_sha256(path: &Path, label: &str, source: &str, evidence_root: &Path) -> String {
    let canonical = fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()));
    let directory = evidence_root.join(format!("{label}-sha256"));
    create_capture_directory(&directory);
    let arguments = vec![OsString::from("--"), canonical.as_os_str().to_owned()];
    let output = run_recorded(OsStr::new("sha256sum"), &arguments, &[], &directory);
    assert_success(&output, &format!("sha256sum {}", canonical.display()));
    let line = text(&output.stdout);
    let (digest, hashed_path) = line
        .trim_end()
        .split_once("  ")
        .expect("sha256sum output contains digest and path");
    assert_eq!(hashed_path, canonical.to_string_lossy());
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "sha256sum returned malformed digest {digest}"
    );
    write_new(
        &evidence_root.join(format!("{label}-identity.txt")),
        format!(
            "binary_source={source}\ncanonical_path={}\nsha256={digest}\n",
            canonical.display()
        )
        .as_bytes(),
    );
    digest.to_string()
}

fn valgrind_summary_line(report: &str, marker: &str, mode: &str) -> String {
    let matching = report
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
    let [line] = matching.as_slice() else {
        panic!(
            "{mode} Valgrind report must contain one {marker:?} summary row; found {}:\n{report}",
            matching.len()
        );
    };
    (*line).clone()
}

fn valgrind_summary_pid(line: &str, mode: &str) -> String {
    let pid = line
        .strip_prefix("==")
        .and_then(|line| line.split_once("=="))
        .map(|(pid, _)| pid)
        .unwrap_or_else(|| panic!("{mode} Valgrind summary lacks a PID: {line}"));
    assert!(
        !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
        "{mode} Valgrind summary has a malformed PID: {line}"
    );
    pid.to_owned()
}

fn parse_grouped_memcheck_count(value: &str, mode: &str) -> u64 {
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
    assert!(
        valid,
        "{mode} has a malformed grouped Memcheck count: {value:?}"
    );
    value
        .bytes()
        .filter(|byte| *byte != b',')
        .map(char::from)
        .collect::<String>()
        .parse::<u64>()
        .unwrap_or_else(|error| panic!("{mode} Memcheck count overflows u64: {error}"))
}

fn parse_memcheck_bytes_blocks(line: &str, marker: &str, mode: &str) -> (u64, u64) {
    let value = line
        .strip_prefix(marker)
        .map(str::trim_start)
        .unwrap_or_else(|| panic!("{mode} has a malformed {marker} row: {line}"));
    let (bytes, blocks) = value
        .strip_suffix(" blocks")
        .and_then(|value| value.split_once(" bytes in "))
        .unwrap_or_else(|| panic!("{mode} has a malformed {marker} row: {line}"));
    (
        parse_grouped_memcheck_count(bytes, mode),
        parse_grouped_memcheck_count(blocks, mode),
    )
}

fn memcheck_summary(report: &str, mode: &str) -> String {
    assert!(
        report.ends_with('\n'),
        "{mode} Valgrind report is truncated or lacks its final newline"
    );
    let report_rows = report.lines().collect::<Vec<_>>();
    assert!(!report_rows.is_empty(), "{mode} Valgrind report is empty");
    let report_pid = valgrind_summary_pid(report_rows[0], mode);
    for line in &report_rows {
        assert_eq!(
            valgrind_summary_pid(line, mode),
            report_pid,
            "{mode} report contains a malformed or foreign PID: {line}"
        );
    }

    let all_free_rows = report
        .lines()
        .filter(|line| {
            line.strip_prefix("==")
                .and_then(|line| line.split_once("=="))
                .is_some_and(|(_, message)| {
                    message
                        .trim_start()
                        .starts_with("All heap blocks were freed -- no leaks are possible")
                })
        })
        .collect::<Vec<_>>();
    if !all_free_rows.is_empty() {
        assert_eq!(
            all_free_rows.len(),
            1,
            "{mode} has duplicate all-free summaries:\n{report}"
        );
        let in_use = valgrind_summary_line(report, "in use at exit:", mode);
        let total = valgrind_summary_line(report, "total heap usage:", mode);
        let all_free = valgrind_summary_line(report, "All heap blocks were freed", mode);
        let errors = valgrind_summary_line(report, "ERROR SUMMARY:", mode);
        let pid = valgrind_summary_pid(&all_free, mode);
        let row_prefix = format!("=={pid}== ");
        for line in report.lines().filter(|line| line.starts_with("==")) {
            assert_eq!(
                valgrind_summary_pid(line, mode),
                pid,
                "{mode} all-free report has a malformed or foreign PID: {line}"
            );
        }
        assert_eq!(
            in_use.strip_prefix(&row_prefix),
            Some("in use at exit: 0 bytes in 0 blocks"),
            "{mode} all-free report must show zero bytes and blocks in use at exit"
        );
        assert_eq!(
            all_free.strip_prefix(&row_prefix),
            Some("All heap blocks were freed -- no leaks are possible"),
            "{mode} all-free report has an unexpected marker"
        );
        assert_eq!(
            errors.strip_prefix(&row_prefix),
            Some("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)"),
            "{mode} all-free report must have zero errors and suppressions"
        );
        let usage = total
            .strip_prefix(&row_prefix)
            .and_then(|message| message.strip_prefix("total heap usage:"))
            .unwrap_or_else(|| panic!("{mode} has a malformed heap usage row: {total}"))
            .trim();
        let (allocations, after_allocations) = usage
            .split_once(" allocs, ")
            .unwrap_or_else(|| panic!("{mode} has a malformed allocation total: {total}"));
        let (frees, allocated_bytes) = after_allocations
            .split_once(" frees, ")
            .unwrap_or_else(|| panic!("{mode} has a malformed free total: {total}"));
        let allocated_bytes = allocated_bytes
            .strip_suffix(" bytes allocated")
            .unwrap_or_else(|| panic!("{mode} has a malformed allocated-byte total: {total}"));
        let allocations = parse_grouped_memcheck_count(allocations, mode);
        let frees = parse_grouped_memcheck_count(frees, mode);
        let _allocated_bytes = parse_grouped_memcheck_count(allocated_bytes, mode);
        assert_eq!(
            allocations, frees,
            "{mode} heap allocation/free totals differ: {total}"
        );

        let leak_markers = [
            "definitely lost:",
            "indirectly lost:",
            "possibly lost:",
            "still reachable:",
        ];
        let leak_rows = leak_markers.map(|marker| {
            report
                .lines()
                .filter_map(|line| {
                    let (_, message) = line.strip_prefix("==")?.split_once("==")?;
                    message
                        .trim_start()
                        .contains(marker)
                        .then(|| format!("=={report_pid}== {}", message.trim()))
                })
                .collect::<Vec<_>>()
        });
        let observed_counts = leak_rows.each_ref().map(Vec::len);
        assert!(
            observed_counts == [0, 0, 0, 0] || observed_counts == [1, 1, 1, 1],
            "{mode} all-free report has a partial or duplicate loss-summary set {observed_counts:?}:\n{report}"
        );
        let leak_summary_rows = report
            .lines()
            .filter_map(|line| {
                let (_, message) = line.strip_prefix("==")?.split_once("==")?;
                message
                    .trim_start()
                    .starts_with("LEAK SUMMARY:")
                    .then(|| format!("=={report_pid}== {}", message.trim()))
            })
            .collect::<Vec<_>>();
        if observed_counts == [0, 0, 0, 0] {
            assert!(
                leak_summary_rows.is_empty(),
                "{mode} has a LEAK SUMMARY header without its complete set of rows:\n{report}"
            );
        } else {
            if !leak_summary_rows.is_empty() {
                assert_eq!(
                    leak_summary_rows,
                    vec![format!("=={report_pid}== LEAK SUMMARY:")],
                    "{mode} all-free report has a malformed or duplicate LEAK SUMMARY header:\n{report}"
                );
            }
            for rows in &leak_rows {
                let [row] = rows.as_slice() else {
                    unreachable!("complete all-free loss-summary set was checked above")
                };
                let normalized = row
                    .strip_prefix(&row_prefix)
                    .unwrap_or_else(|| panic!("{mode} has a malformed loss-summary row: {row}"));
                let marker = leak_markers
                    .iter()
                    .find(|marker| normalized.starts_with(**marker))
                    .unwrap_or_else(|| panic!("{mode} has an unexpected loss-summary row: {row}"));
                assert_eq!(
                    parse_memcheck_bytes_blocks(normalized, marker, mode),
                    (0, 0),
                    "{mode} all-free report has nonzero {marker} memory:\n{report}"
                );
            }
        }

        let suppressed_rows = report
            .lines()
            .filter_map(|line| {
                let (_, message) = line.strip_prefix("==")?.split_once("==")?;
                message
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("suppressed:")
                    .then(|| format!("=={report_pid}== {}", message.trim()))
            })
            .collect::<Vec<_>>();
        assert!(
            suppressed_rows.len() <= 1,
            "{mode} has duplicate suppression heap rows:\n{report}"
        );
        if let Some(row) = suppressed_rows.first() {
            let normalized = row
                .strip_prefix(&row_prefix)
                .unwrap_or_else(|| panic!("{mode} has a malformed suppression row: {row}"));
            assert_eq!(
                parse_memcheck_bytes_blocks(normalized, "suppressed:", mode),
                (0, 0),
                "{mode} all-free report has suppressed live heap allocations:\n{report}"
            );
        }

        assert!(
            !report.lines().any(|line| line.contains(" loss record ")),
            "{mode} all-free report contains a loss detail record:\n{report}"
        );
        return format!("{in_use}\n{all_free}\n{errors}\n");
    }

    let mut selected = Vec::new();
    let in_use = valgrind_summary_line(report, "in use at exit:", mode);
    let total = valgrind_summary_line(report, "total heap usage:", mode);
    let leak_summary = valgrind_summary_line(report, "LEAK SUMMARY:", mode);
    let error_summary = valgrind_summary_line(report, "ERROR SUMMARY:", mode);
    let pid = valgrind_summary_pid(&in_use, mode);
    let row_prefix = format!("=={pid}== ");
    for line in report.lines().filter(|line| line.starts_with("==")) {
        assert_eq!(
            valgrind_summary_pid(line, mode),
            pid,
            "{mode} report contains a malformed or foreign PID: {line}"
        );
    }
    let in_use_pair = parse_memcheck_bytes_blocks(
        in_use
            .strip_prefix(&row_prefix)
            .unwrap_or_else(|| panic!("{mode} has a malformed in-use row: {in_use}")),
        "in use at exit: ",
        mode,
    );
    let usage = total
        .strip_prefix(&row_prefix)
        .and_then(|message| message.strip_prefix("total heap usage:"))
        .unwrap_or_else(|| panic!("{mode} has a malformed heap usage row: {total}"))
        .trim();
    let (allocations, after_allocations) = usage
        .split_once(" allocs, ")
        .unwrap_or_else(|| panic!("{mode} has a malformed allocation total: {total}"));
    let (frees, allocated_bytes) = after_allocations
        .split_once(" frees, ")
        .unwrap_or_else(|| panic!("{mode} has a malformed free total: {total}"));
    let allocated_bytes = allocated_bytes
        .strip_suffix(" bytes allocated")
        .unwrap_or_else(|| panic!("{mode} has a malformed allocated-byte total: {total}"));
    let allocations = parse_grouped_memcheck_count(allocations, mode);
    let frees = parse_grouped_memcheck_count(frees, mode);
    let _allocated_bytes = parse_grouped_memcheck_count(allocated_bytes, mode);
    assert!(
        allocations >= frees,
        "{mode} heap frees exceed allocations: {total}"
    );
    assert_eq!(
        leak_summary.strip_prefix(&row_prefix),
        Some("LEAK SUMMARY:"),
        "{mode} has an unexpected leak summary row"
    );
    let mut reachable_pair = None;
    for marker in [
        "definitely lost:",
        "indirectly lost:",
        "possibly lost:",
        "still reachable:",
        "suppressed:",
    ] {
        let line = valgrind_summary_line(report, marker, mode);
        let normalized = line
            .strip_prefix(&row_prefix)
            .unwrap_or_else(|| panic!("{mode} has a malformed {marker} row: {line}"));
        let pair = parse_memcheck_bytes_blocks(normalized, marker, mode);
        if marker != "still reachable:" {
            assert_eq!(pair, (0, 0), "{mode} reports {marker} memory:\n{report}");
        } else {
            reachable_pair = Some(pair);
        }
        selected.push(line);
    }
    assert_eq!(
        reachable_pair,
        Some(in_use_pair),
        "{mode} in-use blocks must be exactly the reported still-reachable blocks:\n{report}"
    );
    assert_eq!(
        error_summary.strip_prefix(&row_prefix),
        Some("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)"),
        "{mode} reports Memcheck errors or suppressions:\n{report}"
    );
    selected.push(error_summary);
    format!("{}\n", selected.join("\n"))
}

#[cfg(test)]
mod memcheck_summary_tests {
    use super::memcheck_summary;

    const ALL_FREE_REPORT: &str = "==31415== in use at exit: 0 bytes in 0 blocks\n\
==31415== total heap usage: 2 allocs, 2 frees, 64 bytes allocated\n\
==31415== All heap blocks were freed -- no leaks are possible\n\
==31415== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n";

    const ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS: &str =
        "==31415== in use at exit: 0 bytes in 0 blocks\n\
==31415== total heap usage: 2 allocs, 2 frees, 64 bytes allocated\n\
==31415== All heap blocks were freed -- no leaks are possible\n\
==31415==    definitely lost: 0 bytes in 0 blocks\n\
==31415==    indirectly lost: 0 bytes in 0 blocks\n\
==31415==      possibly lost: 0 bytes in 0 blocks\n\
==31415==    still reachable: 0 bytes in 0 blocks\n\
==31415==         suppressed: 0 bytes in 0 blocks\n\
==31415== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n";

    const STILL_REACHABLE_REPORT: &str = "==31415== in use at exit: 1,449 bytes in 17 blocks\n\
==31415== total heap usage: 90 allocs, 73 frees, 10,188 bytes allocated\n\
==31415== LEAK SUMMARY:\n\
==31415==    definitely lost: 0 bytes in 0 blocks\n\
==31415==    indirectly lost: 0 bytes in 0 blocks\n\
==31415==      possibly lost: 0 bytes in 0 blocks\n\
==31415==    still reachable: 1,449 bytes in 17 blocks\n\
==31415==         suppressed: 0 bytes in 0 blocks\n\
==31415== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n";

    #[test]
    fn accepts_valgrind_322_all_free_summary_without_synthesizing_categories() {
        assert_eq!(
            memcheck_summary(ALL_FREE_REPORT, "all-free fixture"),
            "==31415== in use at exit: 0 bytes in 0 blocks\n\
==31415== All heap blocks were freed -- no leaks are possible\n\
==31415== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n"
        );
    }

    #[test]
    fn accepts_all_free_summary_with_complete_zero_loss_rows() {
        assert_eq!(
            memcheck_summary(
                ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS,
                "all-free fixture with explicit zero loss rows"
            ),
            "==31415== in use at exit: 0 bytes in 0 blocks\n\
==31415== All heap blocks were freed -- no leaks are possible\n\
==31415== ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)\n"
        );
    }

    #[test]
    fn accepts_complete_zero_loss_rows_with_optional_leak_summary_header() {
        let report = ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS.replace(
            "==31415==    definitely lost:",
            "==31415== LEAK SUMMARY:\n==31415==    definitely lost:",
        );
        assert!(
            memcheck_summary(&report, "all-free fixture with leak header")
                .contains("All heap blocks were freed -- no leaks are possible")
        );
    }

    #[test]
    fn accepts_all_free_summary_with_separate_zero_suppressed_heap_row() {
        let report = ALL_FREE_REPORT.replace(
            "==31415== ERROR SUMMARY:",
            "==31415==         suppressed: 0 bytes in 0 blocks\n==31415== ERROR SUMMARY:",
        );
        assert!(
            memcheck_summary(&report, "all-free fixture with zero suppressed heap")
                .contains("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)")
        );
    }

    #[test]
    fn records_reachable_heap_and_rejects_all_lost_categories() {
        let summary = memcheck_summary(STILL_REACHABLE_REPORT, "reachable fixture");
        assert!(summary.contains("still reachable: 1,449 bytes in 17 blocks"));
        assert!(summary.contains("definitely lost: 0 bytes in 0 blocks"));
        assert!(summary.contains("ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)"));
    }

    #[test]
    #[should_panic(expected = "malformed or foreign PID")]
    fn rejects_foreign_process_identity_in_all_free_report() {
        let foreign_pid =
            ALL_FREE_REPORT.replace("==31415== ERROR SUMMARY:", "==27182== ERROR SUMMARY:");
        let _ = memcheck_summary(&foreign_pid, "foreign-pid fixture");
    }

    #[test]
    #[should_panic(expected = "LEAK SUMMARY header without its complete set of rows")]
    fn rejects_leak_categories_mixed_into_all_free_report() {
        let mixed = ALL_FREE_REPORT.replace(
            "==31415== ERROR SUMMARY:",
            "==31415== LEAK SUMMARY:\n==31415== ERROR SUMMARY:",
        );
        let _ = memcheck_summary(&mixed, "mixed-report fixture");
    }

    #[test]
    #[should_panic(expected = "nonzero definitely lost:")]
    fn rejects_nonzero_loss_rows_in_all_free_report() {
        let nonzero = ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS.replace(
            "definitely lost: 0 bytes in 0 blocks",
            "definitely lost: 1 bytes in 1 blocks",
        );
        let _ = memcheck_summary(&nonzero, "nonzero loss fixture");
    }

    #[test]
    #[should_panic(expected = "suppressed live heap allocations")]
    fn rejects_nonzero_suppressed_heap_row_in_all_free_report() {
        let report = ALL_FREE_REPORT.replace(
            "==31415== ERROR SUMMARY:",
            "==31415==         suppressed: 1 bytes in 1 blocks\n==31415== ERROR SUMMARY:",
        );
        let _ = memcheck_summary(&report, "nonzero suppressed heap fixture");
    }

    #[test]
    #[should_panic(expected = "partial or duplicate loss-summary set")]
    fn rejects_incomplete_loss_rows_in_all_free_report() {
        let incomplete = ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS
            .replace("==31415==      possibly lost: 0 bytes in 0 blocks\n", "");
        assert_ne!(
            incomplete, ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS,
            "incomplete loss row fixture must remove the possibly lost row"
        );
        let _ = memcheck_summary(&incomplete, "incomplete loss fixture");
    }

    #[test]
    #[should_panic(expected = "zero errors and suppressions")]
    fn rejects_nonzero_error_suppression_count_in_all_free_report() {
        let suppressed = ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS.replace(
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
            "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 1 from 1)",
        );
        let _ = memcheck_summary(&suppressed, "suppressed error fixture");
    }

    #[test]
    #[should_panic(expected = "lacks its final newline")]
    fn rejects_truncated_all_free_report() {
        let truncated = ALL_FREE_REPORT.trim_end_matches('\n');
        let _ = memcheck_summary(truncated, "truncated fixture");
    }

    #[test]
    #[should_panic(expected = "has a malformed grouped Memcheck count")]
    fn rejects_malformed_loss_row_in_all_free_report() {
        let malformed = ALL_FREE_REPORT_WITH_ZERO_LOSS_ROWS.replace(
            "definitely lost: 0 bytes in 0 blocks",
            "definitely lost: zero bytes in no blocks",
        );
        let _ = memcheck_summary(&malformed, "malformed loss fixture");
    }
}

fn assert_strict_memcheck(binary: &Path, evidence_directory: &Path) {
    write_new(
        &evidence_directory.join("expected-native-result.txt"),
        format!(
            "expected_exit_code=0\nexpected_stdout_bytes={}\n",
            EXPECTED_STDOUT.len()
        )
        .as_bytes(),
    );
    write_new(
        &evidence_directory.join("expected-native-stdout.bin"),
        EXPECTED_STDOUT,
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
    let output = run_recorded(OsStr::new("valgrind"), &arguments, &[], evidence_directory);
    let report = fs::read(&valgrind_log).unwrap_or_else(|error| {
        panic!("read raw Valgrind log {}: {error}", valgrind_log.display())
    });
    write_new(
        &evidence_directory.join("memcheck-summary.txt"),
        memcheck_summary(&text(&report), "strict native Memcheck").as_bytes(),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "Valgrind changed the expected native result or found an error:\n{}\n{}",
        text(&report),
        text(&output.stderr)
    );
    assert_eq!(output.stdout.as_slice(), EXPECTED_STDOUT);
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mimi-map-v2-values-snapshot-replacement-l3-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated ValuesSnapshotReplacement L3 directory");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct EvidenceDirectory {
    root: PathBuf,
    native: PathBuf,
    _temporary: Option<TemporaryDirectory>,
}

impl EvidenceDirectory {
    fn new() -> Self {
        let (root, temporary) = if let Some(configured) =
            std::env::var_os("MIMI_MAP_V2_VALUES_SNAPSHOT_REPLACEMENT_L3_ARTIFACT_DIR")
                .filter(|value| !value.is_empty())
        {
            let configured = PathBuf::from(configured);
            let root = if configured.is_absolute() {
                configured
            } else {
                std::env::current_dir()
                    .expect("resolve current directory for ValuesSnapshotReplacement artifacts")
                    .join(configured)
            };
            if !root.exists() {
                if let Some(parent) = root.parent() {
                    fs::create_dir_all(parent)
                        .expect("create parent for ValuesSnapshotReplacement artifact directory");
                }
                fs::create_dir(&root)
                    .expect("create fresh ValuesSnapshotReplacement artifact directory");
            }
            (
                fs::canonicalize(&root)
                    .expect("canonicalize ValuesSnapshotReplacement artifact directory"),
                None,
            )
        } else {
            let temporary = TemporaryDirectory::new();
            (temporary.0.clone(), Some(temporary))
        };
        let native = root.join("native");
        create_capture_directory(&native);
        Self {
            root,
            native,
            _temporary: temporary,
        }
    }
}

fn cli_environment() -> [(&'static str, &'static str); 2] {
    [("MIMI_ROUTE_CENSUS", "1"), ("MIMI_VERBOSE", "1")]
}

fn assert_successful_run(output: &Output, direct: bool, digest: &str, mode: &str) -> String {
    assert_success(output, mode);
    assert_eq!(
        output.stdout.as_slice(),
        EXPECTED_STDOUT,
        "{mode} output differs from the ValuesSnapshotReplacement fixture contract"
    );
    let stderr = text(&output.stderr);
    let profile = assert_route_admission(
        &stderr,
        "run",
        if direct { "explicit-mir" } else { "default" },
        digest,
        mode,
    );
    if direct {
        assert_explicit_run_profile(&stderr, digest, mode);
    } else {
        assert_default_profile(&stderr, digest, mode);
    }
    profile
}

fn negative_case(mimi: &Path, native: &Path, source: &Path, label: &str) {
    let case_directory = native.join(format!("negative/{label}"));
    fs::create_dir_all(&case_directory)
        .unwrap_or_else(|error| panic!("create negative case directory: {error}"));
    let source_bytes = fs::read(source)
        .unwrap_or_else(|error| panic!("read negative fixture {}: {error}", source.display()));
    write_new(&case_directory.join("source.fixture.mimi"), &source_bytes);

    let receipt_directory = case_directory.join("receipt");
    create_capture_directory(&receipt_directory);
    let receipt_arguments = vec![
        OsString::from("mir"),
        OsString::from("--all"),
        OsString::from("--receipt"),
        source.as_os_str().to_owned(),
    ];
    let receipt = run_mimi(mimi, &receipt_arguments, &[], &receipt_directory);
    let receipt_fields = parse_mir_receipt(&receipt, &format!("{label} read-only MIR receipt"));
    assert!(
        receipt_fields["root_owners"].contains("function:main"),
        "{label} receipt omitted the main owner"
    );

    let environment = cli_environment();
    let mut command_count = 0;
    for direct in [false, true] {
        let mode = if direct {
            "explicit-mir-run"
        } else {
            "default-run"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("run")];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        arguments.push(source.as_os_str().to_owned());
        let output = run_mimi(mimi, &arguments, &environment, &directory);
        assert_fail_closed(&output, &format!("{label} {mode}"));
        command_count += 1;
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-mir-build"
        } else {
            "default-build"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let binary = directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(mimi, &arguments, &environment, &directory);
        assert_fail_closed(&output, &format!("{label} {mode}"));
        assert!(
            !binary.exists(),
            "{label} {mode} left a native binary after rejecting the route"
        );
        command_count += 1;
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-mir-verify"
        } else {
            "default-verify"
        };
        let directory = case_directory.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("verify"), source.as_os_str().to_owned()];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(mimi, &arguments, &environment, &directory);
        assert_fail_closed(&output, &format!("{label} {mode}"));
        command_count += 1;
    }
    assert_eq!(command_count, NEGATIVE_COMMANDS_PER_CASE);
}

#[test]
#[ignore = "strict ValuesSnapshotReplacement native Memcheck L3; run scripts/verify-ordinary-map-v2-values-snapshot-replacement-l3.sh"]
fn ordinary_map_owned_any_v2_values_snapshot_replacement_cli_and_native_memcheck_are_strict() {
    const {
        assert!(
            !mimi::core::mir::ORDINARY_MAP_OWNED_ANY_V2_PRODUCTION_READY,
            "the ValuesSnapshotReplacement slice must not open global Map/Any V2 readiness"
        );
    };

    let mimi = mimi_bin();
    let evidence = EvidenceDirectory::new();
    retain_sha256(&mimi, "mimi-binary", "CARGO_BIN_EXE_mimi", &evidence.native);

    let version_directory = evidence.native.join("valgrind-version");
    create_capture_directory(&version_directory);
    let version = run_recorded(
        OsStr::new("valgrind"),
        &[OsString::from("--version")],
        &[],
        &version_directory,
    );
    assert_success(&version, "Valgrind version query");

    let positive = evidence.native.join("positive");
    create_capture_directory(&positive);
    let source = fixture("mir_ordinary_map_any_owner_v2_values_snapshot_replacement.mimi");
    let source_bytes = fs::read(&source).unwrap_or_else(|error| {
        panic!(
            "read ValuesSnapshotReplacement fixture {}: {error}",
            source.display()
        )
    });
    write_new(&positive.join("source.fixture.mimi"), &source_bytes);

    let receipt_directory = positive.join("receipt");
    create_capture_directory(&receipt_directory);
    let receipt_arguments = vec![
        OsString::from("mir"),
        OsString::from("--all"),
        OsString::from("--receipt"),
        source.as_os_str().to_owned(),
    ];
    let receipt = run_mimi(&mimi, &receipt_arguments, &[], &receipt_directory);
    let receipt_fields = parse_mir_receipt(&receipt, "mimi mir --all --receipt");
    let digest = receipt_fields["mir_digest"].clone();
    let mut observed_route_profiles = Vec::with_capacity(6);

    for direct in [false, true] {
        let mode = if direct {
            "explicit-run"
        } else {
            "default-run"
        };
        let directory = positive.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("run")];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        arguments.push(source.as_os_str().to_owned());
        let output = run_mimi(&mimi, &arguments, &cli_environment(), &directory);
        observed_route_profiles.push(assert_successful_run(&output, direct, &digest, mode));
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-build"
        } else {
            "default-build"
        };
        let directory = positive.join(mode);
        create_capture_directory(&directory);
        let binary = directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            source.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(&mimi, &arguments, &cli_environment(), &directory);
        assert_success(&output, mode);
        assert!(binary.is_file(), "{mode} did not produce a native binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(&binary)
                    .expect("inspect ValuesSnapshotReplacement native binary")
                    .permissions()
                    .mode()
                    & 0o111,
                0,
                "{mode} binary is not executable"
            );
        }
        let stderr = text(&output.stderr);
        observed_route_profiles.push(assert_route_admission(
            &stderr,
            "build",
            if direct { "explicit-mir" } else { "default" },
            &digest,
            mode,
        ));
        assert_build_receipt(
            &stderr,
            if direct { "explicit-mir" } else { "default" },
            &receipt_fields,
            mode,
        );
        assert_native_census(&stderr, &digest, mode);
        if !direct {
            assert_default_profile(&stderr, &digest, mode);
        }
        retain_sha256(&binary, "program", "mimi build --output", &directory);

        let memcheck_directory = positive.join(if direct {
            "explicit-memcheck"
        } else {
            "default-memcheck"
        });
        create_capture_directory(&memcheck_directory);
        assert_strict_memcheck(&binary, &memcheck_directory);
    }

    for direct in [false, true] {
        let mode = if direct {
            "explicit-verify"
        } else {
            "default-verify"
        };
        let directory = positive.join(mode);
        create_capture_directory(&directory);
        let mut arguments = vec![OsString::from("verify"), source.as_os_str().to_owned()];
        if direct {
            arguments.push(OsString::from("--mir"));
        }
        let output = run_mimi(&mimi, &arguments, &cli_environment(), &directory);
        assert_success(&output, mode);
        assert!(
            text(&output.stdout).contains("No contracts to verify"),
            "{mode} did not preserve the no-contract verification result: {}",
            text(&output.stdout)
        );
        let stderr = text(&output.stderr);
        observed_route_profiles.push(assert_route_admission(
            &stderr,
            "verify",
            if direct { "explicit-mir" } else { "default" },
            &digest,
            mode,
        ));
        if !direct {
            assert_default_profile(&stderr, &digest, mode);
        }
        assert_verifier_receipt(&stderr, &receipt_fields, mode);
        assert_no_legacy_fallback(&stderr, mode);
    }

    let negative_values = fixture("mir_ordinary_map_any_owner_v2_values_unpack.mimi");
    negative_case(
        &mimi,
        &evidence.native,
        &negative_values,
        "broader-values-unpack",
    );

    let altered_directory = evidence.native.join("negative/altered-index");
    fs::create_dir_all(&altered_directory)
        .expect("create altered-index negative fixture directory");
    let original = text(&source_bytes);
    let needle = "snapshot[0] as Label";
    assert_eq!(
        original.matches(needle).count(),
        1,
        "ValuesSnapshotReplacement fixture no longer has one literal index-zero Label projection"
    );
    let altered_source_text = original.replacen(needle, "snapshot[1] as Label", 1);
    let altered_source = altered_directory.join("altered-index.mimi");
    write_new(&altered_source, altered_source_text.as_bytes());
    negative_case(
        &mimi,
        &evidence.native,
        &altered_source,
        "altered-index-source",
    );

    assert_eq!(observed_route_profiles.len(), 6);
    assert!(
        observed_route_profiles
            .iter()
            .all(|profile| profile == ROUTE_PROFILE),
        "the six CLI consumers disagreed on the Checker route profile: {observed_route_profiles:?}"
    );
    let route_profile = observed_route_profiles[0].clone();
    write_new(
        &positive.join("run-summary.txt"),
        format!(
            "route_profile={route_profile}\nglobal_map_any_v2_production_ready=false\ncli_receipt_profile={}\nreceipt_schema={}\nmir_digest={}\ntype_desc_digest={}\nabi_digest={}\nffi_digest={}\nownership_digest={}\nflow_transition_digest={}\nroot_owners={}\ncase_count=1\ncli_receipt_count=1\ndefault_run_count=1\nexplicit_mir_run_count=1\nnative_build_count=2\nmemcheck_run_count=2\ndefault_native_build_receipt_count=1\nexplicit_native_build_receipt_count=1\ndefault_verify_count=1\nexplicit_mir_verify_count=1\nnegative_case_count=2\nnegative_route_attempt_count={}\nlegacy_fallback_count=0\nexpected_stdout_hex=747275650a747275650a\n",
            receipt_fields["profile"],
            receipt_fields["schema"],
            receipt_fields["mir_digest"],
            receipt_fields["type_desc_digest"],
            receipt_fields["abi_digest"],
            receipt_fields["ffi_digest"],
            receipt_fields["ownership_digest"],
            receipt_fields["flow_transition_digest"],
            receipt_fields["root_owners"],
            2 * NEGATIVE_COMMANDS_PER_CASE,
        )
        .as_bytes(),
    );
    assert!(evidence.root.is_dir());
}
