//! Standalone native SetResult Memcheck gate.
//!
//! Cargo runs this no-harness driver outside the measured process. It compiles
//! the checked SetLineage route to a persistent LLVM object, builds the exact
//! runtime archive, links two real executables, and invokes only those native
//! executables under strict Memcheck.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

use inkwell::context::Context;
use mimi::codegen::CodeGenerator;
use mimi::core::mir::{
    materialize_ordinary_map_set_result_route, ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE,
};
use mimi::lexer::Lexer;
use mimi::parser::Parser;

const EVIDENCE_ENV: &str = "MIMI_ORDINARY_MAP_SET_RESULT_L3_EVIDENCE";
const SET_RESULT_FIXTURE: &str =
    include_str!("fixtures/mir_ordinary_map_owned_any_v2_set_result_lineage.mimi");
const GENERATED_ENTRY: &str = "mimi_l3_generated_main";
const RUNTIME_FEATURE: &str = "ordinary-map-set-result-l3";
const ROUTE_PROFILE: &str = "ordinary-map-set-result-v1";

const DRIVER_SOURCE: &str = r#"
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

struct mimi_set_result_l3_observation {
    uint64_t set_calls;
    int32_t source_before_status;
    int32_t source_before_size;
    int32_t source_after_status;
    int32_t source_after_size;
    int32_t set_status;
    int32_t output_root_zero;
    uint64_t roots_after_set;
    uint64_t managed_any_after_set;
    uint64_t any_owners_after_set;
    uint64_t root_publish_attempts;
    int32_t shutdown_registered;
    uint64_t final_roots;
    uint64_t final_ordinary_map_handles;
    uint64_t final_owned_get_handles;
    uint64_t final_runtime_map_handles;
    uint64_t final_managed_any;
    uint64_t final_owned_strings;
    uint64_t final_any_owner_tokens;
    uint64_t final_lists;
};
_Static_assert(sizeof(struct mimi_set_result_l3_observation) == 136,
               "Rust/C L3 observation ABI changed");

extern int32_t mimi_l3_generated_main(void);
extern int32_t mimi_ordinary_map_set_result_l3_install_capacity_failure(void);
extern int32_t mimi_ordinary_map_set_result_l3_read_observation(
    struct mimi_set_result_l3_observation *out_observation);

int main(void) {
#if MIMI_L3_CAPACITY
    if (mimi_ordinary_map_set_result_l3_install_capacity_failure() != 0) {
        fputs("SetResult L3 failpoint installation failed\n", stderr);
        return 90;
    }
#endif

    const int32_t generated_status = mimi_l3_generated_main();
    struct mimi_set_result_l3_observation observation = {0};
    const int32_t observation_status =
        mimi_ordinary_map_set_result_l3_read_observation(&observation);
    const char *mode = MIMI_L3_CAPACITY ? "capacity" : "success";

    printf("native-map-set-result mode=%s set_calls=%" PRIu64
           " generated_status=%" PRId32
           " set_status=%" PRId32 " source_before=%" PRId32 "/%" PRId32
           " source_after=%" PRId32 "/%" PRId32 " output_zero=%" PRId32
           " roots_after_set=%" PRIu64 " managed_any_after_set=%" PRIu64
           " any_owners_after_set=%" PRIu64 " publish_attempts=%" PRIu64
           " shutdown_registered=%" PRId32 " final_roots=%" PRIu64
           " final_map_handles=%" PRIu64 " final_owned_get_handles=%" PRIu64
           " final_runtime_map_handles=%" PRIu64 " final_managed_any=%" PRIu64
           " final_owned_strings=%" PRIu64 " final_any_owner_tokens=%" PRIu64
           " final_lists=%" PRIu64 "\n",
           mode, observation.set_calls, generated_status, observation.set_status,
           observation.source_before_status, observation.source_before_size,
           observation.source_after_status, observation.source_after_size,
           observation.output_root_zero, observation.roots_after_set,
           observation.managed_any_after_set, observation.any_owners_after_set,
           observation.root_publish_attempts, observation.shutdown_registered,
           observation.final_roots, observation.final_ordinary_map_handles,
           observation.final_owned_get_handles,
           observation.final_runtime_map_handles, observation.final_managed_any,
           observation.final_owned_strings, observation.final_any_owner_tokens,
           observation.final_lists);

    const int32_t expected_result = MIMI_L3_CAPACITY ? 2 : 1;
    const int32_t expected_set_status = MIMI_L3_CAPACITY ? 2 : 0;
    const int32_t expected_output_zero = MIMI_L3_CAPACITY ? 1 : 0;
    const uint64_t expected_roots_after_set = MIMI_L3_CAPACITY ? 1 : 2;
    const uint64_t expected_managed_any_after_set = 0;
    const uint64_t expected_any_owners_after_set = MIMI_L3_CAPACITY ? 0 : 1;
    const uint64_t expected_publish_attempts = MIMI_L3_CAPACITY ? 2 : 0;

    const int observation_matches =
        observation_status == 0 && observation.set_calls == 1 &&
        observation.source_before_status == 0 && observation.source_before_size == 0 &&
        observation.source_after_status == 0 && observation.source_after_size == 0 &&
        observation.set_status == expected_set_status &&
        observation.output_root_zero == expected_output_zero &&
        observation.roots_after_set == expected_roots_after_set &&
        observation.managed_any_after_set == expected_managed_any_after_set &&
        observation.any_owners_after_set == expected_any_owners_after_set &&
        observation.root_publish_attempts == expected_publish_attempts &&
        observation.shutdown_registered == 1 && observation.final_roots == 0 &&
        observation.final_ordinary_map_handles == 0 &&
        observation.final_owned_get_handles == 0 &&
        observation.final_runtime_map_handles == 0 &&
        observation.final_managed_any == 0 && observation.final_owned_strings == 0 &&
        observation.final_any_owner_tokens == 0 && observation.final_lists == 0;

    if (generated_status != expected_result || !observation_matches) {
        fprintf(stderr,
                "SetResult L3 invariant failure: observation_status=%" PRId32
                " generated_status=%" PRId32 " expected=%" PRId32 "\n",
                observation_status, generated_status, expected_result);
        return 91;
    }
    return 0;
}
"#;

fn main() {
    if let Err(error) = run_parent() {
        eprintln!("ordinary-map-set-result-l3: {error}");
        std::process::exit(1);
    }
}

fn run_parent() -> Result<(), String> {
    let evidence_root = std::env::var_os(EVIDENCE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{EVIDENCE_ENV} must name the central evidence directory"))?;
    std::fs::create_dir_all(&evidence_root).map_err(|error| {
        format!(
            "create evidence directory {}: {error}",
            evidence_root.display()
        )
    })?;

    let l3_root = evidence_root.join("native-l3-standalone");
    std::fs::create_dir_all(&l3_root)
        .map_err(|error| format!("create {}: {error}", l3_root.display()))?;
    let attempt_root = l3_root.join("attempt-6");
    std::fs::create_dir(&attempt_root).map_err(|error| {
        format!(
            "create fresh attempt directory {} (existing attempts are preserved): {error}",
            attempt_root.display()
        )
    })?;

    let mut result = build_and_measure(&evidence_root, &attempt_root);
    let source_after = verify_source_inputs_unchanged(&attempt_root);
    if let Err(source_error) = source_after {
        result = match result {
            Ok(()) => Err(source_error),
            Err(build_error) => Err(format!("{build_error}\n\n{source_error}")),
        };
    }
    if let Err(error) = &result {
        let _ = write_text(
            &attempt_root.join("build-failure.txt"),
            &format!("{error}\n"),
        );
    }
    if let Err(error) = write_recursive_attempt_manifest(&attempt_root, &l3_root) {
        return Err(match result {
            Ok(()) => error,
            Err(build_error) => format!("{build_error}\n\n{error}"),
        });
    }
    result
}

fn build_and_measure(evidence_root: &Path, attempt_root: &Path) -> Result<(), String> {
    let build_root = attempt_root.join("build");
    let commands_root = attempt_root.join("commands");
    let success_root = attempt_root.join("success");
    let capacity_root = attempt_root.join("capacity");
    let cli_default_root = attempt_root.join("cli-default");
    let cli_explicit_root = attempt_root.join("cli-explicit-mir");
    for directory in [
        &build_root,
        &commands_root,
        &success_root,
        &capacity_root,
        &cli_default_root,
        &cli_explicit_root,
    ] {
        std::fs::create_dir(directory)
            .map_err(|error| format!("create {}: {error}", directory.display()))?;
    }
    let temp_root = build_root.join("tmp");
    std::fs::create_dir(&temp_root)
        .map_err(|error| format!("create {}: {error}", temp_root.display()))?;
    let runner_binary = std::env::current_exe()
        .map_err(|error| format!("resolve no-harness runner binary: {error}"))?;
    let runner_copy = build_root.join("ordinary-map-set-result-l3-runner");
    std::fs::copy(&runner_binary, &runner_copy).map_err(|error| {
        format!(
            "copy no-harness runner {} to {}: {error}",
            runner_binary.display(),
            runner_copy.display()
        )
    })?;
    write_text(
        &commands_root.join("cargo-test-runner-binary.txt"),
        &format!(
            "source={}\ncopy={}\n",
            runner_binary.display(),
            runner_copy.display()
        ),
    )?;

    let l3_root = evidence_root.join("native-l3-standalone");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cargo_tmp_root = l3_root.join("cargo-tmp");
    // Reuse the repository's already populated canonical Cargo cache. This
    // avoids creating a second multi-gigabyte target tree beside the evidence.
    let cargo_target_root = repo_root.join("target");
    let cargo_invocation = format!(
        "mkdir -p {} &&\nTMPDIR={} CARGO_TARGET_DIR={} LLVM_SYS_181_PREFIX=/tmp/llvm-wrapper {EVIDENCE_ENV}={} cargo test --test ordinary-map-set-result-l3 --features {RUNTIME_FEATURE}\n",
        shell_quote(&cargo_tmp_root.to_string_lossy()),
        shell_quote(&cargo_tmp_root.to_string_lossy()),
        shell_quote(&cargo_target_root.to_string_lossy()),
        shell_quote(&evidence_root.to_string_lossy()),
    );
    write_text(
        &commands_root.join("cargo-parent-invocation.argv.txt"),
        &cargo_invocation,
    )?;

    let fixture_path = attempt_root.join("shared-set-result-fixture.mimi");
    write_text(&fixture_path, SET_RESULT_FIXTURE)?;
    write_text(
        &attempt_root.join("implementation-note.md"),
        &format!(
            "# Native SetResult L3 artifact flow\n\n\
             This runner claims L3 only after the direct owner-counter success and capacity executables plus the default and explicit --mir production CLI binaries all pass strict Valgrind, and the two CLI receipts match the direct MIR route identity.\n\n\
             The exact shared fixture is copied to `{}`. The no-harness Cargo driver runs outside Memcheck and performs Checker, exact SetLineage MIR materialization, native LLVM emission, and object writing. It builds the feature-gated standalone runtime archive, links separate success and capacity C-main executables, and then launches only those final executables under strict Valgrind. The capacity C main installs the thread-local root-publication failure before calling the generated main, on the same process main thread.\n\n\
             Exact Cargo command recorded for the parent lane:\n\n\
             ```sh\n{}\n\
             ```\n\n\
             The command sets `{}` to the central evidence directory, reuses the repository's existing `target/` Cargo cache, and places Cargo temporary files under the evidence root. The no-harness test binary is copied into `attempt-6/build/` for identity. All emitted objects, archives, shims, CLI-built binaries, logs, route identity records, command records, provenance, and hashes are written below `native-l3-standalone/attempt-6/`; the runner fails if that attempt directory already exists. The recursive `attempt-6-files.sha256` manifest is next to the attempt directory and hashes every file under attempt-6 after success or failure evidence has been written. Reachable bytes are reported separately in each `memcheck-summary.txt` and are not treated as lost bytes.\n",
            fixture_path.display(),
            cargo_invocation.trim_end(),
            EVIDENCE_ENV,
        ),
    )?;

    let runtime_source = repo_root.join("src/runtime/standalone.rs");
    let object_path = build_root.join("set-result-native.o");
    let ir_path = build_root.join("set-result-native.ll");
    let runtime_archive = build_root.join("libmimi_runtime_set_result_l3.a");
    let shim_source = build_root.join("set-result-l3-driver.c");
    write_text(&shim_source, DRIVER_SOURCE)?;
    write_text(
        &commands_root.join("native-llvm-emission.argv.txt"),
        &format!(
            "in-process native route: Lexer -> Parser -> Checker -> materialize_ordinary_map_set_result_route -> CodeGenerator::compile_checked_mir_native_with_ordinary_map_set_result_route -> CodeGenerator::compile_to_object\nfixture={}\nMIMI_OPT=0\nllvm-ir={}\nobject={}\n",
            fixture_path.display(),
            ir_path.display(),
            object_path.display(),
        ),
    )?;
    let provenance_root = attempt_root.join("source-binding");
    std::fs::create_dir(&provenance_root)
        .map_err(|error| format!("create {}: {error}", provenance_root.display()))?;
    write_tool_identities(&repo_root, &temp_root, &commands_root, attempt_root)?;
    write_source_binding(&repo_root, &provenance_root, &temp_root, &commands_root)?;
    write_target_abi_identity(&attempt_root, &commands_root)?;

    let direct_route_identity =
        compile_checked_native_object(&fixture_path, &object_path, &ir_path)?;
    compile_runtime_archive(
        &runtime_source,
        &runtime_archive,
        &repo_root,
        &temp_root,
        &commands_root,
    )?;

    let mut binaries = Vec::new();
    for (mode, capacity, mode_root) in [
        ("success", false, &success_root),
        ("capacity", true, &capacity_root),
    ] {
        let shim_object = mode_root.join("driver.o");
        let binary_path = mode_root.join("native-set-result-l3");
        compile_shim(
            mode,
            capacity,
            &shim_source,
            &shim_object,
            &repo_root,
            &temp_root,
            &commands_root,
        )?;
        link_native_executable(
            mode,
            &object_path,
            &shim_object,
            &runtime_archive,
            &binary_path,
            &repo_root,
            &temp_root,
            &commands_root,
        )?;
        binaries.push((mode, binary_path, mode_root));
    }

    let mut measurement_errors = Vec::new();
    for (mode, binary_path, mode_root) in binaries {
        if let Err(error) = run_strict_memcheck(
            mode,
            &binary_path,
            mode_root,
            &repo_root,
            &temp_root,
            &commands_root,
        ) {
            measurement_errors.push(error);
        }
    }
    if !measurement_errors.is_empty() {
        return Err(measurement_errors.join("\n\n"));
    }

    // The feature-gated owner counters above establish the success and
    // capacity state transitions, but their direct codegen entry alone does
    // not prove which public production route selected the graph. Build the
    // same fixture with both CLI entries, bind each emitted route receipt to
    // the direct artifact's receipt, and measure both CLI-built binaries.
    let fixture_path = attempt_root.join("shared-set-result-fixture.mimi");
    let mimi = mimi_binary_path()?;
    let mut cli_identities = Vec::new();
    for (explicit_mir, mode_root) in [(false, &cli_default_root), (true, &cli_explicit_root)] {
        let mode = if explicit_mir {
            "explicit-mir"
        } else {
            "default"
        };
        let binary = mode_root.join("set-result-cli-native");
        let mut args = vec![OsString::from("build"), fixture_path.as_os_str().to_owned()];
        if explicit_mir {
            args.push(OsString::from("--mir"));
        }
        args.extend([OsString::from("--output"), binary.as_os_str().to_owned()]);
        let output = Command::new(&mimi)
            .args(&args)
            .env("MIMI_ROUTE_CENSUS", "1")
            .env("MIMI_OPT", "0")
            .current_dir(&repo_root)
            .output()
            .map_err(|error| format!("run {mode} CLI build: {error}"))?;
        retain_process_output(mode_root, &mimi, &args, &output)?;
        if !output.status.success() {
            return Err(format!(
                "{mode} production CLI build failed ({:?})\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            ));
        }
        let identity = parse_cli_route_identity(&String::from_utf8_lossy(&output.stderr), mode)?;
        if identity.profile != ROUTE_PROFILE
            || identity.mir_digest != direct_route_identity.mir_digest
            || identity.receipt_identity_digest != direct_route_identity.receipt_identity_digest
        {
            return Err(format!(
                "{mode} CLI route receipt does not match the direct owner-counter artifact: CLI={identity:?}; direct profile={} mir_digest={} receipt_identity_digest={}",
                direct_route_identity.profile,
                direct_route_identity.mir_digest,
                direct_route_identity.receipt_identity_digest,
            ));
        }
        write_text(
            &mode_root.join("route-identity.txt"),
            &format!(
                "profile={}\nmir_digest={}\nreceipt_identity_digest={}\n",
                identity.profile, identity.mir_digest, identity.receipt_identity_digest
            ),
        )?;
        run_strict_memcheck_cli_binary(
            mode,
            &binary,
            mode_root,
            &repo_root,
            &temp_root,
            &commands_root,
        )?;
        cli_identities.push(identity);
    }
    if cli_identities.len() != 2 || cli_identities[0] != cli_identities[1] {
        return Err(format!(
            "default and explicit --mir CLI builds do not share one SetResult route identity: {cli_identities:?}"
        ));
    }
    write_text(
        &attempt_root.join("cli-route-binding.txt"),
        &format!(
            "default and explicit --mir CLI-built binaries were each run under strict Valgrind; both route receipts match the direct owner-counter success/capacity artifact.\nprofile={}\nmir_digest={}\nreceipt_identity_digest={}\n",
            direct_route_identity.profile,
            direct_route_identity.mir_digest,
            direct_route_identity.receipt_identity_digest,
        ),
    )?;
    println!(
        "native-map-set-result-l3 artifacts prepared under {} (Memcheck evidence recorded)",
        attempt_root.display()
    );
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct RouteIdentity {
    profile: String,
    mir_digest: String,
    receipt_identity_digest: String,
}

fn compile_checked_native_object(
    fixture_path: &Path,
    object_path: &Path,
    ir_path: &Path,
) -> Result<RouteIdentity, String> {
    let tokens = Lexer::new(SET_RESULT_FIXTURE)
        .tokenize()
        .map_err(|error| format!("lex exact shared SetResult fixture: {error}"))?;
    let (mut file, parse_errors) = mimi::loader::parser_for_path(tokens, fixture_path)
        .map_err(|error| format!("create shared SetResult fixture parser: {error}"))?
        .parse_file_with_recovery();
    if !parse_errors.is_empty() {
        return Err(format!(
            "parse exact shared SetResult fixture: {parse_errors:?}"
        ));
    }
    // Match the public CLI build frontend precisely: default `--std` prelude
    // merge and `main` ordering must be identical before checking and
    // materializing, since they participate in the receipt's semantic IDs.
    mimi::loader::merge_prelude_into(&mut file);
    if let Some(main_index) = file
        .items
        .iter()
        .position(|item| matches!(item, mimi::ast::Item::Func(function) if function.name == "main"))
    {
        let main_item = file.items.remove(main_index);
        file.items.push(main_item);
    }
    let checked = mimi::core::check_program(&file)
        .map_err(|errors| format!("Checker rejected exact SetResult fixture: {errors:?}"))?;
    let program = materialize_ordinary_map_set_result_route(&checked)
        .map_err(|error| format!("materialize exact SetLineage MIR route: {error}"))?;
    let receipt = program.route_receipt(ORDINARY_MAP_SET_RESULT_EXECUTION_PROFILE);
    let route_identity = RouteIdentity {
        profile: receipt.profile.clone(),
        mir_digest: receipt.mir_digest.clone(),
        receipt_identity_digest: receipt.route_identity_digest(),
    };

    let prior_opt = std::env::var_os("MIMI_OPT");
    let prior_dump_module = std::env::var_os("MIMI_DUMP_MODULE");
    std::env::set_var("MIMI_OPT", "0");
    std::env::remove_var("MIMI_DUMP_MODULE");
    let result = (|| {
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "native_map_set_result_l3_artifact");
        generator
            .compile_checked_mir_native_with_ordinary_map_set_result_route(
                &checked, &program, &receipt,
            )
            .map_err(|errors| format!("emit exact native SetResult route: {errors:?}"))?;
        generator
            .module
            .verify()
            .map_err(|error| format!("verify native SetResult module: {error}"))?;

        let main = generator
            .module
            .get_function("main")
            .ok_or_else(|| "native SetResult module lacks main".to_owned())?;
        if main.count_params() != 0
            || main.get_type().get_return_type() != Some(context.i32_type().into())
        {
            return Err("native SetResult entry does not have exact () -> i32 ABI".into());
        }
        rename_function(&main, GENERATED_ENTRY);
        generator
            .module
            .verify()
            .map_err(|error| format!("verify renamed native SetResult module: {error}"))?;
        if generator.module.get_function(GENERATED_ENTRY).is_none() {
            return Err("renamed native SetResult entry is absent".into());
        }
        generator
            .module
            .print_to_file(ir_path)
            .map_err(|error| format!("write native SetResult LLVM IR: {error}"))?;
        generator
            .compile_to_object(object_path)
            .map_err(|error| format!("emit native SetResult object: {error}"))
    })();
    match prior_opt {
        Some(value) => std::env::set_var("MIMI_OPT", value),
        None => std::env::remove_var("MIMI_OPT"),
    }
    match prior_dump_module {
        Some(value) => std::env::set_var("MIMI_DUMP_MODULE", value),
        None => std::env::remove_var("MIMI_DUMP_MODULE"),
    }
    result.map(|()| route_identity)
}

fn mimi_binary_path() -> Result<PathBuf, String> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_mimi") {
        return Ok(PathBuf::from(path));
    }
    let path = std::env::current_exe()
        .ok()
        .and_then(|test| {
            test.parent()
                .and_then(Path::parent)
                .map(|dir| dir.join("mimi"))
        })
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            "Cargo-built `mimi` CLI binary is unavailable to the L3 runner".to_owned()
        })?;
    Ok(path)
}

fn parse_cli_route_identity(stderr: &str, mode: &str) -> Result<RouteIdentity, String> {
    let lines = stderr
        .lines()
        .filter(|line| {
            line.contains(ROUTE_PROFILE)
                && line.contains("mir_digest=")
                && line.contains("receipt_identity_digest=")
        })
        .collect::<Vec<_>>();
    if lines.len() != 1 {
        return Err(format!(
            "{mode} CLI build must emit one exact SetResult route receipt; found {} matching records in stderr:\n{stderr}",
            lines.len()
        ));
    }
    let field = |key: &str| {
        lines[0]
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("{mode} route receipt lacks {key}: {}", lines[0]))
    };
    if stderr.contains("canonical route disposition: legacy") {
        return Err(format!(
            "{mode} CLI build selected a legacy route:\n{stderr}"
        ));
    }
    Ok(RouteIdentity {
        profile: ROUTE_PROFILE.to_owned(),
        mir_digest: field("mir_digest=")?,
        receipt_identity_digest: field("receipt_identity_digest=")?,
    })
}

fn retain_process_output(
    directory: &Path,
    program: &Path,
    args: &[OsString],
    output: &Output,
) -> Result<(), String> {
    let argv = std::iter::once(program.as_os_str().to_owned())
        .chain(args.iter().cloned())
        .map(|argument| shell_quote(&argument.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    write_text(&directory.join("command.argv.txt"), &format!("{argv}\n"))?;
    write_bytes(&directory.join("stdout.log"), &output.stdout)?;
    write_bytes(&directory.join("stderr.log"), &output.stderr)?;
    write_text(
        &directory.join("exit-status.txt"),
        &format_exit_status(output.status),
    )
}

fn run_strict_memcheck_cli_binary(
    mode: &str,
    binary_path: &Path,
    mode_root: &Path,
    repo_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
) -> Result<(), String> {
    let memcheck_log = mode_root.join("memcheck.log");
    let args = vec![
        OsString::from("--tool=memcheck"),
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=91"),
        OsString::from("--track-origins=yes"),
        OsString::from(format!("--log-file={}", memcheck_log.display())),
        binary_path.as_os_str().to_owned(),
    ];
    let output = run_logged(
        &format!("valgrind-cli-{mode}"),
        "valgrind",
        &args,
        repo_root,
        temp_root,
        commands_root,
    )?;
    write_bytes(&mode_root.join("program-stdout.log"), &output.stdout)?;
    write_bytes(&mode_root.join("program-stderr.log"), &output.stderr)?;
    write_text(
        &mode_root.join("program-exit-status.txt"),
        &format_exit_status(output.status),
    )?;
    let memcheck = std::fs::read_to_string(&memcheck_log)
        .map_err(|error| format!("read {}: {error}", memcheck_log.display()))?;
    write_text(
        &mode_root.join("memcheck-summary.txt"),
        &memcheck_summary(&memcheck),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() != Some(1) {
        return Err(format!(
            "strict Valgrind {mode} CLI binary must preserve fixture exit status 1, got {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}\nMemcheck:\n{memcheck}",
            output.status
        ));
    }
    if !stdout.is_empty() || !stderr.is_empty() {
        return Err(format!(
            "{mode} CLI binary emitted unexpected output under Valgrind\nstdout:\n{stdout}\nstderr:\n{stderr}"
        ));
    }
    if !strict_memcheck_is_clean(&memcheck) {
        return Err(format!(
            "strict Memcheck rejected {mode} CLI binary\n{memcheck}"
        ));
    }
    Ok(())
}

fn rename_function(function: &inkwell::values::FunctionValue<'_>, name: &str) {
    use inkwell::llvm_sys::core::LLVMSetValueName2;
    use inkwell::values::AsValueRef;

    // SAFETY: this is a live function in the current module, and LLVM copies
    // the exact byte span before returning.
    unsafe {
        LLVMSetValueName2(
            function.as_value_ref(),
            name.as_ptr() as *const std::os::raw::c_char,
            name.len(),
        );
    }
}

fn compile_runtime_archive(
    runtime_source: &Path,
    archive_path: &Path,
    repo_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
) -> Result<(), String> {
    let args = vec![
        "--edition".into(),
        "2021".into(),
        "--crate-type".into(),
        "staticlib".into(),
        "--cfg".into(),
        "standalone".into(),
        "--cfg".into(),
        format!("feature=\"{RUNTIME_FEATURE}\"").into(),
        "--crate-name".into(),
        "mimi_runtime".into(),
        "-A".into(),
        "dead_code".into(),
        "-o".into(),
        archive_path.as_os_str().to_owned(),
        runtime_source.as_os_str().to_owned(),
    ];
    let output = run_logged(
        "rustc-runtime-archive",
        "rustc",
        &args,
        repo_root,
        temp_root,
        commands_root,
    )?;
    require_success("build feature-gated standalone runtime archive", output)
}

fn compile_shim(
    mode: &str,
    capacity: bool,
    shim_source: &Path,
    shim_object: &Path,
    repo_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
) -> Result<(), String> {
    let args = vec![
        "-std=c11".into(),
        "-Wall".into(),
        "-Wextra".into(),
        "-Werror".into(),
        "-c".into(),
        format!("-DMIMI_L3_CAPACITY={}", if capacity { 1 } else { 0 }).into(),
        shim_source.as_os_str().to_owned(),
        "-o".into(),
        shim_object.as_os_str().to_owned(),
    ];
    let output = run_logged(
        &format!("cc-shim-{mode}"),
        "cc",
        &args,
        repo_root,
        temp_root,
        commands_root,
    )?;
    require_success(&format!("compile {mode} C main shim"), output)
}

fn link_native_executable(
    mode: &str,
    generated_object: &Path,
    shim_object: &Path,
    runtime_archive: &Path,
    binary_path: &Path,
    repo_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
) -> Result<(), String> {
    let args = vec![
        "-no-pie".into(),
        generated_object.as_os_str().to_owned(),
        shim_object.as_os_str().to_owned(),
        runtime_archive.as_os_str().to_owned(),
        "-o".into(),
        binary_path.as_os_str().to_owned(),
        "-lpthread".into(),
        "-ldl".into(),
        "-lm".into(),
    ];
    let output = run_logged(
        &format!("cc-link-{mode}"),
        "cc",
        &args,
        repo_root,
        temp_root,
        commands_root,
    )?;
    require_success(&format!("link {mode} native SetResult executable"), output)
}

fn run_strict_memcheck(
    mode: &str,
    binary_path: &Path,
    mode_root: &Path,
    repo_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
) -> Result<(), String> {
    let memcheck_log = mode_root.join("memcheck.log");
    let args = vec![
        "--tool=memcheck".into(),
        "--leak-check=full".into(),
        "--show-leak-kinds=all".into(),
        "--errors-for-leak-kinds=definite,indirect,possible".into(),
        "--error-exitcode=91".into(),
        "--track-origins=yes".into(),
        format!("--log-file={}", memcheck_log.display()).into(),
        binary_path.as_os_str().to_owned(),
    ];
    let output = run_logged(
        &format!("valgrind-{mode}"),
        "valgrind",
        &args,
        repo_root,
        temp_root,
        commands_root,
    )?;
    write_bytes(&mode_root.join("stdout.log"), &output.stdout)?;
    write_bytes(&mode_root.join("stderr.log"), &output.stderr)?;
    write_text(
        &mode_root.join("exit-status.txt"),
        &format_exit_status(output.status),
    )?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let memcheck = std::fs::read_to_string(&memcheck_log)
        .map_err(|error| format!("read {}: {error}", memcheck_log.display()))?;
    write_text(
        &mode_root.join("memcheck-summary.txt"),
        &memcheck_summary(&memcheck),
    )?;
    if !output.status.success() {
        return Err(format!(
            "strict Valgrind {mode} executable failed ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}\nMemcheck:\n{memcheck}",
            output.status
        ));
    }
    let expected_stdout = expected_stdout(mode);
    if stdout.as_ref() != expected_stdout.as_str() {
        return Err(format!(
            "native {mode} stdout did not exactly match the generated result/owner observation\nexpected: {expected_stdout:?}\nactual: {stdout:?}\nstderr:\n{stderr}"
        ));
    }
    if !strict_memcheck_is_clean(&memcheck) {
        return Err(format!("strict Memcheck rejected {mode}\n{memcheck}"));
    }
    Ok(())
}

fn expected_stdout(mode: &str) -> String {
    match mode {
        "success" => "native-map-set-result mode=success set_calls=1 generated_status=1 set_status=0 source_before=0/0 source_after=0/0 output_zero=0 roots_after_set=2 managed_any_after_set=0 any_owners_after_set=1 publish_attempts=0 shutdown_registered=1 final_roots=0 final_map_handles=0 final_owned_get_handles=0 final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n".to_owned(),
        "capacity" => "native-map-set-result mode=capacity set_calls=1 generated_status=2 set_status=2 source_before=0/0 source_after=0/0 output_zero=1 roots_after_set=1 managed_any_after_set=0 any_owners_after_set=0 publish_attempts=2 shutdown_registered=1 final_roots=0 final_map_handles=0 final_owned_get_handles=0 final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 final_any_owner_tokens=0 final_lists=0\n".to_owned(),
        _ => unreachable!("only the two closed test modes are emitted"),
    }
}

fn run_logged(
    name: &str,
    program: &str,
    args: &[OsString],
    repo_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
) -> Result<Output, String> {
    let argv_path = commands_root.join(format!("{name}.argv.txt"));
    let rendered = std::iter::once(program.to_owned())
        .chain(
            args.iter()
                .map(|argument| argument.to_string_lossy().into_owned()),
        )
        .map(|argument| shell_quote(&argument))
        .collect::<Vec<_>>()
        .join(" ");
    write_text(&argv_path, &format!("{rendered}\n"))?;

    let output = Command::new(program)
        .args(args)
        .current_dir(repo_root)
        .env("TMPDIR", temp_root)
        .output()
        .map_err(|error| format!("run {name} command `{rendered}`: {error}"))?;
    write_bytes(
        &commands_root.join(format!("{name}.stdout.log")),
        &output.stdout,
    )?;
    write_bytes(
        &commands_root.join(format!("{name}.stderr.log")),
        &output.stderr,
    )?;
    write_text(
        &commands_root.join(format!("{name}.exit-status.txt")),
        &format_exit_status(output.status),
    )?;
    Ok(output)
}

fn require_success(name: &str, output: Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{name} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ))
    }
}

fn write_source_binding(
    repo_root: &Path,
    provenance_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
) -> Result<(), String> {
    let head = run_logged(
        "git-head",
        "git",
        &["rev-parse".into(), "HEAD".into()],
        repo_root,
        temp_root,
        commands_root,
    )?;
    if !head.status.success() {
        return Err(format!(
            "read repository HEAD: {}",
            String::from_utf8_lossy(&head.stderr)
        ));
    }
    let head_path = provenance_root.join("HEAD.txt");
    write_bytes(&head_path, &head.stdout)?;

    let status = run_logged(
        "git-worktree-status",
        "git",
        &[
            "status".into(),
            "--porcelain=v1".into(),
            "--untracked-files=all".into(),
        ],
        repo_root,
        temp_root,
        commands_root,
    )?;
    if !status.status.success() {
        return Err(format!(
            "capture worktree status: {}",
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    let status_path = provenance_root.join("worktree-status.porcelain");
    write_bytes(&status_path, &status.stdout)?;
    let status_text = String::from_utf8_lossy(&status.stdout).into_owned();

    let tracked_diff = run_logged(
        "git-tracked-diff-from-head",
        "git",
        &["diff".into(), "--binary".into(), "HEAD".into()],
        repo_root,
        temp_root,
        commands_root,
    )?;
    if !tracked_diff.status.success() {
        return Err(format!(
            "capture tracked diff from HEAD: {}",
            String::from_utf8_lossy(&tracked_diff.stderr)
        ));
    }
    let diff_path = provenance_root.join("tracked-diff-from-HEAD.patch");
    write_bytes(&diff_path, &tracked_diff.stdout)?;
    let diff_hash = run_logged(
        "sha256sum-tracked-diff-from-HEAD",
        "sha256sum",
        &[diff_path.as_os_str().to_owned()],
        repo_root,
        temp_root,
        commands_root,
    )?;
    if !diff_hash.status.success() {
        return Err(format!(
            "hash tracked diff from HEAD: {}",
            String::from_utf8_lossy(&diff_hash.stderr)
        ));
    }
    write_bytes(
        &provenance_root.join("tracked-diff-sha256.txt"),
        &diff_hash.stdout,
    )?;

    let cargo_version = std::fs::read(commands_root.join("identity-cargo.stdout.log"))
        .map_err(|error| format!("read recorded Cargo version: {error}"))?;
    let cargo_version_path = provenance_root.join("cargo-version.txt");
    write_bytes(&cargo_version_path, &cargo_version)?;

    let mut source_inputs = collect_rust_source_inputs(&repo_root.join("src"))?;
    source_inputs.extend([
        repo_root.join("Cargo.toml"),
        repo_root.join("tests/ordinary_map_set_result_l3.rs"),
        repo_root.join("tests/fixtures/mir_ordinary_map_owned_any_v2_set_result_lineage.mimi"),
    ]);
    for relative in [
        "Cargo.lock",
        "build.rs",
        ".cargo/config.toml",
        "rust-toolchain.toml",
        "rust-toolchain",
    ] {
        let candidate = repo_root.join(relative);
        if candidate.is_file() {
            source_inputs.push(candidate);
        }
    }
    source_inputs.sort();
    source_inputs.dedup();
    let hash_args = source_inputs
        .iter()
        .map(|path| path.as_os_str().to_owned())
        .collect::<Vec<_>>();
    let source_hashes = run_logged(
        "sha256sum-source-inputs",
        "sha256sum",
        &hash_args,
        repo_root,
        temp_root,
        commands_root,
    )?;
    if !source_hashes.status.success() {
        return Err(format!(
            "hash relevant runtime and runner source inputs: {}",
            String::from_utf8_lossy(&source_hashes.stderr)
        ));
    }
    let input_hashes_path = provenance_root.join("source-input-sha256.txt");
    write_bytes(&input_hashes_path, &source_hashes.stdout)?;

    let untracked_paths = source_inputs
        .iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(repo_root).ok()?.to_string_lossy();
            status_text
                .lines()
                .any(|line| line.starts_with("?? ") && &line[3..] == relative.as_ref())
                .then(|| relative.into_owned())
        })
        .collect::<Vec<_>>();
    let untracked_hashes = String::from_utf8_lossy(&source_hashes.stdout)
        .lines()
        .filter(|line| {
            untracked_paths
                .iter()
                .any(|path| line.ends_with(path.as_str()))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let untracked_input_path =
        provenance_root.join("untracked-runner-fixture-runtime-inputs.sha256.txt");
    write_text(
        &untracked_input_path,
        &format!(
            "Untracked relevant inputs from worktree-status.porcelain:\n{}\n\nSHA-256:\n{}\n",
            if untracked_paths.is_empty() {
                "(none)".to_owned()
            } else {
                untracked_paths.join("\n")
            },
            if untracked_hashes.is_empty() {
                "(none)".to_owned()
            } else {
                untracked_hashes
            }
        ),
    )?;

    let source_binding_note = provenance_root.join("source-binding.md");
    write_text(
        &source_binding_note,
        "# Source binding for native SetResult L3 attempt-6\n\n\
         `HEAD.txt` binds the base revision. `worktree-status.porcelain` records the complete working-tree state including untracked files. `tracked-diff-from-HEAD.patch` preserves the complete tracked diff used by this run, and `tracked-diff-sha256.txt` records its digest. `source-input-sha256.txt` hashes Cargo configuration, the no-harness runner, the exact fixture, and every Rust source file under `src/`, covering Checker, MIR, native codegen, and runtime inputs. `source-input-sha256-after.txt` and `source-input-hash-comparison.txt` prove each relevant file stayed byte-identical after build and measurement. `untracked-runner-fixture-runtime-inputs.sha256.txt` names and hashes relevant source inputs explicitly. Tool versions and target/ABI details are recorded at `../tool-identities.md` and `../target-abi.md`; `../../attempt-6-files.sha256` hashes the tracked-diff copy, source binding, command records, logs, objects, archive, shims, binaries, and other attempt files.\n\n\
         The C observation record is 136 bytes and guarded by a C11 `_Static_assert`; generated native entry ABI is `mimi_l3_generated_main() -> i32`. The tested runtime ABI is `mimi_ordinary_map_owned_any_v2_set_i32_result(i64, *const u8, i64, i32, *mut i64) -> i32`, with success `(0, nonzero output)` and Set capacity `(2, zero output)`. The runtime archive is built with `standalone` plus the feature-only `ordinary-map-set-result-l3` cfg.\n",
    )?;
    Ok(())
}

fn verify_source_inputs_unchanged(attempt_root: &Path) -> Result<(), String> {
    let provenance_root = attempt_root.join("source-binding");
    std::fs::create_dir_all(&provenance_root).map_err(|error| {
        format!(
            "create source-binding directory {}: {error}",
            provenance_root.display()
        )
    })?;
    let before_path = provenance_root.join("source-input-sha256.txt");
    let comparison_path = provenance_root.join("source-input-hash-comparison.txt");
    if !before_path.is_file() {
        write_text(
            &comparison_path,
            "Source input post-check unavailable: the pre-build source-input SHA-256 manifest was not created because the runner stopped before source binding completed.\n",
        )?;
        return Err(
            "source input hashes could not be compared because no pre-build manifest exists"
                .to_owned(),
        );
    }

    let before_text = read_text(&before_path)?;
    let before = parse_sha256_manifest(&before_text)?;
    let source_paths = before
        .iter()
        .map(|(_, path)| OsString::from(path.as_str()))
        .collect::<Vec<_>>();
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let commands_root = attempt_root.join("commands");
    let temp_root = attempt_root.join("build/tmp");
    std::fs::create_dir_all(&commands_root)
        .map_err(|error| format!("create command record directory: {error}"))?;
    std::fs::create_dir_all(&temp_root)
        .map_err(|error| format!("create post-check temporary directory: {error}"))?;
    let after = run_logged(
        "sha256sum-source-inputs-after",
        "sha256sum",
        &source_paths,
        &repo_root,
        &temp_root,
        &commands_root,
    )?;
    if !after.status.success() {
        write_bytes(
            &provenance_root.join("source-input-sha256-after.txt"),
            &after.stdout,
        )?;
        write_text(
            &comparison_path,
            &format!(
                "Source input post-check failed: sha256sum exited {:?}.\n{}",
                after.status,
                String::from_utf8_lossy(&after.stderr)
            ),
        )?;
        return Err("source input post-build SHA-256 recomputation failed".to_owned());
    }
    let after_text = String::from_utf8_lossy(&after.stdout).into_owned();
    write_text(
        &provenance_root.join("source-input-sha256-after.txt"),
        &after_text,
    )?;
    let after = parse_sha256_manifest(&after_text)?;
    let after_by_path = after
        .iter()
        .map(|(digest, path)| (path.as_str(), digest.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut all_match = before.len() == after.len();
    let mut comparison = String::from(
        "Per-input SHA-256 comparison after the build and measurement phase:\n\npath | before | after | result\n--- | --- | --- | ---\n",
    );
    for (before_digest, path) in &before {
        let after_digest = after_by_path
            .get(path.as_str())
            .copied()
            .unwrap_or("<missing>");
        let matches = before_digest.as_str() == after_digest;
        all_match &= matches;
        comparison.push_str(&format!(
            "{} | {} | {} | {}\n",
            path,
            before_digest,
            after_digest,
            if matches { "MATCH" } else { "MISMATCH" },
        ));
    }
    if before.len() != after.len() {
        comparison.push_str(&format!(
            "\nInput count mismatch: before={} after={}\n",
            before.len(),
            after.len()
        ));
    }
    comparison.push_str(if all_match {
        "\nResult: all recorded source inputs are byte-identical before and after the attempt.\n"
    } else {
        "\nResult: at least one source input changed or is missing.\n"
    });
    write_text(&comparison_path, &comparison)?;
    if all_match {
        Ok(())
    } else {
        Err("one or more source input hashes changed during the attempt".to_owned())
    }
}

fn parse_sha256_manifest(manifest: &str) -> Result<Vec<(String, String)>, String> {
    manifest
        .lines()
        .map(|line| {
            let (digest, path) = line
                .split_once("  ")
                .ok_or_else(|| format!("invalid sha256sum manifest line: {line:?}"))?;
            Ok((digest.to_owned(), path.to_owned()))
        })
        .collect()
}

fn collect_rust_source_inputs(source_root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut inputs = Vec::new();
    collect_rust_sources_recursively(source_root, &mut inputs)?;
    inputs.sort();
    Ok(inputs)
}

fn collect_rust_sources_recursively(
    directory: &Path,
    inputs: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("read runtime sources in {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("read runtime source directory entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources_recursively(&path, inputs)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            inputs.push(path);
        }
    }
    Ok(())
}

fn write_tool_identities(
    repo_root: &Path,
    temp_root: &Path,
    commands_root: &Path,
    attempt_root: &Path,
) -> Result<(), String> {
    let mut identity = String::from(
        "# Tool identities for attempt-6\n\nChecker/MIR/native LLVM object creation occurred in the outer no-harness runner. Memcheck children executed only the linked native binaries.\n\n",
    );
    for (label, program, args) in [
        ("cargo", "cargo", vec!["--version", "--verbose"]),
        ("rustc", "rustc", vec!["--version", "--verbose"]),
        ("cc", "cc", vec!["--version"]),
        ("cc-target", "cc", vec!["-dumpmachine"]),
        ("linker", "ld", vec!["--version"]),
        ("archiver", "ar", vec!["--version"]),
        ("host", "uname", vec!["-a"]),
        ("valgrind", "valgrind", vec!["--version"]),
        ("llvm-config", "llvm-config", vec!["--version"]),
        ("llvm-host-target", "llvm-config", vec!["--host-target"]),
        ("llvm-targets-built", "llvm-config", vec!["--targets-built"]),
        ("sha256sum", "sha256sum", vec!["--version"]),
    ] {
        let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
        let output = run_logged(
            &format!("identity-{label}"),
            program,
            &args,
            repo_root,
            temp_root,
            commands_root,
        );
        match output {
            Ok(output) if output.status.success() => identity.push_str(&format!(
                "## {label}\n\n{}\n",
                String::from_utf8_lossy(&output.stdout)
            )),
            Ok(output) => identity.push_str(&format!(
                "## {label}\n\ncommand exited {:?}\nstdout:\n{}\nstderr:\n{}\n",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )),
            Err(error) => identity.push_str(&format!("## {label}\n\n{error}\n")),
        }
    }
    write_text(&attempt_root.join("tool-identities.md"), &identity)
}

fn write_target_abi_identity(attempt_root: &Path, commands_root: &Path) -> Result<(), String> {
    let rustc = read_text(&commands_root.join("identity-rustc.stdout.log"))
        .unwrap_or_else(|error| format!("<unavailable: {error}>"));
    let cc_target = read_text(&commands_root.join("identity-cc-target.stdout.log"))
        .unwrap_or_else(|error| format!("<unavailable: {error}>"));
    let llvm_target = read_text(&commands_root.join("identity-llvm-host-target.stdout.log"))
        .unwrap_or_else(|error| format!("<unavailable: {error}>"));
    let rust_host = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or("<missing from rustc -vV>");
    let target_abi = format!(
        "# Target and ABI identity for attempt-6\n\n\
         The no-harness runner was compiled for and is running on Rust target `{}` (`{}`, pointer width {} bits). Its Cargo invocation specifies no `--target`, so Cargo uses its host target `{rust_host}` unless repository Cargo configuration overrides that default; the hashed Cargo config inputs and exact command are preserved with this attempt.\n\n\
         C compiler target (`cc -dumpmachine`): `{}`. LLVM configured host target (`llvm-config --host-target`): `{}`. Native link uses `cc -no-pie` and links the generated LLVM object, the C main shim object, and the feature-gated Rust runtime static archive; the exact arguments are recorded under `commands/`.\n\n\
         ABI checks: the generated entry point is `int32_t mimi_l3_generated_main(void)`; the runtime Set ABI is `int32_t mimi_ordinary_map_owned_any_v2_set_i32_result(int64_t, const uint8_t *, int64_t, int32_t, int64_t *)`; the C observation struct is 136 bytes and guarded by `_Static_assert`, matching Rust `#[repr(C)]`.\n",
        std::env::consts::ARCH,
        std::env::consts::OS,
        usize::BITS,
        cc_target.trim(),
        llvm_target.trim(),
    );
    write_text(&attempt_root.join("target-abi.md"), &target_abi)
}

fn write_recursive_attempt_manifest(attempt_root: &Path, l3_root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_files_recursively(attempt_root, &mut files)?;
    let commands_root = attempt_root.join("commands");
    std::fs::create_dir_all(&commands_root)
        .map_err(|error| format!("create command record directory: {error}"))?;
    let command_record_path = commands_root.join("sha256sum-attempt-files.argv.txt");
    if !files.contains(&command_record_path) {
        files.push(command_record_path.clone());
    }
    files.sort();
    let manifest_path = l3_root.join("attempt-6-files.sha256");
    let hash_args = files
        .iter()
        .map(|path| path.as_os_str().to_owned())
        .collect::<Vec<_>>();
    let rendered = std::iter::once("sha256sum".to_owned())
        .chain(
            hash_args
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned()),
        )
        .map(|argument| shell_quote(&argument))
        .collect::<Vec<_>>()
        .join(" ");
    write_text(&command_record_path, &format!("{rendered}\n"))?;
    let output = Command::new("sha256sum")
        .args(&hash_args)
        .output()
        .map_err(|error| format!("hash every attempt-6 file: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "recursive sha256sum failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    write_bytes(&manifest_path, &output.stdout)
}

fn collect_files_recursively(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("read attempt files in {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read attempt directory entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect attempt entry {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_files_recursively(&path, files)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            files.push(path);
        }
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn strict_memcheck_is_clean(log: &str) -> bool {
    let zero_lost_kinds = ["definitely lost:", "indirectly lost:", "possibly lost:"]
        .iter()
        .all(|kind| {
            log.lines().any(|line| {
                line.contains(kind)
                    && line
                        .split_once(kind)
                        .is_some_and(|(_, tail)| tail.trim_start().starts_with("0 bytes"))
            })
        });
    let empty_heap_at_exit = log
        .lines()
        .any(|line| line.contains("in use at exit: 0 bytes in 0 blocks"));
    let all_blocks_freed = log
        .lines()
        .any(|line| line.contains("All heap blocks were freed -- no leaks are possible"));
    let zero_errors = log.lines().any(|line| {
        line.split_once("ERROR SUMMARY:")
            .is_some_and(|(_, tail)| tail.trim_start().starts_with("0 errors"))
    });
    (zero_lost_kinds || (empty_heap_at_exit && all_blocks_freed)) && zero_errors
}

fn memcheck_summary(log: &str) -> String {
    let labels = [
        "definitely lost:",
        "indirectly lost:",
        "possibly lost:",
        "still reachable:",
        "ERROR SUMMARY:",
    ];
    let mut summary = String::new();
    let no_heap_at_exit = log
        .lines()
        .any(|line| line.contains("in use at exit: 0 bytes in 0 blocks"))
        && log
            .lines()
            .any(|line| line.contains("All heap blocks were freed -- no leaks are possible"));
    for label in labels {
        match log.lines().find(|line| line.contains(label)) {
            Some(line) => summary.push_str(&format!("{}\n", line.trim())),
            None if no_heap_at_exit && label != "ERROR SUMMARY:" => {
                summary.push_str(&format!("{label} 0 bytes (empty heap at exit)\n"));
            }
            None => summary.push_str(&format!("{label} <missing>\n")),
        }
    }
    if no_heap_at_exit {
        summary.push_str("heap-at-exit: 0 bytes in 0 blocks; all heap blocks freed\n");
    }
    summary
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

fn format_exit_status(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit-code={code}\n"),
        None => "terminated-without-exit-code\n".to_owned(),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
