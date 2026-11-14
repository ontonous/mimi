//! Standalone strict Memcheck gate for the exact New/Set/Get-i32 Native route.
//!
//! This is a Cargo `harness = false` target. Checker, MIR materialization,
//! LLVM code generation, and native linking happen in this outer runner. The
//! Valgrind child executes only the persisted native ELF and its runtime ABI
//! probes; it never starts Cargo, the Mimi parser, or libtest.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

use inkwell::context::Context;
use mimi::codegen::CodeGenerator;
use mimi::core::mir::{
    materialize_ordinary_map_set_get_i32_route, ORDINARY_MAP_SET_GET_I32_EXECUTION_PROFILE,
};
use mimi::lexer::Lexer;

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const EVIDENCE_ENV: &str = "MIMI_ORDINARY_MAP_SET_GET_I32_L3_EVIDENCE";
const PROFILE: &str = ORDINARY_MAP_SET_GET_I32_EXECUTION_PROFILE;
const RUNTIME_FEATURE: &str = "ordinary-map-set-result-l3";
const GENERATED_ENTRY: &str = "mimi_l3_generated_main";
const FIXTURE: &str =
    include_str!("fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi");
const MISMATCH_TYPE_ID: &[u8; 35] = b"rt:00000000000000000000000000000001";
const MISMATCH_DESCRIPTOR_HASH: [u8; 32] = [0xa5; 32];
const REGISTRY_HASH_DOMAIN: &[u8] = b"mimi-managed-any-payload-v2:admitted-descriptor-set:v1\n";

const DRIVER_SOURCE: &str = r#"
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

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

static const uint8_t mismatch_type_id[35] = { @MISMATCH_TYPE_ID@ };
static const uint8_t mismatch_descriptor_hash[32] = { @MISMATCH_HASH@ };
static const uint8_t mismatch_selection[87] = { @MISMATCH_SELECTION@ };
static const uint8_t mismatch_selection_hash[32] = { @MISMATCH_SELECTION_HASH@ };

extern int32_t mimi_l3_generated_main(void);
extern int32_t mimi_ordinary_map_set_result_l3_read_observation(
    struct mimi_set_result_l3_observation *out_observation);
extern int32_t mimi_ordinary_map_owned_any_v2_new(int64_t *out_root);
extern int32_t mimi_ordinary_map_owned_any_v2_size(int64_t handle, int32_t *out_size);
extern int32_t mimi_ordinary_map_owned_any_v2_drop(int64_t handle);
extern int32_t mimi_ordinary_map_owned_any_v2_get_i32_result(
    int64_t handle, const uint8_t *key, int64_t key_len, int32_t *out_value);
extern int32_t mimi_any_payload_v2_new(
    int32_t tag, const uint8_t *type_id, int64_t type_id_len,
    const uint8_t *descriptor_hash, const uint8_t *payload,
    int64_t size, int64_t align,
    int32_t (*clone_fn)(uint8_t *destination, const uint8_t *source),
    void (*drop_fn)(uint8_t *payload), int64_t *out_token);
extern int32_t mimi_any_payload_v2_drop(int64_t token);
extern int32_t mimi_ordinary_map_owned_any_v2_set(
    int64_t handle, const uint8_t *key, int64_t key_len, int64_t payload_owner,
    const uint8_t *selection, int64_t selection_len,
    const uint8_t *selection_hash, int64_t *out_root);

static int32_t clone_i32(uint8_t *destination, const uint8_t *source) {
    memcpy(destination, source, sizeof(int32_t));
    return 0;
}

static void drop_i32(uint8_t *payload) {
    (void)payload;
}

static int observation_is(struct mimi_set_result_l3_observation *observation,
                          uint64_t roots, uint64_t runtime_maps,
                          uint64_t managed_any, uint64_t owned_strings,
                          uint64_t any_owner_tokens, uint64_t lists) {
    if (mimi_ordinary_map_set_result_l3_read_observation(observation) != 0) {
        return 0;
    }
    return observation->final_roots == roots &&
           observation->final_ordinary_map_handles == roots &&
           observation->final_owned_get_handles == 0 &&
           observation->final_runtime_map_handles == runtime_maps &&
           observation->final_managed_any == managed_any &&
           observation->final_owned_strings == owned_strings &&
           observation->final_any_owner_tokens == any_owner_tokens &&
           observation->final_lists == lists;
}

static int all_runtime_counts_are_zero(struct mimi_set_result_l3_observation *observation) {
    return observation_is(observation, 0, 0, 0, 0, 0, 0);
}

int main(void) {
    const int32_t generated_status = mimi_l3_generated_main();
    struct mimi_set_result_l3_observation observation = {0};
    if (generated_status != 42 || !all_runtime_counts_are_zero(&observation) ||
        observation.set_calls != 1 || observation.set_status != 0 ||
        observation.output_root_zero != 0 || observation.roots_after_set != 2 ||
        observation.managed_any_after_set != 0 ||
        observation.any_owners_after_set != 1 || observation.root_publish_attempts != 0 ||
        observation.shutdown_registered != 1 ||
        observation.source_before_status != 0 || observation.source_before_size != 0 ||
        observation.source_after_status != 0 || observation.source_after_size != 0) {
        fputs("Set/Get-i32 L3 generated hit or final-settlement invariant failed\n", stderr);
        return 91;
    }

    int64_t missing_root = 0;
    int32_t output = INT32_MIN;
    if (mimi_ordinary_map_owned_any_v2_new(&missing_root) != 0 || missing_root == 0 ||
        mimi_ordinary_map_owned_any_v2_get_i32_result(
            missing_root, (const uint8_t *)"answer", 6, &output) != 1 || output != 0 ||
        !observation_is(&observation, 1, 1, 0, 0, 0, 0)) {
        fputs("Set/Get-i32 L3 missing lookup invariant failed\n", stderr);
        return 92;
    }
    int32_t missing_size = -1;
    if (mimi_ordinary_map_owned_any_v2_size(missing_root, &missing_size) != 0 ||
        missing_size != 0 || mimi_ordinary_map_owned_any_v2_drop(missing_root) != 0 ||
        !all_runtime_counts_are_zero(&observation)) {
        fputs("Set/Get-i32 L3 missing-root settlement invariant failed\n", stderr);
        return 93;
    }

    int64_t source_root = 0;
    int64_t noncanonical_owner = 0;
    int64_t mismatch_root = 0;
    int32_t payload = 41;
    if (mimi_ordinary_map_owned_any_v2_new(&source_root) != 0 || source_root == 0 ||
        mimi_any_payload_v2_new(
            1, mismatch_type_id, 35, mismatch_descriptor_hash,
            (const uint8_t *)&payload, sizeof(payload), _Alignof(int32_t),
            clone_i32, drop_i32, &noncanonical_owner) != 0 || noncanonical_owner == 0 ||
        mimi_ordinary_map_owned_any_v2_set(
            source_root, (const uint8_t *)"wrong", 5, noncanonical_owner,
            mismatch_selection, sizeof(mismatch_selection), mismatch_selection_hash,
            &mismatch_root) != 0 || mismatch_root == 0 ||
        mimi_ordinary_map_owned_any_v2_drop(source_root) != 0 ||
        mimi_any_payload_v2_drop(noncanonical_owner) != 0) {
        fputs("Set/Get-i32 L3 type-mismatch fixture setup failed\n", stderr);
        return 94;
    }
    output = INT32_MIN;
    int32_t mismatch_status = mimi_ordinary_map_owned_any_v2_get_i32_result(
        mismatch_root, (const uint8_t *)"wrong", 5, &output);
    int32_t mismatch_size = -1;
    if (mismatch_status != 2 || output != 0 ||
        mimi_ordinary_map_owned_any_v2_size(mismatch_root, &mismatch_size) != 0 ||
        mismatch_size != 1 || !observation_is(&observation, 1, 1, 0, 0, 1, 0)) {
        fputs("Set/Get-i32 L3 mismatch lookup changed its source owner or output\n", stderr);
        return 95;
    }
    if (mimi_ordinary_map_owned_any_v2_drop(mismatch_root) != 0 ||
        !all_runtime_counts_are_zero(&observation)) {
        fputs("Set/Get-i32 L3 mismatch-root settlement invariant failed\n", stderr);
        return 96;
    }

    printf("native-map-set-get-i32 generated_status=%" PRId32
           " set_calls=%" PRIu64 " set_status=%" PRId32
           " set_roots_after=%" PRIu64 " set_anyowners_after=%" PRIu64
           " missing_status=1 missing_output=0 mismatch_status=%" PRId32
           " mismatch_output=0 final_roots=%" PRIu64
           " final_map_handles=%" PRIu64 " final_runtime_map_handles=%" PRIu64
           " final_managed_any=%" PRIu64 " final_owned_strings=%" PRIu64
           " final_any_owner_tokens=%" PRIu64 " final_lists=%" PRIu64 "\n",
           generated_status, observation.set_calls, observation.set_status,
           observation.roots_after_set, observation.any_owners_after_set,
           mismatch_status, observation.final_roots,
           observation.final_ordinary_map_handles, observation.final_runtime_map_handles,
           observation.final_managed_any, observation.final_owned_strings,
           observation.final_any_owner_tokens, observation.final_lists);
    return 0;
}
"#;

fn main() {
    if let Err(error) = run_parent() {
        eprintln!("ordinary-map-set-get-i32-l3: {error}");
        std::process::exit(1);
    }
}

fn run_parent() -> Result<(), String> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let evidence_root = std::env::var_os(EVIDENCE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{EVIDENCE_ENV} must name a fresh central evidence directory"))?;
    if !evidence_root.is_dir() {
        return Err(format!(
            "evidence directory does not exist: {}",
            evidence_root.display()
        ));
    }
    let attempt_root = evidence_root.join("attempt-1");
    fs::create_dir(&attempt_root).map_err(|error| {
        format!(
            "create fresh attempt directory {} (existing evidence is preserved): {error}",
            attempt_root.display()
        )
    })?;
    for name in [
        "build",
        "commands",
        "default-build",
        "explicit-build",
        "memcheck",
    ] {
        fs::create_dir(attempt_root.join(name))
            .map_err(|error| format!("create attempt directory {name}: {error}"))?;
    }
    let build_root = attempt_root.join("build");
    let command_root = attempt_root.join("commands");
    let before = source_snapshot(&repo_root)?;
    write_text(&attempt_root.join("source-hashes-before.sha256"), &before)?;

    let attempt_result = run_attempt(&repo_root, &attempt_root, &build_root, &command_root);
    let source_result = match source_snapshot(&repo_root) {
        Ok(after) => {
            write_text(&attempt_root.join("source-hashes-after.sha256"), &after)?;
            if before == after {
                write_text(
                    &attempt_root.join("source-hash-comparison.txt"),
                    "All source inputs in the manifest are byte-identical before and after the attempt.\n",
                )?;
                Ok(())
            } else {
                let message =
                    "source inputs changed while preparing or running the native artifact";
                write_text(&attempt_root.join("source-hash-comparison.txt"), message)?;
                Err(message.to_owned())
            }
        }
        Err(error) => {
            write_text(
                &attempt_root.join("source-hashes-after.error.txt"),
                &format!("{error}\n"),
            )?;
            write_text(
                &attempt_root.join("source-hash-comparison.txt"),
                "Could not recompute source inputs after the attempt.\n",
            )?;
            Err(error)
        }
    };
    let result = match (attempt_result, source_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(attempt_error), Err(source_error)) => Err(format!("{attempt_error}\n{source_error}")),
    };
    if let Err(error) = &result {
        write_text(&attempt_root.join("failure.txt"), &format!("{error}\n"))?;
    }
    write_attempt_manifest(&attempt_root, &evidence_root)?;
    result?;
    println!(
        "ordinary-map-set-get-i32-l3 completed; strict Memcheck evidence: {}",
        attempt_root.display()
    );
    Ok(())
}

fn run_attempt(
    repo_root: &Path,
    attempt_root: &Path,
    build_root: &Path,
    command_root: &Path,
) -> Result<(), String> {
    let fixture_path = attempt_root.join("shared-set-get-i32-fixture.mimi");
    let shared_fixture_path =
        repo_root.join("tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi");
    let current_fixture = fs::read_to_string(&shared_fixture_path).map_err(|error| {
        format!(
            "read exact shared Set/Get-i32 fixture {}: {error}",
            shared_fixture_path.display()
        )
    })?;
    if current_fixture != FIXTURE {
        return Err(
            "the compile-time fixture bytes differ from the shared source fixture".to_owned(),
        );
    }
    write_text(&fixture_path, FIXTURE)?;
    let tokens = Lexer::new(FIXTURE)
        .tokenize()
        .map_err(|error| format!("lex exact shared Set/Get-i32 fixture: {error}"))?;
    let parser = mimi::loader::parser_for_path(tokens, &shared_fixture_path)
        .map_err(|error| format!("register exact shared Set/Get-i32 source identity: {error}"))?;
    let file = parser
        .parse_file()
        .map_err(|error| format!("parse exact shared Set/Get-i32 fixture: {error}"))?;
    let mut file = file;
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
        .map_err(|errors| format!("Checker rejected exact fixture: {errors:?}"))?;
    let program = materialize_ordinary_map_set_get_i32_route(&checked)
        .map_err(|error| format!("materialize exact SetGet MIR route: {error}"))?;
    let receipt = program.route_receipt(PROFILE);
    receipt
        .validate_against_program(&program)
        .map_err(|error| format!("SetGet execution receipt rejected: {error}"))?;
    let mir_digest = program.canonical_digest();
    let receipt_identity_digest = receipt.route_identity_digest();
    write_text(
        &attempt_root.join("native-route-identity.txt"),
        &format!(
            "route_profile={PROFILE}\nmir_digest={mir_digest}\nroute_identity_digest={receipt_identity_digest}\nsource_fixture=tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi\n"
        ),
    )?;

    let object_path = build_root.join("set-get-i32-native.o");
    let ir_path = build_root.join("set-get-i32-native.ll");
    compile_checked_native_object(&checked, &program, &receipt, &object_path, &ir_path)?;
    verify_default_and_explicit_cli_routes(
        attempt_root,
        command_root,
        &shared_fixture_path,
        &mir_digest,
        &receipt_identity_digest,
    )?;

    let runtime_archive = build_root.join("libmimi_runtime_set_get_i32_l3.a");
    compile_runtime_archive(
        &repo_root.join("src/runtime/standalone.rs"),
        &runtime_archive,
        repo_root,
        command_root,
    )?;
    let driver_source_path = build_root.join("set-get-i32-l3-driver.c");
    write_text(&driver_source_path, &driver_source())?;
    let driver_object = build_root.join("set-get-i32-l3-driver.o");
    compile_c_driver(&driver_source_path, &driver_object, repo_root, command_root)?;
    let native_binary = build_root.join("native-set-get-i32-l3");
    link_native_executable(
        &object_path,
        &driver_object,
        &runtime_archive,
        &native_binary,
        repo_root,
        command_root,
    )?;
    run_strict_memcheck(
        &native_binary,
        &attempt_root.join("memcheck"),
        repo_root,
        command_root,
    )
}

fn compile_checked_native_object(
    checked: &mimi::core::resolved::CheckedProgram,
    program: &mimi::core::mir::reference::MirProgram,
    receipt: &mimi::core::mir::CanonicalMirRouteReceipt,
    object_path: &Path,
    ir_path: &Path,
) -> Result<(), String> {
    let previous_optimization = std::env::var_os("MIMI_OPT");
    let previous_dump_module = std::env::var_os("MIMI_DUMP_MODULE");
    std::env::set_var("MIMI_OPT", "0");
    std::env::remove_var("MIMI_DUMP_MODULE");
    let result = (|| {
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "native_map_set_get_i32_l3_artifact");
        generator
            .compile_checked_mir_native_with_ordinary_map_set_get_i32_route(
                checked, program, receipt,
            )
            .map_err(|errors| {
                format!("exact native Set/Get-i32 route emission failed: {errors:?}")
            })?;
        generator
            .module
            .verify()
            .map_err(|error| format!("verify exact native Set/Get-i32 module: {error}"))?;
        let main = generator
            .module
            .get_function("main")
            .ok_or_else(|| "exact native Set/Get-i32 module lacks main".to_owned())?;
        if main.count_params() != 0
            || main.get_type().get_return_type() != Some(context.i32_type().into())
        {
            return Err("exact native Set/Get-i32 main ABI is not () -> i32".into());
        }
        rename_function(&main, GENERATED_ENTRY);
        generator
            .module
            .verify()
            .map_err(|error| format!("verify renamed native Set/Get-i32 module: {error}"))?;
        generator
            .module
            .print_to_file(ir_path)
            .map_err(|error| format!("write native Set/Get-i32 LLVM IR: {error}"))?;
        generator
            .compile_to_object(object_path)
            .map_err(|error| format!("emit native Set/Get-i32 object: {error}"))
    })();
    match previous_optimization {
        Some(value) => std::env::set_var("MIMI_OPT", value),
        None => std::env::remove_var("MIMI_OPT"),
    }
    match previous_dump_module {
        Some(value) => std::env::set_var("MIMI_DUMP_MODULE", value),
        None => std::env::remove_var("MIMI_DUMP_MODULE"),
    }
    result
}

fn verify_default_and_explicit_cli_routes(
    attempt_root: &Path,
    command_root: &Path,
    fixture_path: &Path,
    mir_digest: &str,
    receipt_identity_digest: &str,
) -> Result<(), String> {
    let mimi_binary = PathBuf::from(env!("CARGO_BIN_EXE_mimi"));
    for (mode, explicit) in [("default", false), ("explicit-mir", true)] {
        let binary_path = attempt_root
            .join(if explicit {
                "explicit-build"
            } else {
                "default-build"
            })
            .join("program");
        let mut arguments = vec![
            OsString::from("build"),
            fixture_path.as_os_str().to_owned(),
            OsString::from("--output"),
            binary_path.as_os_str().to_owned(),
        ];
        if explicit {
            arguments.push(OsString::from("--mir"));
        }
        write_text(
            &command_root.join(format!("cli-build-{mode}.environment.txt")),
            "MIMI_ROUTE_CENSUS=1\n",
        )?;
        let output = run_command(
            &format!("cli-build-{mode}"),
            &mimi_binary,
            &arguments,
            command_root,
            Path::new(env!("CARGO_MANIFEST_DIR")),
            Some(("MIMI_ROUTE_CENSUS", "1")),
        )?;
        if !output.status.success() {
            return Err(format!(
                "CLI {mode} native build failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if !binary_path.is_file() {
            return Err(format!("CLI {mode} build produced no binary"));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mode_marker = format!("MIMI_ROUTE_CENSUS route-admission consumer=build mode={mode} ");
        let route_lines = stderr
            .lines()
            .filter(|line| line.contains(&mode_marker))
            .collect::<Vec<_>>();
        if route_lines.len() != 1 {
            return Err(format!(
                "CLI {mode} build did not emit exactly one route admission record: {stderr}"
            ));
        }
        let route_line = route_lines[0];
        for expected in [
            format!("build_route_profile={PROFILE}"),
            format!("mir_digest={mir_digest}"),
            format!("mir_route_identity_digest={receipt_identity_digest}"),
            format!("receipt_identity_digest={receipt_identity_digest}"),
        ] {
            if !route_line.contains(&expected) {
                return Err(format!(
                    "CLI {mode} route record lacks {expected:?}: {route_line}"
                ));
            }
        }
    }
    Ok(())
}

fn compile_runtime_archive(
    runtime_source: &Path,
    archive_path: &Path,
    repo_root: &Path,
    command_root: &Path,
) -> Result<(), String> {
    let arguments = vec![
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
        archive_path.as_os_str().to_owned(),
        runtime_source.as_os_str().to_owned(),
    ];
    require_success(
        "feature-gated runtime archive build",
        run_command(
            "rustc-runtime-archive",
            Path::new("rustc"),
            &arguments,
            command_root,
            repo_root,
            None,
        )?,
    )
}

fn compile_c_driver(
    source_path: &Path,
    object_path: &Path,
    repo_root: &Path,
    command_root: &Path,
) -> Result<(), String> {
    let arguments = vec![
        OsString::from("-std=c11"),
        OsString::from("-Wall"),
        OsString::from("-Wextra"),
        OsString::from("-Werror"),
        OsString::from("-c"),
        source_path.as_os_str().to_owned(),
        OsString::from("-o"),
        object_path.as_os_str().to_owned(),
    ];
    require_success(
        "C runtime-probe driver compile",
        run_command(
            "cc-driver",
            Path::new("cc"),
            &arguments,
            command_root,
            repo_root,
            None,
        )?,
    )
}

fn link_native_executable(
    generated_object: &Path,
    driver_object: &Path,
    runtime_archive: &Path,
    binary_path: &Path,
    repo_root: &Path,
    command_root: &Path,
) -> Result<(), String> {
    let arguments = vec![
        OsString::from("-no-pie"),
        generated_object.as_os_str().to_owned(),
        driver_object.as_os_str().to_owned(),
        runtime_archive.as_os_str().to_owned(),
        OsString::from("-o"),
        binary_path.as_os_str().to_owned(),
        OsString::from("-lpthread"),
        OsString::from("-ldl"),
        OsString::from("-lm"),
    ];
    require_success(
        "native Set/Get-i32 executable link",
        run_command(
            "cc-link-native",
            Path::new("cc"),
            &arguments,
            command_root,
            repo_root,
            None,
        )?,
    )
}

fn run_strict_memcheck(
    binary_path: &Path,
    evidence_directory: &Path,
    repo_root: &Path,
    command_root: &Path,
) -> Result<(), String> {
    let log_path = evidence_directory.join("valgrind.log");
    let arguments = vec![
        OsString::from("--tool=memcheck"),
        OsString::from("--leak-check=full"),
        OsString::from("--show-leak-kinds=all"),
        OsString::from("--errors-for-leak-kinds=definite,indirect,possible"),
        OsString::from("--error-exitcode=91"),
        OsString::from("--track-origins=yes"),
        OsString::from(format!("--log-file={}", log_path.display())),
        binary_path.as_os_str().to_owned(),
    ];
    let output = run_command(
        "valgrind-native",
        Path::new("valgrind"),
        &arguments,
        command_root,
        repo_root,
        None,
    )?;
    write_bytes(&evidence_directory.join("stdout.log"), &output.stdout)?;
    write_bytes(&evidence_directory.join("stderr.log"), &output.stderr)?;
    write_text(
        &evidence_directory.join("exit-status.txt"),
        &format_exit_status(output.status),
    )?;
    let report = fs::read_to_string(&log_path)
        .map_err(|error| format!("read Valgrind report {}: {error}", log_path.display()))?;
    let summary = memcheck_summary::parse(&report);
    write_text(
        &evidence_directory.join("memcheck-summary.txt"),
        &summary.rendered,
    )?;
    if !output.status.success() {
        return Err(format!(
            "strict native Memcheck failed: stdout={} stderr={} report={report}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let expected_stdout = concat!(
        "native-map-set-get-i32 generated_status=42 set_calls=1 set_status=0 ",
        "set_roots_after=2 set_anyowners_after=1 missing_status=1 missing_output=0 ",
        "mismatch_status=2 mismatch_output=0 final_roots=0 final_map_handles=0 ",
        "final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 ",
        "final_any_owner_tokens=0 final_lists=0\n"
    );
    if output.stdout.as_slice() != expected_stdout.as_bytes() {
        return Err(format!(
            "native L3 output differs\nexpected: {expected_stdout:?}\nactual: {:?}",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok(())
}

fn driver_source() -> String {
    let mut selection = Vec::with_capacity(87);
    selection.extend_from_slice(&1u32.to_be_bytes());
    selection.extend_from_slice(&1u32.to_be_bytes());
    selection.extend_from_slice(&(MISMATCH_TYPE_ID.len() as u64).to_be_bytes());
    selection.extend_from_slice(MISMATCH_TYPE_ID);
    selection.extend_from_slice(&1i32.to_be_bytes());
    selection.extend_from_slice(&MISMATCH_DESCRIPTOR_HASH);
    assert_eq!(selection.len(), 87);

    let mut hasher = blake3::Hasher::new();
    hasher.update(REGISTRY_HASH_DOMAIN);
    hasher.update(&selection[8..]);
    let selection_hash = hasher.finalize();
    DRIVER_SOURCE
        .replace("@MISMATCH_TYPE_ID@", &c_bytes(MISMATCH_TYPE_ID))
        .replace("@MISMATCH_HASH@", &c_bytes(&MISMATCH_DESCRIPTOR_HASH))
        .replace("@MISMATCH_SELECTION@", &c_bytes(&selection))
        .replace(
            "@MISMATCH_SELECTION_HASH@",
            &c_bytes(selection_hash.as_bytes()),
        )
}

fn c_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("0x{byte:02x}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn run_command(
    name: &str,
    program: &Path,
    arguments: &[OsString],
    command_root: &Path,
    working_directory: &Path,
    environment: Option<(&str, &str)>,
) -> Result<Output, String> {
    let argv_path = command_root.join(format!("{name}.argv.txt"));
    let rendered = std::iter::once(program.to_string_lossy().into_owned())
        .chain(
            arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned()),
        )
        .map(|argument| shell_quote(&argument))
        .collect::<Vec<_>>()
        .join(" ");
    write_text(&argv_path, &format!("{rendered}\n"))?;
    let mut command = Command::new(program);
    command.args(arguments).current_dir(working_directory);
    if let Some((name, value)) = environment {
        command.env(name, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("run {name} command `{rendered}`: {error}"))?;
    write_bytes(
        &command_root.join(format!("{name}.stdout.log")),
        &output.stdout,
    )?;
    write_bytes(
        &command_root.join(format!("{name}.stderr.log")),
        &output.stderr,
    )?;
    write_text(
        &command_root.join(format!("{name}.exit-status.txt")),
        &format_exit_status(output.status),
    )?;
    Ok(output)
}

fn require_success(label: &str, output: Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label} failed with {:?}: stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn source_snapshot(repo_root: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect_files(&repo_root.join("src"), &mut files)?;
    files.extend([
        repo_root.join("Cargo.toml"),
        repo_root.join("Cargo.lock"),
        repo_root.join("build.rs"),
        repo_root.join("std/prelude.mimi"),
        repo_root.join("tests/ordinary_map_set_get_i32_l3.rs"),
        repo_root.join("tests/support/memcheck_summary.rs"),
        repo_root.join("tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_i32_lineage.mimi"),
        repo_root.join("scripts/verify-ordinary-map-set-get-i32-l3.sh"),
    ]);
    files.sort();
    files.dedup();
    for path in &files {
        if !path.is_file() {
            return Err(format!("source input is missing: {}", path.display()));
        }
    }
    let output = Command::new("sha256sum")
        .args(&files)
        .output()
        .map_err(|error| format!("hash source manifest: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "source manifest sha256sum failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn write_attempt_manifest(attempt_root: &Path, evidence_root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_files(attempt_root, &mut files)?;
    files.sort();
    let command_record = attempt_root.join("commands/attempt-files-sha256.argv.txt");
    write_text(
        &command_record,
        "sha256sum over every regular file in attempt-1, including logs, raw Memcheck report, source snapshots, objects, archive, shim, and linked ELF\n",
    )?;
    files.push(command_record);
    files.sort();
    let output = Command::new("sha256sum")
        .args(&files)
        .output()
        .map_err(|error| format!("hash attempt artifacts: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "attempt artifact sha256sum failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    write_bytes(
        &evidence_root.join("attempt-1-files.sha256"),
        &output.stdout,
    )
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("read directory {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn rename_function(function: &inkwell::values::FunctionValue<'_>, name: &str) {
    use inkwell::llvm_sys::core::LLVMSetValueName2;
    use inkwell::values::AsValueRef;

    // SAFETY: the function value is live in the current LLVM module, and the
    // LLVM API copies the supplied name bytes before returning.
    unsafe {
        LLVMSetValueName2(function.as_value_ref(), name.as_ptr().cast(), name.len());
    }
}

fn write_text(path: &Path, value: &str) -> Result<(), String> {
    fs::write(path, value).map_err(|error| format!("write {}: {error}", path.display()))
}

fn write_bytes(path: &Path, value: &[u8]) -> Result<(), String> {
    fs::write(path, value).map_err(|error| format!("write {}: {error}", path.display()))
}

fn format_exit_status(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit-code={code}\nsuccess={}\n", status.success()),
        None => format!(
            "exit-code=signal\nsuccess={}\nstatus={status:?}\n",
            status.success()
        ),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
