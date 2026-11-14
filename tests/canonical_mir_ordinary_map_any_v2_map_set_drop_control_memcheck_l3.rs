//! Dedicated external L3 gate for MapSetDropControl.
//!
//! This test is intentionally ignored in the ordinary Cargo suite. Run it only
//! through `scripts/verify-ordinary-map-owned-any-v2-map-set-drop-control-result-branch-l3.sh`
//! so the external target directory and evidence location are explicit.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use inkwell::context::Context;
use mimi::codegen::CodeGenerator;
use mimi::core::mir::{
    materialize_canonical_mir_route, reference::MirProgram,
    validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_mir_route,
    validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route,
    CanonicalMirRouteProfile, ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE,
};
use mimi::lexer::Lexer;

const ROUTE_PROFILE: &str = "map-any-owner-v2-map-set-drop-control-result-branch-v1";
const FIXTURE_RELATIVE_PATH: &str =
    "tests/fixtures/mir_ordinary_map_any_v2_map_set_drop_control.mimi";
// Initial source lock, captured 2026-11-13. Any intentional fixture edit must
// update this digest and state its semantic reason in the same review.
const FIXTURE_SHA256: &str = "29fcad6d4e2ec8c5e45be48ccc96a2a9d62567b9dd8c82bb2febdf8ff6c5a12b";
const EXPECTED_STDOUT: &[u8] = b"";
const ERROR_EXITCODE: &str = "99";
const GENERATED_ENTRY: &str = "mimi_l3_generated_main";
const RUNTIME_FEATURE: &str = "ordinary-map-set-result-l3";
const MAP_SET_DROP_CONTROL_HARNESS: &str =
    include_str!("fixtures/canonical_mir_map_set_drop_control_result_l3_harness.c");

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteIdentity {
    profile: String,
    mir_digest: String,
    receipt_identity_digest: String,
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

fn create_directory(path: &Path) {
    fs::create_dir(path).unwrap_or_else(|error| {
        panic!(
            "create fresh L3 evidence directory {}: {error}",
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

fn retain_process(directory: &Path, program: &OsStr, arguments: &[OsString], output: &Output) {
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
}

fn clean_mimi_environment(command: &mut Command) {
    let names = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("MIMI_"))
        .collect::<Vec<_>>();
    for name in names {
        command.env_remove(name);
    }
    command.env_remove("LD_PRELOAD");
}

fn evidence_root() -> PathBuf {
    let configured = std::env::var_os("MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR")
        .filter(|value| !value.is_empty())
        .expect("runner must set MIMI_MAP_SET_DROP_CONTROL_L3_EVIDENCE_DIR");
    let requested = PathBuf::from(configured);
    assert!(requested.is_absolute(), "L3 evidence path must be absolute");
    let metadata = fs::symlink_metadata(&requested).unwrap_or_else(|error| {
        panic!(
            "inspect runner-created L3 evidence root {}: {error}",
            requested.display()
        )
    });
    assert!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "L3 evidence root must be a real directory: {}",
        requested.display()
    );
    fs::canonicalize(&requested).unwrap_or_else(|error| {
        panic!(
            "canonicalize evidence root {}: {error}",
            requested.display()
        )
    })
}

fn executable_on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("external runner must provide PATH");
    for directory in std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()) {
        let candidate = directory.join(name);
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return fs::canonicalize(&candidate).unwrap_or_else(|error| {
            panic!(
                "canonicalize executable {name} at {}: {error}",
                candidate.display()
            )
        });
    }
    panic!("strict MapSetDropControl L3 requires {name} on PATH")
}

fn retain_sha256(path: &Path, directory: &Path) -> String {
    create_directory(directory);
    let canonical = fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()));
    let arguments = vec![OsString::from("--"), canonical.as_os_str().to_owned()];
    let output = Command::new("sha256sum")
        .args(&arguments)
        .output()
        .unwrap_or_else(|error| panic!("sha256sum {}: {error}", canonical.display()));
    retain_process(directory, OsStr::new("sha256sum"), &arguments, &output);
    assert!(
        output.status.success(),
        "sha256sum {} failed: {}",
        canonical.display(),
        text(&output.stderr)
    );
    let line = text(&output.stdout);
    let (digest, hashed_path) = line
        .trim_end()
        .split_once("  ")
        .expect("sha256sum output contains digest and path");
    assert_eq!(hashed_path, canonical.to_string_lossy());
    assert_digest(digest, "SHA-256");
    digest.to_owned()
}

fn compile_checked_native_object(
    fixture_path: &Path,
    fixture_source: &str,
    object_path: &Path,
    ir_path: &Path,
) -> RouteIdentity {
    let tokens = Lexer::new(fixture_source)
        .tokenize()
        .unwrap_or_else(|error| panic!("lex exact pinned MapSetDropControl fixture: {error}"));
    let (mut file, parse_errors) = mimi::loader::parser_for_path(tokens, fixture_path)
        .unwrap_or_else(|error| panic!("create pinned MapSetDropControl parser: {error}"))
        .parse_file_with_recovery();
    assert!(
        parse_errors.is_empty(),
        "parse exact pinned MapSetDropControl fixture: {parse_errors:?}"
    );

    // This mirrors the production CLI frontend: merge the default prelude,
    // then move main last before Checker so stable callable IDs match the
    // default and explicit --mir production route identities.
    mimi::loader::merge_prelude_into(&mut file);
    if let Some(main_index) = file
        .items
        .iter()
        .position(|item| matches!(item, mimi::ast::Item::Func(function) if function.name == "main"))
    {
        let main_item = file.items.remove(main_index);
        file.items.push(main_item);
    }
    let checked = mimi::core::check_program(&file).unwrap_or_else(|errors| {
        panic!("Checker rejected exact pinned MapSetDropControl fixture: {errors:?}")
    });

    let excluded_sources = file
        .sources
        .records()
        .iter()
        .filter(|record| record.key.as_str() == "stdlib:prelude.mimi")
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    let materialized = materialize_canonical_mir_route(&checked, Some(&excluded_sources))
        .unwrap_or_else(|error| {
            panic!("materialize production MapSetDropControl Result route: {error}")
        });
    assert!(
        materialized
            .materialized_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_candidate,
        "production materializer did not mark the dedicated MapSetDropControl Result candidate"
    );
    assert_eq!(
        materialized.admission.ordinary_map_owned_any_v2,
        mimi::core::mir::OrdinaryMapOwnedAnyV2Admission::MapSetDropControlResult,
        "production materializer selected a different owner-v2 profile"
    );

    // Independently rebuild the route from the exact CheckedProgram, then
    // run both public validators. The materializer is evidence, not the
    // validator: all three graph views must agree before native emission.
    let checked_projection =
        MirProgram::from_checked_program_ordinary_map_owned_any_v2_map_set_drop_control_result(
            &checked,
        )
        .unwrap_or_else(|error| {
            panic!("build checked MapSetDropControl Result MIR projection: {error}")
        });
    assert_eq!(
        materialized.program, checked_projection,
        "production materialization differs from the dedicated CheckedProgram projection"
    );
    let checked_receipt =
        validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route(
            &checked,
            &materialized.program,
        )
        .unwrap_or_else(|error| {
            panic!("CheckedProgram→MIR Result route validation failed: {error}")
        });
    let mir_receipt =
        validate_ordinary_map_owned_any_v2_map_set_drop_control_result_branch_mir_route(
            &materialized.program,
        )
        .unwrap_or_else(|error| panic!("MIR-only Result route validation failed: {error}"));
    let materializer_receipt = materialized
        .ordinary_map_owned_any_v2_map_set_drop_control_result_branch_route_receipt
        .as_ref()
        .unwrap_or_else(|| panic!("dedicated Result materialization omitted its route receipt"));
    assert!(
        checked_receipt.same_route_identity(&mir_receipt)
            && checked_receipt.same_route_identity(materializer_receipt),
        "CheckedProgram, MIR-only, and materializer Result receipts differ"
    );
    checked_receipt
        .validate_against_program(&materialized.program)
        .unwrap_or_else(|error| panic!("dedicated Result route receipt rejected its MIR: {error}"));
    assert_eq!(
        checked_receipt.profile,
        CanonicalMirRouteProfile::OrdinaryMapOwnedAnyV2MapSetDropControlResult.as_str(),
        "dedicated Result receipt has the wrong canonical profile"
    );
    assert_eq!(
        checked_receipt.profile,
        ORDINARY_MAP_OWNED_ANY_V2_MAP_SET_DROP_CONTROL_RESULT_BRANCH_PROFILE,
        "dedicated Result profile constant and receipt disagree"
    );

    let identity = RouteIdentity {
        profile: checked_receipt.profile.clone(),
        mir_digest: checked_receipt.mir_digest.clone(),
        receipt_identity_digest: checked_receipt.route_identity_digest(),
    };

    let prior_opt = std::env::var_os("MIMI_OPT");
    let prior_dump_module = std::env::var_os("MIMI_DUMP_MODULE");
    std::env::set_var("MIMI_OPT", "0");
    std::env::remove_var("MIMI_DUMP_MODULE");
    {
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "native_map_set_drop_control_result_l3");
        generator
            .preflight_checked_native_data_exports(&checked)
            .unwrap_or_else(|errors| panic!("preflight native checked data exports: {errors:?}"));
        generator
            .compile_checked_mir_native_with_map_set_drop_control_result_route(
                &checked,
                &materialized.program,
                &checked_receipt,
            )
            .unwrap_or_else(|errors| panic!("emit dedicated native Result route: {errors:?}"));
        generator
            .emit_checked_native_data_exports(&checked)
            .unwrap_or_else(|errors| panic!("emit native checked data exports: {errors:?}"));
        generator.module.verify().unwrap_or_else(|error| {
            panic!("verify native MapSetDropControl Result module: {error}")
        });

        let main = generator
            .module
            .get_function("main")
            .unwrap_or_else(|| panic!("native Result module has no main"));
        assert_eq!(
            main.count_params(),
            0,
            "native Result entry must take no parameters"
        );
        assert_eq!(
            main.get_type().get_return_type(),
            Some(context.i32_type().into()),
            "native Result entry must use the exact () -> i32 ABI"
        );
        rename_function(&main, GENERATED_ENTRY);
        generator
            .module
            .verify()
            .unwrap_or_else(|error| panic!("verify renamed native Result module: {error}"));
        assert!(
            generator.module.get_function(GENERATED_ENTRY).is_some(),
            "renamed generated native entry is missing"
        );
        generator
            .module
            .print_to_file(ir_path)
            .unwrap_or_else(|error| panic!("write direct native Result LLVM IR: {error}"));
        generator
            .compile_to_object(object_path)
            .unwrap_or_else(|error| panic!("compile direct native Result object: {error}"));
    }
    match prior_opt {
        Some(value) => std::env::set_var("MIMI_OPT", value),
        None => std::env::remove_var("MIMI_OPT"),
    }
    match prior_dump_module {
        Some(value) => std::env::set_var("MIMI_DUMP_MODULE", value),
        None => std::env::remove_var("MIMI_DUMP_MODULE"),
    }
    identity
}

fn rename_function(function: &inkwell::values::FunctionValue<'_>, name: &str) {
    use inkwell::llvm_sys::core::LLVMSetValueName2;
    use inkwell::values::AsValueRef;

    // SAFETY: `function` belongs to the live module and LLVM copies the exact
    // byte span before returning.
    unsafe {
        LLVMSetValueName2(
            function.as_value_ref(),
            name.as_ptr() as *const std::os::raw::c_char,
            name.len(),
        );
    }
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

fn parse_fields<'a>(
    tokens: impl Iterator<Item = &'a str>,
    label: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for token in tokens {
        let (key, value) = token
            .split_once('=')
            .unwrap_or_else(|| panic!("{label} contains malformed route field {token:?}"));
        assert!(
            fields.insert(key.to_owned(), value.to_owned()).is_none(),
            "{label} repeats route field {key}"
        );
    }
    fields
}

fn one_route_line<'a>(stderr: &'a str, prefix: &str, label: &str) -> &'a str {
    let lines = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .collect::<Vec<_>>();
    let [line] = lines.as_slice() else {
        panic!("{label} requires exactly one {prefix:?} receipt; stderr:\n{stderr}");
    };
    line
}

fn build_identity(stderr: &str, explicit_mir: bool, label: &str) -> (String, String) {
    assert!(
        !stderr.contains("canonical route disposition: legacy"),
        "{label} selected a legacy fallback:\n{stderr}"
    );
    let mode = if explicit_mir {
        "explicit-mir"
    } else {
        "default"
    };
    let route_line = one_route_line(stderr, "MIMI_ROUTE_CENSUS route-admission ", label);
    let route_fields = parse_fields(
        route_line
            .strip_prefix("MIMI_ROUTE_CENSUS route-admission ")
            .expect("route-admission prefix")
            .split_whitespace(),
        label,
    );
    assert_eq!(
        route_fields.get("consumer").map(String::as_str),
        Some("build")
    );
    assert_eq!(route_fields.get("mode").map(String::as_str), Some(mode));
    assert_eq!(
        route_fields.get("checker_profile").map(String::as_str),
        Some(ROUTE_PROFILE),
        "{label} did not select the dedicated MapSetDropControl Result profile"
    );
    let mir_digest = route_fields
        .get("mir_digest")
        .unwrap_or_else(|| panic!("{label} route admission lacks mir_digest: {route_line}"));
    let receipt_digest = route_fields
        .get("receipt_identity_digest")
        .unwrap_or_else(|| panic!("{label} route admission lacks receipt identity: {route_line}"));
    assert_digest(mir_digest, &format!("{label} MIR digest"));
    assert_digest(receipt_digest, &format!("{label} receipt identity digest"));

    let native_line = one_route_line(stderr, "MIMI_ROUTE_CENSUS canonical-native-build ", label);
    let native_fields = parse_fields(
        native_line
            .strip_prefix("MIMI_ROUTE_CENSUS canonical-native-build ")
            .expect("canonical-native-build prefix")
            .split_whitespace(),
        label,
    );
    assert_eq!(
        native_fields.get("consumer").map(String::as_str),
        Some(mode)
    );
    assert_eq!(
        native_fields.get("route_id").map(String::as_str),
        Some(ROUTE_PROFILE)
    );
    assert_eq!(
        native_fields.get("profile").map(String::as_str),
        Some(ROUTE_PROFILE)
    );
    assert_eq!(native_fields.get("mir_digest"), Some(mir_digest));
    assert_eq!(
        native_fields.get("route_identity_digest"),
        Some(receipt_digest),
        "{label} native receipt differs from route admission"
    );

    let census = one_route_line(stderr, "info: canonical MIR native compiled ", label);
    let (counts, census_digest) = census
        .strip_prefix("info: canonical MIR native compiled ")
        .expect("native census prefix")
        .split_once(" function(s), 0 fell back; mir_digest=")
        .unwrap_or_else(|| panic!("{label} native census does not report zero fallback: {census}"));
    let (emitted, total) = counts
        .split_once('/')
        .unwrap_or_else(|| panic!("{label} native census lacks emitted/total counts: {census}"));
    let emitted = emitted
        .parse::<usize>()
        .unwrap_or_else(|error| panic!("{label} invalid emitted count: {error}"));
    let total = total
        .parse::<usize>()
        .unwrap_or_else(|error| panic!("{label} invalid total count: {error}"));
    assert!(total > 0, "{label} native MIR census is empty");
    assert_eq!(emitted, total, "{label} omitted native MIR functions");
    assert_eq!(
        census_digest, mir_digest,
        "{label} native census digest differs"
    );

    if !explicit_mir {
        let expected = format!(
            "canonical route disposition: canonical ({ROUTE_PROFILE}) mir_digest={mir_digest}"
        );
        assert_eq!(
            stderr.lines().filter(|line| *line == expected).count(),
            1,
            "{label} default build lacks its exact canonical route disposition"
        );
    }

    (mir_digest.clone(), receipt_digest.clone())
}

fn summary_row<'a>(report: &'a str, marker: &str, label: &str) -> &'a str {
    let rows = report
        .lines()
        .filter(|line| {
            let body = line
                .strip_prefix("==")
                .and_then(|line| line.split_once("==").map(|(_, body)| body.trim_start()))
                .unwrap_or(line.trim());
            body.starts_with(marker)
        })
        .collect::<Vec<_>>();
    let [row] = rows.as_slice() else {
        panic!(
            "{label} must contain one {marker:?} Memcheck row, found {}:\n{report}",
            rows.len()
        );
    };
    row
}

fn report_body(line: &str) -> &str {
    line.strip_prefix("==")
        .and_then(|line| line.split_once("==").map(|(_, body)| body.trim_start()))
        .unwrap_or(line.trim())
}

fn validate_memcheck(report: &str, label: &str) -> String {
    for marker in ["definitely lost:", "indirectly lost:", "possibly lost:"] {
        let row = summary_row(report, marker, label);
        let body = report_body(row);
        let count = body
            .strip_prefix(marker)
            .expect("matched loss marker")
            .trim();
        let zero = count.starts_with("0 bytes in 0 blocks");
        assert!(zero, "{label} has {marker} allocations: {row}\n{report}");
    }
    let error_row = summary_row(report, "ERROR SUMMARY:", label);
    assert_eq!(
        report_body(error_row),
        "ERROR SUMMARY: 0 errors from 0 contexts (suppressed: 0 from 0)",
        "{label} has Memcheck errors or suppressions:\n{report}"
    );
    let reachable = report
        .lines()
        .filter(|line| report_body(line).starts_with("still reachable:"))
        .map(report_body)
        .collect::<Vec<_>>();
    let [reachable] = reachable.as_slice() else {
        panic!("{label} requires one still-reachable row:\n{report}");
    };
    format!(
        "{}\n{}\n{}\n{}\n",
        report_body(summary_row(report, "definitely lost:", label)),
        report_body(summary_row(report, "indirectly lost:", label)),
        report_body(summary_row(report, "possibly lost:", label)),
        reachable,
    )
}

fn run_native_memcheck(
    valgrind: &Path,
    binary: &Path,
    directory: &Path,
    label: &str,
    expected_stdout: &[u8],
) {
    let home = directory.join("isolated-home");
    create_directory(&home);
    let canonical_binary = fs::canonicalize(binary)
        .unwrap_or_else(|error| panic!("canonicalize native binary {}: {error}", binary.display()));
    let log = directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--command-line-only=yes"),
        OsString::from("--default-suppressions=no"),
        OsString::from("--tool=memcheck"),
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from(format!("--error-exitcode={ERROR_EXITCODE}")),
        OsString::from(format!("--log-file={}", log.display())),
        canonical_binary.as_os_str().to_owned(),
    ];
    write_new(
        &directory.join("memcheck.environment.txt"),
        format!(
            "HOME={}\nLANG=C\nLC_ALL=C\nMIMI_environment=unset\nLD_PRELOAD=unset\nVALGRIND_OPTS=unset\nVALGRIND_LIB=unset\n",
            home.display()
        )
        .as_bytes(),
    );
    let mut command = Command::new(valgrind);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("HOME", &home)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(&home)
        .args(&arguments);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run {label} under strict Valgrind Memcheck: {error}"));
    retain_process(directory, valgrind.as_os_str(), &arguments, &output);
    let report = fs::read(&log).unwrap_or_else(|error| {
        panic!("read mandatory Valgrind report {}: {error}", log.display())
    });
    assert!(!report.is_empty(), "{label} Valgrind report is empty");
    let report_text = text(&report);
    let summary = validate_memcheck(&report_text, label);
    write_new(&directory.join("memcheck-summary.txt"), summary.as_bytes());
    assert_eq!(
        output.status.code(),
        Some(0),
        "{label} native exit or Valgrind status was not zero:\n{}\n{}",
        process_status(&output),
        report_text
    );
    assert_eq!(output.stdout.as_slice(), expected_stdout, "{label} stdout");
    assert!(
        output.stderr.is_empty(),
        "{label} emitted native stderr: {}",
        text(&output.stderr)
    );
}

fn run_recorded_tool(
    label: &str,
    program: &OsStr,
    arguments: &[OsString],
    working_directory: &Path,
    evidence_directory: &Path,
) -> Output {
    create_directory(evidence_directory);
    write_new(
        &evidence_directory.join("working-directory.txt"),
        format!("{}\n", working_directory.display()).as_bytes(),
    );
    let mut command = Command::new(program);
    clean_mimi_environment(&mut command);
    command.current_dir(working_directory).args(arguments);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run {label} ({program:?}): {error}"));
    retain_process(evidence_directory, program, arguments, &output);
    output
}

fn require_tool_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn expected_error_harness_stdout(new_error: bool) -> Vec<u8> {
    let summary = if new_error {
        "native-map-set-drop-control-result mode=new-err generated_status=3 warmup_new_status=0 warmup_root_nonzero=1 warmup_drop_status=0 fixture_new_calls=1 fixture_new_status=3 fixture_new_output_zero=1 fixture_set_calls=0 fixture_set_status=-2147483648 fixture_set_output_zero=-1 fixture_set_source_before=-2147483648/-2147483648 fixture_set_source_after=-2147483648/-2147483648 new_observer_calls=2 new_observer_status=3 new_observer_output_zero=1 new_roots_after_new=0 new_publish_checkpoints=2 shutdown_registered=1 final_roots=0 final_map_handles=0 final_owned_get_handles=0 final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n"
    } else {
        "native-map-set-drop-control-result mode=set-err generated_status=2 fixture_new_calls=1 fixture_new_status=0 fixture_new_output_zero=0 fixture_set_calls=1 fixture_set_status=2 fixture_set_output_zero=1 fixture_set_source_before=0/0 fixture_set_source_after=0/0 new_observer_calls=1 new_observer_status=0 new_observer_output_zero=0 new_roots_after_new=1 new_publish_checkpoints=1 shutdown_registered=1 final_roots=0 final_map_handles=0 final_owned_get_handles=0 final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n"
    };
    summary.as_bytes().to_vec()
}

fn build_feature_runtime_archive(repo: &Path, build: &Path, commands: &Path) -> PathBuf {
    let runtime_source = repo.join("src/runtime/standalone.rs");
    let archive = build.join("libmimi_runtime_l3.a");
    let args = vec![
        OsString::from("--edition"),
        OsString::from("2021"),
        OsString::from("--crate-type"),
        OsString::from("staticlib"),
        OsString::from("--cfg"),
        OsString::from("standalone"),
        OsString::from("--cfg"),
        OsString::from(format!("feature=\"{RUNTIME_FEATURE}\"")),
        OsString::from("--crate-name"),
        OsString::from("mimi_runtime"),
        OsString::from("-A"),
        OsString::from("dead_code"),
        OsString::from("-o"),
        archive.as_os_str().to_owned(),
        runtime_source.as_os_str().to_owned(),
    ];
    let output = run_recorded_tool(
        "feature-only standalone runtime archive",
        OsStr::new("rustc"),
        &args,
        repo,
        &commands.join("rustc-runtime-archive"),
    );
    require_tool_success("feature-only standalone runtime archive", &output);
    assert!(archive.is_file(), "rustc omitted {}", archive.display());
    archive
}

fn compile_error_harness_object(
    repo: &Path,
    c_source: &Path,
    object: &Path,
    commands: &Path,
    new_error: bool,
) {
    let mode = if new_error { "new-err" } else { "set-err" };
    let args = vec![
        OsString::from("-std=c11"),
        OsString::from("-Wall"),
        OsString::from("-Wextra"),
        OsString::from("-Werror"),
        OsString::from("-c"),
        OsString::from(format!(
            "-DMIMI_L3_NEW_ERR={}",
            if new_error { 1 } else { 0 }
        )),
        c_source.as_os_str().to_owned(),
        OsString::from("-o"),
        object.as_os_str().to_owned(),
    ];
    let output = run_recorded_tool(
        &format!("compile {mode} C harness"),
        OsStr::new("cc"),
        &args,
        repo,
        &commands.join(format!("cc-harness-{mode}")),
    );
    require_tool_success(&format!("compile {mode} C harness"), &output);
}

fn link_error_harness(
    repo: &Path,
    generated_object: &Path,
    harness_object: &Path,
    runtime_archive: &Path,
    binary: &Path,
    commands: &Path,
    new_error: bool,
) {
    let mode = if new_error { "new-err" } else { "set-err" };
    let args = vec![
        OsString::from("-no-pie"),
        generated_object.as_os_str().to_owned(),
        harness_object.as_os_str().to_owned(),
        runtime_archive.as_os_str().to_owned(),
        OsString::from("-Wl,--wrap=mimi_ordinary_map_owned_any_v2_new"),
        OsString::from("-Wl,--wrap=mimi_ordinary_map_owned_any_v2_set"),
        OsString::from("-o"),
        binary.as_os_str().to_owned(),
        OsString::from("-lpthread"),
        OsString::from("-ldl"),
        OsString::from("-lm"),
    ];
    let output = run_recorded_tool(
        &format!("link {mode} native Result executable"),
        OsStr::new("cc"),
        &args,
        repo,
        &commands.join(format!("cc-link-{mode}")),
    );
    require_tool_success(&format!("link {mode} native Result executable"), &output);
    assert!(binary.is_file(), "linker omitted {}", binary.display());
}

fn build_and_memcheck_error_children(
    repo: &Path,
    fixture: &Path,
    fixture_source: &str,
    valgrind: &Path,
    evidence: &Path,
) -> RouteIdentity {
    let direct = evidence.join("native/direct-route");
    let build = direct.join("build");
    let commands = direct.join("commands");
    let new_error = direct.join("new-err");
    let set_error = direct.join("set-err");
    create_directory(&direct);
    create_directory(&build);
    create_directory(&commands);
    create_directory(&new_error);
    create_directory(&set_error);
    let generated_object = build.join("map_set_drop_control_result.o");
    let generated_ir = build.join("map_set_drop_control_result.ll");
    let route_identity =
        compile_checked_native_object(fixture, fixture_source, &generated_object, &generated_ir);
    assert_eq!(route_identity.profile, ROUTE_PROFILE);
    write_new(
        &direct.join("route.identity.txt"),
        format!(
            "profile={}\nmir_digest={}\nreceipt_identity_digest={}\nsource_sha256={}\nprojection=CheckedProgram→dedicated-MIR-validator→MIR-only-validator→production-native-entry\n",
            route_identity.profile, route_identity.mir_digest,
            route_identity.receipt_identity_digest, FIXTURE_SHA256,
        )
        .as_bytes(),
    );
    let generated_object_sha =
        retain_sha256(&generated_object, &build.join("generated-object-sha256"));
    let generated_ir_sha = retain_sha256(&generated_ir, &build.join("generated-ir-sha256"));
    write_new(
        &build.join("generated-object.identity.txt"),
        format!(
            "path={}\nsha256={}\nllvm_ir={}\nllvm_ir_sha256={}\n",
            generated_object.display(),
            generated_object_sha,
            generated_ir.display(),
            generated_ir_sha,
        )
        .as_bytes(),
    );

    let runtime_archive = build_feature_runtime_archive(repo, &build, &commands);
    let runtime_archive_sha =
        retain_sha256(&runtime_archive, &build.join("runtime-archive-sha256"));
    write_new(
        &build.join("runtime-archive.identity.txt"),
        format!(
            "path={}\nsha256={}\nfeature=standalone+feature=\"{RUNTIME_FEATURE}\"\n",
            runtime_archive.display(),
            runtime_archive_sha
        )
        .as_bytes(),
    );
    let harness_path = build.join("canonical_mir_map_set_drop_control_result_l3_harness.c");
    let harness_source_path =
        repo.join("tests/fixtures/canonical_mir_map_set_drop_control_result_l3_harness.c");
    let on_disk_harness = fs::read(&harness_source_path).unwrap_or_else(|error| {
        panic!(
            "read exact C harness {}: {error}",
            harness_source_path.display()
        )
    });
    assert_eq!(
        on_disk_harness.as_slice(),
        MAP_SET_DROP_CONTROL_HARNESS.as_bytes(),
        "C harness file differs from the source pinned into the ignored L3 test"
    );
    let harness_source_sha =
        retain_sha256(&harness_source_path, &direct.join("harness-source-sha256"));
    write_new(
        &direct.join("harness-source.identity.txt"),
        format!(
            "path={}\nsha256={}\nembedded_bytes_equal=true\n",
            harness_source_path.display(),
            harness_source_sha
        )
        .as_bytes(),
    );
    write_new(&harness_path, MAP_SET_DROP_CONTROL_HARNESS.as_bytes());

    for (mode, new_err, mode_directory) in [
        ("new-err", true, &new_error),
        ("set-err", false, &set_error),
    ] {
        let shim_object = mode_directory.join("harness.o");
        compile_error_harness_object(repo, &harness_path, &shim_object, &commands, new_err);
        let binary = mode_directory.join("program");
        link_error_harness(
            repo,
            &generated_object,
            &shim_object,
            &runtime_archive,
            &binary,
            &commands,
            new_err,
        );
        let binary_sha = retain_sha256(&binary, &mode_directory.join("program-sha256"));
        let expected_stdout = expected_error_harness_stdout(new_err);
        write_new(
            &mode_directory.join("expected-stdout.bin"),
            &expected_stdout,
        );
        write_new(
            &mode_directory.join("program.identity.txt"),
            format!(
                "mode={mode}\nprogram_sha256={binary_sha}\nroute_profile={}\nmir_digest={}\nreceipt_identity_digest={}\nexpected_generated_status={}\nlegacy_fallback_count=0\n",
                route_identity.profile,
                route_identity.mir_digest,
                route_identity.receipt_identity_digest,
                if new_err { 3 } else { 2 },
            )
            .as_bytes(),
        );
        run_native_memcheck(
            valgrind,
            &binary,
            &mode_directory.join("memcheck"),
            &format!("direct {mode}"),
            &expected_stdout,
        );
    }
    route_identity
}

#[test]
#[ignore = "requires both dedicated native routes and strict external Valgrind; invoke the L3 runner"]
fn ordinary_map_owned_any_v2_map_set_drop_control_default_and_explicit_native_memcheck_clean() {
    let evidence = evidence_root();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = repo.join(FIXTURE_RELATIVE_PATH);
    let native = evidence.join("native");
    create_directory(&native);

    let fixture_copy = fs::read(&fixture).expect("read pinned MapSetDropControl fixture");
    let fixture_source = std::str::from_utf8(&fixture_copy)
        .expect("pinned MapSetDropControl fixture is valid UTF-8");
    write_new(&evidence.join("source.fixture.mimi"), &fixture_copy);
    let fixture_hash_directory = evidence.join("fixture-sha256");
    let fixture_hash = retain_sha256(&fixture, &fixture_hash_directory);
    write_new(
        &evidence.join("source.fixture.identity.txt"),
        format!(
            "fixture_relative_path={FIXTURE_RELATIVE_PATH}\nfixture_sha256={fixture_hash}\nexpected_fixture_sha256={FIXTURE_SHA256}\nsource_lock_reason=initial exact-source lock captured 2026-11-13; fixture edits require an explicit semantic reason\n"
        )
        .as_bytes(),
    );
    assert_eq!(
        fixture_hash, FIXTURE_SHA256,
        "MapSetDropControl source changed; update the pin only with an explicit semantic reason"
    );

    let mimi = PathBuf::from(env!("CARGO_BIN_EXE_mimi"));
    let mimi_hash = retain_sha256(&mimi, &native.join("mimi-binary-sha256"));
    write_new(
        &native.join("mimi-binary.identity.txt"),
        format!(
            "path={}\nsha256={mimi_hash}\n",
            fs::canonicalize(&mimi).unwrap().display()
        )
        .as_bytes(),
    );

    let valgrind = executable_on_path("valgrind");
    let valgrind_hash = retain_sha256(&valgrind, &native.join("valgrind-sha256"));
    let version_directory = native.join("valgrind-version");
    create_directory(&version_directory);
    let version_arguments = vec![OsString::from("--version")];
    let version = Command::new(&valgrind)
        .args(&version_arguments)
        .output()
        .unwrap_or_else(|error| panic!("query Valgrind version: {error}"));
    retain_process(
        &version_directory,
        valgrind.as_os_str(),
        &version_arguments,
        &version,
    );
    assert!(
        version.status.success(),
        "Valgrind --version failed: {}",
        text(&version.stderr)
    );
    let version_text = text(&version.stdout).trim().to_owned();
    assert!(
        version_text.starts_with("valgrind-"),
        "unexpected Valgrind version: {version_text}"
    );
    write_new(
        &native.join("valgrind.identity.txt"),
        format!(
            "path={}\nsha256={valgrind_hash}\nversion={version_text}\n",
            valgrind.display()
        )
        .as_bytes(),
    );

    let mut identities = Vec::new();
    for (mode, explicit_mir) in [("default", false), ("explicit-mir", true)] {
        let mode_directory = native.join(mode);
        create_directory(&mode_directory);
        let binary = mode_directory.join("program");
        let mut arguments = vec![
            OsString::from("build"),
            fixture.as_os_str().to_owned(),
            OsString::from("--output"),
            binary.as_os_str().to_owned(),
        ];
        if explicit_mir {
            arguments.push(OsString::from("--mir"));
        }
        let mut command = Command::new(&mimi);
        clean_mimi_environment(&mut command);
        command
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_VERBOSE", "1")
            .current_dir(&repo)
            .args(&arguments);
        let build = command
            .output()
            .unwrap_or_else(|error| panic!("run {mode} Mimi native build: {error}"));
        retain_process(&mode_directory, mimi.as_os_str(), &arguments, &build);
        write_new(
            &mode_directory.join("build.environment.txt"),
            b"MIMI_environment=unset_except_MIMI_ROUTE_CENSUS_and_MIMI_VERBOSE\nMIMI_ROUTE_CENSUS=1\nMIMI_VERBOSE=1\nLD_PRELOAD=unset\n",
        );
        assert!(
            build.status.success(),
            "{mode} native build failed:\nstdout:\n{}\nstderr:\n{}",
            text(&build.stdout),
            text(&build.stderr)
        );
        assert!(
            binary.is_file(),
            "{mode} native build omitted {}",
            binary.display()
        );
        let (mir_digest, receipt_digest) = build_identity(&text(&build.stderr), explicit_mir, mode);
        let binary_hash = retain_sha256(&binary, &mode_directory.join("program-sha256"));
        write_new(
            &mode_directory.join("build.identity.txt"),
            format!(
                "mode={mode}\nprofile={ROUTE_PROFILE}\nmir_digest={mir_digest}\nreceipt_identity_digest={receipt_digest}\nprogram_sha256={binary_hash}\n"
            )
            .as_bytes(),
        );
        identities.push((mode.to_owned(), mir_digest, receipt_digest));
        let memcheck_directory = mode_directory.join("memcheck");
        create_directory(&memcheck_directory);
        run_native_memcheck(
            &valgrind,
            &binary,
            &memcheck_directory,
            mode,
            EXPECTED_STDOUT,
        );
    }

    assert_eq!(identities.len(), 2);
    assert_eq!(
        identities[0].1, identities[1].1,
        "default and --mir MIR digests differ"
    );
    assert_eq!(
        identities[0].2, identities[1].2,
        "default and --mir receipt identity digests differ"
    );
    let direct_identity =
        build_and_memcheck_error_children(&repo, &fixture, fixture_source, &valgrind, &evidence);
    assert_eq!(
        direct_identity.profile, ROUTE_PROFILE,
        "direct Checker/MIR/native route selected a different production profile"
    );
    for (mode, mir_digest, receipt_identity_digest) in &identities {
        assert_eq!(
            &direct_identity.mir_digest, mir_digest,
            "direct native route MIR digest differs from {mode} production build"
        );
        assert_eq!(
            &direct_identity.receipt_identity_digest, receipt_identity_digest,
            "direct native route receipt differs from {mode} production build"
        );
    }
    write_new(
        &evidence.join("l3-summary.txt"),
        format!(
            "route_profile={ROUTE_PROFILE}\nfixture_sha256={fixture_hash}\nproduction_cli_native_build_count=2\ndirect_checked_native_object_count=1\nstrict_memcheck_count=4\nerror_harness_native_runs=2\nexpected_exit_code=0\nproduction_cli_expected_stdout_hex=\nerror_harness_stdout_checked_exact=true\ndefault_explicit_mir_digest_equal=true\ndefault_explicit_receipt_identity_equal=true\ndirect_receipt_matches_default_and_explicit=true\ndefault_mir_digest={}\nexplicit_mir_digest={}\ndirect_mir_digest={}\ndefault_receipt_identity_digest={}\nexplicit_receipt_identity_digest={}\ndirect_receipt_identity_digest={}\nnew_err_expected_program_status=3\nset_err_expected_program_status=2\nlegacy_fallback_count=0\n",
            identities[0].1,
            identities[1].1,
            direct_identity.mir_digest,
            identities[0].2,
            identities[1].2,
            direct_identity.receipt_identity_digest,
        )
        .as_bytes(),
    );
}
