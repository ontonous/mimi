//! Standalone strict Memcheck gate for the exact New/Set/GetAny-i32 route.
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
    materialize_ordinary_map_set_get_any_i32_route,
    ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE,
};
use mimi::lexer::Lexer;

#[path = "support/memcheck_summary.rs"]
mod memcheck_summary;

const EVIDENCE_ENV: &str = "MIMI_ORDINARY_MAP_SET_GET_ANY_I32_L3_EVIDENCE";
const PROFILE: &str = ORDINARY_MAP_OWNED_ANY_V2_SET_GET_ANY_I32_EXECUTION_PROFILE;
const RUNTIME_FEATURE: &str = "ordinary-map-set-result-l3";
const GENERATED_HIT_ENTRY: &str = "mimi_l3_generated_main_hit";
const GENERATED_MISS_ENTRY: &str = "mimi_l3_generated_main_miss";
const FIXTURE: &str =
    include_str!("fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi");

// The existing runtime hook symbols retain their Result-route suffix, but
// they sample the shared owner-v2 New, Set-i32, and GetAny C ABI functions
// exercised by both the tuple-valued and Result-valued source routes.

const DRIVER_SOURCE: &str = r#"
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

struct mimi_map_get_any_result_i32_boundary {
    uint64_t calls;
    int32_t status;
    int32_t output_zero;
    int32_t source_before_status;
    int32_t source_before_size;
    int32_t source_after_status;
    int32_t source_after_size;
    int32_t output_found;
    int32_t output_owner_zero;
    uint64_t roots;
    uint64_t ordinary_map_handles;
    uint64_t owned_get_handles;
    uint64_t runtime_map_handles;
    uint64_t managed_any;
    uint64_t owned_strings;
    uint64_t any_owner_tokens;
    uint64_t lists;
    uint64_t root_publish_attempts;
    uint64_t get_publish_attempts;
};

struct mimi_map_get_any_result_i32_observation {
    struct mimi_map_get_any_result_i32_boundary new_boundary;
    struct mimi_map_get_any_result_i32_boundary set_boundary;
    struct mimi_map_get_any_result_i32_boundary get_boundary;
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
_Static_assert(sizeof(struct mimi_map_get_any_result_i32_boundary) == 120,
               "GetAny boundary ABI changed");
_Static_assert(sizeof(struct mimi_map_get_any_result_i32_observation) == 432,
               "Rust/C L3 observation ABI changed");

static const uint8_t canonical_i32_type_id[] = { @I32_TYPE_ID@ };
static const uint8_t canonical_i32_descriptor_hash[32] = { @I32_HASH@ };

extern int32_t mimi_l3_generated_main_hit(void);
extern int32_t mimi_l3_generated_main_miss(void);
extern int32_t mimi_ordinary_map_set_get_any_result_i32_l3_reset(void);
extern int32_t mimi_ordinary_map_set_get_any_result_i32_l3_install_get_capacity_failure(void);
extern int32_t mimi_ordinary_map_set_get_any_result_i32_l3_read_observation(
    struct mimi_map_get_any_result_i32_observation *out_observation);
extern void mimi_mir_ordinary_map_domain_register_shutdown(void);
extern int32_t mimi_ordinary_map_owned_any_v2_new(int64_t *out_root);
extern int32_t mimi_ordinary_map_owned_any_v2_set_i32_result(
    int64_t handle, const uint8_t *key, int64_t key_len,
    int32_t value, int64_t *out_root);
extern int32_t mimi_ordinary_map_owned_any_v2_size(int64_t handle, int32_t *out_size);
extern int32_t mimi_ordinary_map_owned_any_v2_drop(int64_t handle);
extern int32_t mimi_ordinary_map_owned_any_v2_get(
    int64_t handle, const uint8_t *key, int64_t key_len,
    int32_t *out_found, int64_t *out_token, int32_t miss_tag,
    const uint8_t *miss_type_id, int64_t miss_type_id_len,
    const uint8_t *miss_descriptor_hash, int64_t miss_size, int64_t miss_align,
    int32_t (*miss_clone_fn)(uint8_t *destination, const uint8_t *source),
    void (*miss_drop_fn)(uint8_t *payload));

static int32_t clone_i32(uint8_t *destination, const uint8_t *source) {
    memcpy(destination, source, sizeof(int32_t));
    return 0;
}

static void drop_i32(uint8_t *payload) {
    (void)payload;
}

static int final_domains_are_zero(
    const struct mimi_map_get_any_result_i32_observation *observation) {
    return observation->final_roots == 0 &&
           observation->final_ordinary_map_handles == 0 &&
           observation->final_owned_get_handles == 0 &&
           observation->final_runtime_map_handles == 0 &&
           observation->final_managed_any == 0 &&
           observation->final_owned_strings == 0 &&
           observation->final_any_owner_tokens == 0 &&
           observation->final_lists == 0;
}

static int boundary_counts_are(
    const struct mimi_map_get_any_result_i32_boundary *boundary,
    uint64_t roots, uint64_t map_handles, uint64_t runtime_maps,
    uint64_t managed_any, uint64_t owned_strings, uint64_t any_owner_tokens,
    uint64_t root_publish_attempts, uint64_t get_publish_attempts) {
    return boundary->roots == roots &&
           boundary->ordinary_map_handles == map_handles &&
           boundary->owned_get_handles == 0 &&
           boundary->runtime_map_handles == runtime_maps &&
           boundary->managed_any == managed_any &&
           boundary->owned_strings == owned_strings &&
           boundary->any_owner_tokens == any_owner_tokens &&
           boundary->lists == 0 &&
           boundary->root_publish_attempts == root_publish_attempts &&
           boundary->get_publish_attempts == get_publish_attempts;
}

int main(int argc, char **argv) {
    if (argc != 2 || (strcmp(argv[1], "hit") != 0 &&
                      strcmp(argv[1], "miss") != 0 &&
                      strcmp(argv[1], "capacity") != 0)) {
        fputs("expected one case: hit, miss, or capacity\n", stderr);
        return 89;
    }
    const int is_miss = strcmp(argv[1], "miss") == 0;
    const int is_capacity = strcmp(argv[1], "capacity") == 0;
    if (is_capacity) {
        // This case calls the raw production ABI directly instead of entering
        // a generated MIR `main`, so it registers the same process cleanup
        // hook before it creates runtime-owned handles.
        mimi_mir_ordinary_map_domain_register_shutdown();
        if (mimi_ordinary_map_set_get_any_result_i32_l3_install_get_capacity_failure() != 0) {
            fputs("GetAny L3 capacity failpoint installation failed\n", stderr);
            return 90;
        }
    } else if (mimi_ordinary_map_set_get_any_result_i32_l3_reset() != 0) {
        fputs("GetAny L3 observer reset failed\n", stderr);
        return 90;
    }

    int32_t result_status;
    if (!is_capacity) {
        result_status = is_miss
            ? mimi_l3_generated_main_miss()
            : mimi_l3_generated_main_hit();
    } else {
        int64_t source_root = 0;
        int64_t updated_root = 0;
        const int32_t new_status = mimi_ordinary_map_owned_any_v2_new(&source_root);
        const int32_t set_status = new_status == 0
            ? mimi_ordinary_map_owned_any_v2_set_i32_result(
                  source_root, (const uint8_t *)"answer", 6, 42, &updated_root)
            : -1;
        if (new_status != 0 || source_root == 0 || set_status != 0 ||
            updated_root == 0 ||
            mimi_ordinary_map_owned_any_v2_drop(source_root) != 0) {
            fputs("GetAny L3 capacity fixture setup failed\n", stderr);
            return 91;
        }

        int32_t found = INT32_MIN;
        int64_t owner = INT64_MIN;
        result_status = mimi_ordinary_map_owned_any_v2_get(
            updated_root, (const uint8_t *)"answer", 6, &found, &owner,
            1, canonical_i32_type_id, (int64_t)sizeof(canonical_i32_type_id),
            canonical_i32_descriptor_hash, 4, 4, clone_i32, drop_i32);
        int32_t size = -1;
        if (result_status != 3 || found != 0 || owner != 0 ||
            mimi_ordinary_map_owned_any_v2_size(updated_root, &size) != 0 ||
            size != 1 || mimi_ordinary_map_owned_any_v2_drop(updated_root) != 0) {
            fputs("GetAny L3 capacity source-retention or output invariant failed\n", stderr);
            return 92;
        }
    }

    struct mimi_map_get_any_result_i32_observation observation = {0};
    if (mimi_ordinary_map_set_get_any_result_i32_l3_read_observation(&observation) != 0) {
        fputs("GetAny L3 observation read failed\n", stderr);
        return 93;
    }

    const int32_t expected_result = is_capacity ? 3 : (is_miss ? 0 : 42);
    const int32_t expected_get_status = is_capacity ? 3 : 0;
    const int32_t expected_found = is_miss || is_capacity ? 0 : 1;
    const int32_t expected_owner_zero = is_capacity ? 1 : 0;
    const uint64_t expected_get_managed_any = is_capacity ? 0 : 1;
    const uint64_t expected_get_any_owner_tokens = is_capacity ? 1 : 2;
    const uint64_t expected_get_publish_attempts = is_capacity ? 1 : 0;

    const struct mimi_map_get_any_result_i32_boundary *new_boundary =
        &observation.new_boundary;
    const struct mimi_map_get_any_result_i32_boundary *set_boundary =
        &observation.set_boundary;
    const struct mimi_map_get_any_result_i32_boundary *get_boundary =
        &observation.get_boundary;
    const int boundaries_match =
        new_boundary->calls == 1 && new_boundary->status == 0 &&
        new_boundary->output_zero == 0 &&
        new_boundary->source_before_status == INT32_MIN &&
        new_boundary->source_before_size == INT32_MIN &&
        new_boundary->source_after_status == INT32_MIN &&
        new_boundary->source_after_size == INT32_MIN &&
        new_boundary->output_found == INT32_MIN && new_boundary->output_owner_zero == -1 &&
        boundary_counts_are(new_boundary, 1, 1, 1, 0, 0, 0, 1, 0) &&
        set_boundary->calls == 1 && set_boundary->status == 0 &&
        set_boundary->output_zero == 0 &&
        set_boundary->source_before_status == 0 && set_boundary->source_before_size == 0 &&
        set_boundary->source_after_status == 0 && set_boundary->source_after_size == 0 &&
        set_boundary->output_found == INT32_MIN && set_boundary->output_owner_zero == -1 &&
        boundary_counts_are(set_boundary, 2, 2, 2, 0, is_capacity ? 0 : 1, 1, 2, 0) &&
        get_boundary->calls == 1 && get_boundary->status == expected_get_status &&
        get_boundary->output_zero == (is_capacity ? 1 : 0) &&
        get_boundary->source_before_status == 0 && get_boundary->source_before_size == 1 &&
        get_boundary->source_after_status == 0 && get_boundary->source_after_size == 1 &&
        get_boundary->output_found == expected_found &&
        get_boundary->output_owner_zero == expected_owner_zero &&
        boundary_counts_are(get_boundary, 1, 1, 1, expected_get_managed_any,
                            is_capacity ? 0 : 1,
                            expected_get_any_owner_tokens, 2,
                            expected_get_publish_attempts);
    if (result_status != expected_result || !boundaries_match ||
        observation.shutdown_registered != 1 || !final_domains_are_zero(&observation)) {
        fprintf(stderr,
                "GetAny i32 L3 invariant failure for %s: result=%" PRId32
                " expected=%" PRId32 " new=%" PRId32 " set=%" PRId32
                " get=%" PRId32 " found=%" PRId32 " owner_zero=%" PRId32 "\n",
                argv[1], result_status, expected_result, new_boundary->status,
                set_boundary->status, get_boundary->status, get_boundary->output_found,
                get_boundary->output_owner_zero);
        fprintf(stderr,
                "boundary checks: new_calls=%" PRIu64 " set_calls=%" PRIu64
                " get_calls=%" PRIu64 " get_status=%" PRId32
                " found=%" PRId32 " owner_zero=%" PRId32
                " final_roots=%" PRIu64 " final_maps=%" PRIu64
                " final_runtime_maps=%" PRIu64 " final_any=%" PRIu64
                " final_strings=%" PRIu64 " final_tokens=%" PRIu64
                " final_lists=%" PRIu64 "\n",
                new_boundary->calls, set_boundary->calls, get_boundary->calls,
                get_boundary->status, get_boundary->output_found,
                get_boundary->output_owner_zero, observation.final_roots,
                observation.final_ordinary_map_handles,
                observation.final_runtime_map_handles, observation.final_managed_any,
                observation.final_owned_strings, observation.final_any_owner_tokens,
                observation.final_lists);
        fprintf(stderr,
                "new boundary: status=%" PRId32 " output_zero=%" PRId32
                " before=(%" PRId32 ",%" PRId32 ") after=(%" PRId32 ",%" PRId32 ")"
                " roots=%" PRIu64 " map_handles=%" PRIu64
                " owned_get=%" PRIu64 " runtime_maps=%" PRIu64
                " any=%" PRIu64 " strings=%" PRIu64 " tokens=%" PRIu64
                " lists=%" PRIu64 " root_publishes=%" PRIu64 " get_publishes=%" PRIu64 "\n",
                new_boundary->status, new_boundary->output_zero,
                new_boundary->source_before_status, new_boundary->source_before_size,
                new_boundary->source_after_status, new_boundary->source_after_size,
                new_boundary->roots, new_boundary->ordinary_map_handles,
                new_boundary->owned_get_handles, new_boundary->runtime_map_handles,
                new_boundary->managed_any, new_boundary->owned_strings,
                new_boundary->any_owner_tokens, new_boundary->lists,
                new_boundary->root_publish_attempts, new_boundary->get_publish_attempts);
        fprintf(stderr,
                "set boundary: status=%" PRId32 " output_zero=%" PRId32
                " before=(%" PRId32 ",%" PRId32 ") after=(%" PRId32 ",%" PRId32 ")"
                " roots=%" PRIu64 " map_handles=%" PRIu64
                " owned_get=%" PRIu64 " runtime_maps=%" PRIu64
                " any=%" PRIu64 " strings=%" PRIu64 " tokens=%" PRIu64
                " lists=%" PRIu64 " root_publishes=%" PRIu64 " get_publishes=%" PRIu64 "\n",
                set_boundary->status, set_boundary->output_zero,
                set_boundary->source_before_status, set_boundary->source_before_size,
                set_boundary->source_after_status, set_boundary->source_after_size,
                set_boundary->roots, set_boundary->ordinary_map_handles,
                set_boundary->owned_get_handles, set_boundary->runtime_map_handles,
                set_boundary->managed_any, set_boundary->owned_strings,
                set_boundary->any_owner_tokens, set_boundary->lists,
                set_boundary->root_publish_attempts, set_boundary->get_publish_attempts);
        fprintf(stderr,
                "get boundary: status=%" PRId32 " found=%" PRId32
                " owner_zero=%" PRId32 " output_zero=%" PRId32
                " before=(%" PRId32 ",%" PRId32 ") after=(%" PRId32 ",%" PRId32 ")"
                " roots=%" PRIu64 " map_handles=%" PRIu64
                " owned_get=%" PRIu64 " runtime_maps=%" PRIu64
                " any=%" PRIu64 " strings=%" PRIu64 " tokens=%" PRIu64
                " lists=%" PRIu64 " root_publishes=%" PRIu64 " get_publishes=%" PRIu64 "\n",
                get_boundary->status, get_boundary->output_found,
                get_boundary->output_owner_zero, get_boundary->output_zero,
                get_boundary->source_before_status, get_boundary->source_before_size,
                get_boundary->source_after_status, get_boundary->source_after_size,
                get_boundary->roots, get_boundary->ordinary_map_handles,
                get_boundary->owned_get_handles, get_boundary->runtime_map_handles,
                get_boundary->managed_any, get_boundary->owned_strings,
                get_boundary->any_owner_tokens, get_boundary->lists,
                get_boundary->root_publish_attempts, get_boundary->get_publish_attempts);
        fprintf(stderr,
                "observation: shutdown_registered=%" PRId32
                " final_roots=%" PRIu64 " final_map_handles=%" PRIu64
                " final_owned_get=%" PRIu64 " final_runtime_maps=%" PRIu64
                " final_any=%" PRIu64 " final_strings=%" PRIu64
                " final_tokens=%" PRIu64 " final_lists=%" PRIu64 "\n",
                observation.shutdown_registered, observation.final_roots,
                observation.final_ordinary_map_handles, observation.final_owned_get_handles,
                observation.final_runtime_map_handles, observation.final_managed_any,
                observation.final_owned_strings, observation.final_any_owner_tokens,
                observation.final_lists);
        return 94;
    }

    printf("native-map-set-get-any-i32 mode=%s result_status=%" PRId32
           " new_calls=%" PRIu64 " new_status=%" PRId32
           " new_roots=%" PRIu64 " new_map_handles=%" PRIu64
           " new_owned_get_handles=%" PRIu64 " new_runtime_map_handles=%" PRIu64
           " new_managed_any=%" PRIu64 " new_owned_strings=%" PRIu64
           " new_any_owner_tokens=%" PRIu64 " new_lists=%" PRIu64
           " new_root_publish_attempts=%" PRIu64 " new_get_publish_attempts=%" PRIu64
           " set_calls=%" PRIu64 " set_status=%" PRId32
           " set_roots=%" PRIu64 " set_map_handles=%" PRIu64
           " set_owned_get_handles=%" PRIu64 " set_runtime_map_handles=%" PRIu64
           " set_managed_any=%" PRIu64 " set_owned_strings=%" PRIu64
           " set_any_owner_tokens=%" PRIu64 " set_lists=%" PRIu64
           " set_root_publish_attempts=%" PRIu64 " set_get_publish_attempts=%" PRIu64
           " get_calls=%" PRIu64 " get_status=%" PRId32
           " found=%" PRId32 " owner_zero=%" PRId32
           " get_roots=%" PRIu64 " get_map_handles=%" PRIu64
           " get_owned_get_handles=%" PRIu64 " get_runtime_map_handles=%" PRIu64
           " get_managed_any=%" PRIu64 " get_owned_strings=%" PRIu64
           " get_any_owner_tokens=%" PRIu64 " get_lists=%" PRIu64
           " get_root_publish_attempts=%" PRIu64 " get_publish_attempts=%" PRIu64
           " shutdown_registered=%" PRId32 " final_roots=%" PRIu64
           " final_map_handles=%" PRIu64 " final_owned_get_handles=%" PRIu64
           " final_runtime_map_handles=%" PRIu64 " final_managed_any=%" PRIu64
           " final_owned_strings=%" PRIu64 " final_any_owner_tokens=%" PRIu64
           " final_lists=%" PRIu64 "\n",
           argv[1], result_status, new_boundary->calls, new_boundary->status,
           new_boundary->roots, new_boundary->ordinary_map_handles,
           new_boundary->owned_get_handles, new_boundary->runtime_map_handles,
           new_boundary->managed_any, new_boundary->owned_strings,
           new_boundary->any_owner_tokens, new_boundary->lists,
           new_boundary->root_publish_attempts, new_boundary->get_publish_attempts,
           set_boundary->calls, set_boundary->status, set_boundary->roots,
           set_boundary->ordinary_map_handles, set_boundary->owned_get_handles,
           set_boundary->runtime_map_handles, set_boundary->managed_any,
           set_boundary->owned_strings, set_boundary->any_owner_tokens,
           set_boundary->lists, set_boundary->root_publish_attempts,
           set_boundary->get_publish_attempts,
           get_boundary->calls, get_boundary->status, get_boundary->output_found,
           get_boundary->output_owner_zero, get_boundary->roots,
           get_boundary->ordinary_map_handles, get_boundary->owned_get_handles,
           get_boundary->runtime_map_handles, get_boundary->managed_any,
           get_boundary->owned_strings, get_boundary->any_owner_tokens,
           get_boundary->lists, get_boundary->root_publish_attempts,
           get_boundary->get_publish_attempts, observation.shutdown_registered,
           observation.final_roots, observation.final_ordinary_map_handles,
           observation.final_owned_get_handles, observation.final_runtime_map_handles,
           observation.final_managed_any, observation.final_owned_strings,
           observation.final_any_owner_tokens, observation.final_lists);
    return 0;
}
"#;

fn main() {
    if let Err(error) = run_parent() {
        eprintln!("ordinary-map-set-get-any-i32-l3: {error}");
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
    let canonical_repo = fs::canonicalize(&repo_root)
        .map_err(|error| format!("canonicalize repository root: {error}"))?;
    let canonical_evidence = fs::canonicalize(&evidence_root)
        .map_err(|error| format!("canonicalize evidence directory: {error}"))?;
    if !evidence_root.is_absolute() || canonical_evidence.starts_with(&canonical_repo) {
        return Err(format!(
            "{EVIDENCE_ENV} must name an absolute directory outside the repository: {}",
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
    for case in ["hit", "miss", "capacity"] {
        fs::create_dir(attempt_root.join("memcheck").join(case))
            .map_err(|error| format!("create {case} Memcheck directory: {error}"))?;
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
        "ordinary-map-set-get-any-i32-l3 completed; strict Memcheck evidence: {}",
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
    let fixture_path = attempt_root.join("shared-set-get-any-i32-fixture.mimi");
    let shared_fixture_path =
        repo_root.join("tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi");
    let current_fixture = fs::read_to_string(&shared_fixture_path).map_err(|error| {
        format!(
            "read exact shared GetAny-i32 fixture {}: {error}",
            shared_fixture_path.display()
        )
    })?;
    if current_fixture != FIXTURE {
        return Err("compile-time bytes differ from the exact shared GetAny fixture".into());
    }
    write_text(&fixture_path, FIXTURE)?;
    let hit_checked = check_fixture(FIXTURE, &shared_fixture_path, "hit")?;
    let hit_program = materialize_ordinary_map_set_get_any_i32_route(&hit_checked)
        .map_err(|error| format!("materialize exact hit Set/GetAny-i32 MIR route: {error}"))?;
    let hit_receipt = hit_program.route_receipt(PROFILE);
    hit_receipt
        .validate_against_program(&hit_program)
        .map_err(|error| format!("hit GetAny execution receipt rejected: {error}"))?;
    let hit_mir_digest = hit_program.canonical_digest();
    let hit_receipt_digest = hit_receipt.route_identity_digest();
    write_route_identity(
        &attempt_root.join("native-route-identity-hit.txt"),
        "tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi",
        &hit_mir_digest,
        &hit_receipt_digest,
    )?;

    // The Checker profile accepts a static Get key that differs from Set,
    // allowing this same lineage to exercise the public GetAny miss path.
    const HIT_GET: &str = "map_get(updated, \"answer\")";
    const MISS_GET: &str = "map_get(updated, \"absent\")";
    if FIXTURE.matches(HIT_GET).count() != 1 {
        return Err("exact fixture no longer has one canonical hit GetAny call".into());
    }
    let miss_fixture = FIXTURE.replace(HIT_GET, MISS_GET);
    let miss_fixture_path = build_root.join("derived-get-any-miss-fixture.mimi");
    write_text(&miss_fixture_path, &miss_fixture)?;
    let miss_hash_output = run_command(
        "sha256-derived-miss-fixture",
        Path::new("sha256sum"),
        &[miss_fixture_path.as_os_str().to_owned()],
        command_root,
        repo_root,
        None,
    )?;
    if !miss_hash_output.status.success() {
        return Err(format!(
            "derived miss fixture sha256sum failed: {}",
            String::from_utf8_lossy(&miss_hash_output.stderr)
        ));
    }
    write_bytes(
        &attempt_root.join("derived-miss-fixture.sha256"),
        &miss_hash_output.stdout,
    )?;
    write_text(
        &attempt_root.join("derived-miss-source-note.txt"),
        "Derived from the exact hit fixture by changing only the GetAny static key from answer to absent; Set remains answer. This exercises the same Checker route and the native runtime miss branch.\n",
    )?;
    let miss_checked = check_fixture(&miss_fixture, &miss_fixture_path, "miss")?;
    let miss_program = materialize_ordinary_map_set_get_any_i32_route(&miss_checked)
        .map_err(|error| format!("materialize derived miss Set/GetAny-i32 route: {error}"))?;
    let miss_receipt = miss_program.route_receipt(PROFILE);
    miss_receipt
        .validate_against_program(&miss_program)
        .map_err(|error| format!("miss GetAny execution receipt rejected: {error}"))?;
    let miss_mir_digest = miss_program.canonical_digest();
    let miss_receipt_digest = miss_receipt.route_identity_digest();
    write_route_identity(
        &attempt_root.join("native-route-identity-miss.txt"),
        "derived from tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi",
        &miss_mir_digest,
        &miss_receipt_digest,
    )?;
    write_text(
        &attempt_root.join("capacity-probe-note.txt"),
        "The tuple-valued GetAny source route traps on nonzero runtime status. Its recoverable capacity lifecycle is therefore exercised through the same raw owner-v2 Get ABI after exact New/Set-i32 setup; the observer verifies zero outputs, retained source root, and final settlement.\n",
    )?;
    let cli_cases = [
        (
            "hit",
            shared_fixture_path.as_path(),
            hit_mir_digest.as_str(),
            hit_receipt_digest.as_str(),
            42,
        ),
        (
            "miss",
            miss_fixture_path.as_path(),
            miss_mir_digest.as_str(),
            miss_receipt_digest.as_str(),
            0,
        ),
    ];

    let hit_object = build_root.join("set-get-any-i32-hit-native.o");
    let hit_ir = build_root.join("set-get-any-i32-hit-native.ll");
    compile_checked_native_object(
        &hit_checked,
        &hit_program,
        &hit_receipt,
        &hit_object,
        &hit_ir,
        GENERATED_HIT_ENTRY,
        "hit",
    )?;
    let miss_object = build_root.join("set-get-any-i32-miss-native.o");
    let miss_ir = build_root.join("set-get-any-i32-miss-native.ll");
    compile_checked_native_object(
        &miss_checked,
        &miss_program,
        &miss_receipt,
        &miss_object,
        &miss_ir,
        GENERATED_MISS_ENTRY,
        "miss",
    )?;
    verify_default_and_explicit_cli_routes(attempt_root, command_root, &cli_cases)?;

    let runtime_archive = build_root.join("libmimi_runtime_set_get_any_i32_l3.a");
    compile_runtime_archive(
        &repo_root.join("src/runtime/standalone.rs"),
        &runtime_archive,
        repo_root,
        command_root,
    )?;
    let driver_source_path = build_root.join("set-get-any-i32-l3-driver.c");
    write_text(&driver_source_path, &driver_source(&hit_program)?)?;
    let driver_object = build_root.join("set-get-any-i32-l3-driver.o");
    compile_c_driver(&driver_source_path, &driver_object, repo_root, command_root)?;
    let native_binary = build_root.join("native-set-get-any-i32-l3");
    link_native_executable(
        &hit_object,
        &miss_object,
        &driver_object,
        &runtime_archive,
        &native_binary,
        repo_root,
        command_root,
    )?;
    run_strict_memcheck_cases(
        &native_binary,
        &attempt_root.join("memcheck"),
        repo_root,
        command_root,
    )
}

fn check_fixture(
    source: &str,
    source_path: &Path,
    label: &str,
) -> Result<mimi::core::resolved::CheckedProgram, String> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|error| format!("lex {label} GetAny fixture: {error}"))?;
    let parser = mimi::loader::parser_for_path(tokens, source_path)
        .map_err(|error| format!("register {label} GetAny-Result source identity: {error}"))?;
    let mut file = parser
        .parse_file()
        .map_err(|error| format!("parse {label} GetAny fixture: {error}"))?;
    mimi::loader::merge_prelude_into(&mut file);
    if let Some(main_index) = file
        .items
        .iter()
        .position(|item| matches!(item, mimi::ast::Item::Func(function) if function.name == "main"))
    {
        let main_item = file.items.remove(main_index);
        file.items.push(main_item);
    }
    mimi::core::check_program(&file)
        .map_err(|errors| format!("Checker rejected {label} GetAny fixture: {errors:?}"))
}

fn write_route_identity(
    path: &Path,
    fixture: &str,
    mir_digest: &str,
    receipt_identity_digest: &str,
) -> Result<(), String> {
    write_text(
        path,
        &format!(
            "route_profile={PROFILE}\nmir_digest={mir_digest}\nroute_identity_digest={receipt_identity_digest}\nsource_fixture={fixture}\n"
        ),
    )
}

fn compile_checked_native_object(
    checked: &mimi::core::resolved::CheckedProgram,
    program: &mimi::core::mir::reference::MirProgram,
    receipt: &mimi::core::mir::CanonicalMirRouteReceipt,
    object_path: &Path,
    ir_path: &Path,
    generated_entry: &str,
    label: &str,
) -> Result<(), String> {
    let previous_optimization = std::env::var_os("MIMI_OPT");
    let previous_dump_module = std::env::var_os("MIMI_DUMP_MODULE");
    std::env::set_var("MIMI_OPT", "0");
    std::env::remove_var("MIMI_DUMP_MODULE");
    let result = (|| {
        let context = Context::create();
        let mut generator = CodeGenerator::new(&context, "native_map_set_get_any_i32_l3_artifact");
        generator
            .compile_checked_mir_native_with_ordinary_map_owned_any_v2_set_get_any_i32_route(
                checked, program, receipt,
            )
            .map_err(|errors| {
                format!("exact native Set/GetAny-i32 {label} route emission failed: {errors:?}")
            })?;
        generator
            .module
            .verify()
            .map_err(|error| format!("verify exact native Set/GetAny-i32 module: {error}"))?;
        let native_ir = generator.module.print_to_string().to_string();
        if !native_ir.contains("mimi_ordinary_map_owned_any_v2_get(") {
            return Err(format!(
                "native {label} GetAny route did not call the raw owner-v2 GetAny status/out ABI"
            ));
        }
        let main = generator
            .module
            .get_function("main")
            .ok_or_else(|| "exact native Set/GetAny-i32 module lacks main".to_owned())?;
        if main.count_params() != 0
            || main.get_type().get_return_type() != Some(context.i32_type().into())
        {
            return Err("exact native Set/GetAny-i32 main ABI is not () -> i32".into());
        }
        rename_function(&main, generated_entry);
        generator
            .module
            .verify()
            .map_err(|error| format!("verify renamed native GetAny module: {error}"))?;
        generator
            .module
            .print_to_file(ir_path)
            .map_err(|error| format!("write native GetAny LLVM IR: {error}"))?;
        generator
            .compile_to_object(object_path)
            .map_err(|error| format!("emit native GetAny object: {error}"))
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
    cases: &[(&str, &Path, &str, &str, i32)],
) -> Result<(), String> {
    let mimi_binary = PathBuf::from(env!("CARGO_BIN_EXE_mimi"));
    for (mode, explicit) in [("default", false), ("explicit-mir", true)] {
        for (label, fixture_path, mir_digest, receipt_identity_digest, expected_status) in cases {
            let build_directory = attempt_root
                .join(if explicit {
                    "explicit-build"
                } else {
                    "default-build"
                })
                .join(label);
            fs::create_dir_all(&build_directory)
                .map_err(|error| format!("create CLI {mode} {label} build directory: {error}"))?;
            let binary_path = build_directory.join("program");
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
                &command_root.join(format!("cli-build-{label}-{mode}.environment.txt")),
                "MIMI_ROUTE_CENSUS=1\n",
            )?;
            let output = run_command(
                &format!("cli-build-{label}-{mode}"),
                &mimi_binary,
                &arguments,
                command_root,
                Path::new(env!("CARGO_MANIFEST_DIR")),
                Some(("MIMI_ROUTE_CENSUS", "1")),
            )?;
            if !output.status.success() {
                return Err(format!(
                    "CLI {mode} {label} native build failed: stdout={} stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            if !binary_path.is_file() {
                return Err(format!("CLI {mode} {label} build produced no binary"));
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mode_marker =
                format!("MIMI_ROUTE_CENSUS route-admission consumer=build mode={mode} ");
            let route_lines = stderr
                .lines()
                .filter(|line| line.contains(&mode_marker))
                .collect::<Vec<_>>();
            if route_lines.len() != 1 {
                return Err(format!(
                    "CLI {mode} {label} build did not emit exactly one route admission record: {stderr}"
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
                        "CLI {mode} {label} route record lacks {expected:?}: {route_line}"
                    ));
                }
            }
            let native = run_command(
                &format!("cli-native-{label}-{mode}"),
                &binary_path,
                &[],
                command_root,
                Path::new(env!("CARGO_MANIFEST_DIR")),
                None,
            )?;
            if native.status.code() != Some(*expected_status) {
                return Err(format!(
                    "CLI {mode} {label} native artifact returned {:?}, expected {expected_status}",
                    native.status.code()
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
    hit_generated_object: &Path,
    miss_generated_object: &Path,
    driver_object: &Path,
    runtime_archive: &Path,
    binary_path: &Path,
    repo_root: &Path,
    command_root: &Path,
) -> Result<(), String> {
    let arguments = vec![
        OsString::from("-no-pie"),
        hit_generated_object.as_os_str().to_owned(),
        miss_generated_object.as_os_str().to_owned(),
        driver_object.as_os_str().to_owned(),
        runtime_archive.as_os_str().to_owned(),
        OsString::from("-o"),
        binary_path.as_os_str().to_owned(),
        OsString::from("-lpthread"),
        OsString::from("-ldl"),
        OsString::from("-lm"),
    ];
    require_success(
        "native Set/GetAny-i32 executable link",
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

fn run_strict_memcheck_cases(
    binary_path: &Path,
    evidence_root: &Path,
    repo_root: &Path,
    command_root: &Path,
) -> Result<(), String> {
    for (case, expected_stdout) in [
        (
            "hit",
            concat!(
                "native-map-set-get-any-i32 mode=hit result_status=42 ",
                "new_calls=1 new_status=0 new_roots=1 new_map_handles=1 ",
                "new_owned_get_handles=0 new_runtime_map_handles=1 new_managed_any=0 ",
                "new_owned_strings=0 new_any_owner_tokens=0 new_lists=0 ",
                "new_root_publish_attempts=1 new_get_publish_attempts=0 ",
                "set_calls=1 set_status=0 set_roots=2 set_map_handles=2 ",
                "set_owned_get_handles=0 set_runtime_map_handles=2 set_managed_any=0 ",
                "set_owned_strings=1 set_any_owner_tokens=1 set_lists=0 ",
                "set_root_publish_attempts=2 set_get_publish_attempts=0 ",
                "get_calls=1 get_status=0 found=1 owner_zero=0 get_roots=1 ",
                "get_map_handles=1 get_owned_get_handles=0 get_runtime_map_handles=1 ",
                "get_managed_any=1 get_owned_strings=1 get_any_owner_tokens=2 get_lists=0 ",
                "get_root_publish_attempts=2 get_publish_attempts=0 shutdown_registered=1 ",
                "final_roots=0 final_map_handles=0 final_owned_get_handles=0 ",
                "final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 ",
                "final_any_owner_tokens=0 final_lists=0\n"
            ),
        ),
        (
            "miss",
            concat!(
                "native-map-set-get-any-i32 mode=miss result_status=0 ",
                "new_calls=1 new_status=0 new_roots=1 new_map_handles=1 ",
                "new_owned_get_handles=0 new_runtime_map_handles=1 new_managed_any=0 ",
                "new_owned_strings=0 new_any_owner_tokens=0 new_lists=0 ",
                "new_root_publish_attempts=1 new_get_publish_attempts=0 ",
                "set_calls=1 set_status=0 set_roots=2 set_map_handles=2 ",
                "set_owned_get_handles=0 set_runtime_map_handles=2 set_managed_any=0 ",
                "set_owned_strings=1 set_any_owner_tokens=1 set_lists=0 ",
                "set_root_publish_attempts=2 set_get_publish_attempts=0 ",
                "get_calls=1 get_status=0 found=0 owner_zero=0 get_roots=1 ",
                "get_map_handles=1 get_owned_get_handles=0 get_runtime_map_handles=1 ",
                "get_managed_any=1 get_owned_strings=1 get_any_owner_tokens=2 get_lists=0 ",
                "get_root_publish_attempts=2 get_publish_attempts=0 shutdown_registered=1 ",
                "final_roots=0 final_map_handles=0 final_owned_get_handles=0 ",
                "final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 ",
                "final_any_owner_tokens=0 final_lists=0\n"
            ),
        ),
        (
            "capacity",
            concat!(
                "native-map-set-get-any-i32 mode=capacity result_status=3 ",
                "new_calls=1 new_status=0 new_roots=1 new_map_handles=1 ",
                "new_owned_get_handles=0 new_runtime_map_handles=1 new_managed_any=0 ",
                "new_owned_strings=0 new_any_owner_tokens=0 new_lists=0 ",
                "new_root_publish_attempts=1 new_get_publish_attempts=0 ",
                "set_calls=1 set_status=0 set_roots=2 set_map_handles=2 ",
                "set_owned_get_handles=0 set_runtime_map_handles=2 set_managed_any=0 ",
                "set_owned_strings=0 set_any_owner_tokens=1 set_lists=0 ",
                "set_root_publish_attempts=2 set_get_publish_attempts=0 ",
                "get_calls=1 get_status=3 found=0 owner_zero=1 get_roots=1 ",
                "get_map_handles=1 get_owned_get_handles=0 get_runtime_map_handles=1 ",
                "get_managed_any=0 get_owned_strings=0 get_any_owner_tokens=1 get_lists=0 ",
                "get_root_publish_attempts=2 get_publish_attempts=1 shutdown_registered=1 ",
                "final_roots=0 final_map_handles=0 final_owned_get_handles=0 ",
                "final_runtime_map_handles=0 final_managed_any=0 final_owned_strings=0 ",
                "final_any_owner_tokens=0 final_lists=0\n"
            ),
        ),
    ] {
        let evidence_directory = evidence_root.join(case);
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
            OsString::from(case),
        ];
        let output = run_command(
            &format!("valgrind-native-{case}"),
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
        let report = fs::read_to_string(&log_path).map_err(|error| {
            format!(
                "read Valgrind {case} report {}: {error}",
                log_path.display()
            )
        })?;
        let summary = memcheck_summary::parse(&report);
        write_text(
            &evidence_directory.join("memcheck-summary.txt"),
            &summary.rendered,
        )?;
        if !output.status.success() {
            return Err(format!(
                "strict native Memcheck failed for {case}: stdout={} stderr={} report={report}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if output.stdout.as_slice() != expected_stdout.as_bytes() {
            return Err(format!(
                "native {case} L3 output differs\nexpected: {expected_stdout:?}\nactual: {:?}",
                String::from_utf8_lossy(&output.stdout)
            ));
        }
    }
    Ok(())
}

fn driver_source(program: &mimi::core::mir::reference::MirProgram) -> Result<String, String> {
    let (type_id, descriptor_hash) = canonical_i32_payload_identity(program)?;
    Ok(DRIVER_SOURCE
        .replace("@I32_TYPE_ID@", &c_bytes(&type_id))
        .replace("@I32_HASH@", &c_bytes(&descriptor_hash)))
}

fn canonical_i32_payload_identity(
    program: &mimi::core::mir::reference::MirProgram,
) -> Result<(Vec<u8>, [u8; 32]), String> {
    use mimi::core::mir::types::{MirLayout, MirTypeKind};

    let catalog = program.type_catalog();
    let (type_id, descriptor) = catalog
        .iter()
        .find(|(_, descriptor)| {
            descriptor.kind == MirTypeKind::Primitive(mimi::core::PrimitiveType::I32)
        })
        .ok_or_else(|| "exact GetAny-i32 route has no canonical i32 TypeDesc".to_owned())?;
    let checker_children = catalog
        .resolved_type_children(type_id)
        .ok_or_else(|| "canonical i32 TypeDesc has no sealed Checker child inventory".to_owned())?;
    if descriptor.layout != MirLayout::Scalar
        || !checker_children.is_empty()
        || descriptor.session_protocol.is_some()
        || descriptor.drop_plan.is_some()
        || descriptor.variant_drop_plan.is_some()
    {
        return Err("canonical i32 TypeDesc unexpectedly has recursive children".into());
    }

    // Mirror the runtime's closed TypeDesc-closure hash for the scalar leaf
    // used by this exact route's miss-owner ABI.
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"mimi-managed-any-payload-v2:typedesc-closure:v1\n");
    update_framed(&mut hasher, type_id.as_str().as_bytes());
    update_framed(&mut hasher, descriptor.canonical_text().as_bytes());
    update_framed(&mut hasher, b"checker");
    hasher.update(&0u64.to_be_bytes());
    update_framed(&mut hasher, b"layout");
    hasher.update(&0u64.to_be_bytes());
    update_framed(&mut hasher, b"session-protocol");
    hasher.update(&0u64.to_be_bytes());
    update_framed(&mut hasher, b"drop-plan");
    hasher.update(&0u64.to_be_bytes());
    update_framed(&mut hasher, b"variant-drop-plans");
    hasher.update(&0u64.to_be_bytes());
    Ok((
        type_id.as_str().as_bytes().to_vec(),
        *hasher.finalize().as_bytes(),
    ))
}

fn update_framed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
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
        repo_root.join("tests/ordinary_map_set_get_any_i32_l3.rs"),
        repo_root.join("scripts/verify-ordinary-map-set-get-any-i32-l3.sh"),
        repo_root.join("tests/support/memcheck_summary.rs"),
        repo_root.join("tests/fixtures/mir_ordinary_map_owned_any_v2_set_get_any_i32_lineage.mimi"),
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
        "sha256sum over every regular file in attempt-1, including source variants, route identities, command logs, raw Memcheck reports, objects, archive, native driver, and linked ELF\n",
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
